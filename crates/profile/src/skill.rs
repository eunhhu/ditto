//! Skills: documents a profile carries, discovered by their header and read on
//! demand (ADR 0046, `docs/design/platform-surface.md` P3).
//!
//! A skill is data, not code: a directory with `SKILL.md`, a one-line
//! `description` in a small frontmatter block, and a body. Installing one grants
//! no effect, which is why reading one needs no lease.

use std::path::{Path, PathBuf};

use crate::ProfileError;

/// Directory inside a profile's data root that holds skills.
pub const SKILLS_DIR: &str = "skills";

/// The file that makes a directory a skill.
pub const SKILL_FILE: &str = "SKILL.md";

/// Longest accepted skill name; the same alphabet as a profile name.
pub const MAX_SKILL_NAME_LEN: usize = 64;

/// Characters a description may hold. It is what the model sees before deciding
/// whether to read the skill at all, so it stays one line long.
pub const MAX_DESCRIPTION_CHARS: usize = 200;

/// Bytes a skill body may hold. A skill that needs more is a reference file the
/// body points at.
pub const MAX_BODY_BYTES: usize = 64 * 1024;

/// Bytes a whole `SKILL.md` may hold: the body limit plus room for the
/// frontmatter. A document is read only this far, so an oversized file costs a
/// bounded read on every turn rather than its whole size.
pub const MAX_DOCUMENT_BYTES: usize = MAX_BODY_BYTES + 4 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillError {
    /// The directory name or the frontmatter name is not usable.
    InvalidName { name: String, reason: &'static str },
    /// No `SKILL.md`, or it could not be read.
    Missing { path: String },
    /// The file exists but does not follow the format.
    Malformed { path: String, reason: &'static str },
}

impl std::fmt::Display for SkillError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidName { name, reason } => {
                write!(formatter, "skill name {name:?} is not usable: {reason}")
            }
            Self::Missing { path } => write!(formatter, "no skill document at {path}"),
            Self::Malformed { path, reason } => {
                write!(formatter, "{path} is not a skill document: {reason}")
            }
        }
    }
}

impl std::error::Error for SkillError {}

impl From<SkillError> for ProfileError {
    fn from(error: SkillError) -> Self {
        match error {
            SkillError::InvalidName { name, reason } => ProfileError::InvalidName { name, reason },
            other => ProfileError::NotFound {
                name: other.to_string(),
            },
        }
    }
}

/// A skill's header: everything the model may see before it asks for the body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillHeader {
    pub name: String,
    pub description: String,
    /// Where the body lives, for the reader.
    pub path: PathBuf,
}

/// A skill's header and body together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub header: SkillHeader,
    pub body: String,
}

/// The same alphabet as a profile name, and a dot-free string, so a skill name
/// can never leave the skills directory.
pub fn validate_name(name: &str) -> Result<(), SkillError> {
    if name.is_empty() {
        return Err(invalid(name, "it is empty"));
    }
    if name.len() > MAX_SKILL_NAME_LEN {
        return Err(invalid(name, "it is longer than 64 characters"));
    }
    let first = name.chars().next().expect("checked non-empty");
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return Err(invalid(
            name,
            "it must start with a lowercase letter or digit",
        ));
    }
    for character in name.chars() {
        let allowed = character.is_ascii_lowercase()
            || character.is_ascii_digit()
            || character == '-'
            || character == '_';
        if !allowed {
            return Err(invalid(name, "only a-z, 0-9, '-' and '_' are allowed"));
        }
    }
    Ok(())
}

fn invalid(name: &str, reason: &'static str) -> SkillError {
    SkillError::InvalidName {
        name: name.to_owned(),
        reason,
    }
}

/// Every skill in a profile's skills directory, by name. Each document is read
/// once, up to [`MAX_DOCUMENT_BYTES`].
///
/// A malformed skill is reported rather than skipped: a skill that silently does
/// not load is worse than one that says why. That includes a directory whose
/// name the format refuses (`Photo Triage`): it was meant as a skill, and
/// leaving it out would make the catalogue silently shorter. Hidden entries
/// (`.git`, an editor's folder) and plain files are not attempts at a skill and
/// are ignored.
pub fn list(root: &Path) -> Result<Vec<SkillHeader>, SkillError> {
    let directory = root.join(SKILLS_DIR);
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => {
            return Err(SkillError::Missing {
                path: directory.display().to_string(),
            });
        }
    };
    let mut skills = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        validate_name(&name)?;
        skills.push(read_document(&name, &path)?.header);
    }
    skills.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(skills)
}

/// Read one skill by name.
pub fn read(root: &Path, name: &str) -> Result<Skill, SkillError> {
    validate_name(name)?;
    read_document(name, &root.join(SKILLS_DIR).join(name))
}

/// One skill document, read once and checked against every rule of the format.
fn read_document(name: &str, directory: &Path) -> Result<Skill, SkillError> {
    let path = directory.join(SKILL_FILE);
    let text = read_bounded(&path)?;
    let (declared, description, body) = parse(&path, &text)?;
    if declared != name {
        return Err(malformed(
            &path,
            "the declared name does not match its directory",
        ));
    }
    if body.len() > MAX_BODY_BYTES {
        return Err(malformed(&path, "the body is larger than 64 KiB"));
    }
    Ok(Skill {
        header: SkillHeader {
            name: name.to_owned(),
            description,
            path,
        },
        body,
    })
}

/// The document's text, read no further than [`MAX_DOCUMENT_BYTES`]. Only a
/// regular file is opened, so a FIFO or device named `SKILL.md` cannot stall
/// the turn that reads the catalogue.
fn read_bounded(path: &Path) -> Result<String, SkillError> {
    use std::io::Read;

    let missing = || SkillError::Missing {
        path: path.display().to_string(),
    };
    let metadata = std::fs::metadata(path).map_err(|_| missing())?;
    if !metadata.is_file() {
        return Err(malformed(path, "it is not a regular file"));
    }
    let file = std::fs::File::open(path).map_err(|_| missing())?;
    let mut bytes = Vec::new();
    file.take(MAX_DOCUMENT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| missing())?;
    if bytes.len() > MAX_DOCUMENT_BYTES {
        return Err(malformed(path, "the document is larger than 68 KiB"));
    }
    String::from_utf8(bytes).map_err(|_| malformed(path, "it is not UTF-8 text"))
}

fn malformed(path: &Path, reason: &'static str) -> SkillError {
    SkillError::Malformed {
        path: path.display().to_string(),
        reason,
    }
}

/// The `name` and `description` from the frontmatter, and the body after it.
fn parse(path: &Path, text: &str) -> Result<(String, String, String), SkillError> {
    let rest = text
        .strip_prefix("---\n")
        .ok_or_else(|| malformed(path, "it does not start with a --- frontmatter block"))?;
    let Some(end) = rest.find("\n---\n") else {
        return Err(malformed(path, "the frontmatter block is not closed"));
    };
    let frontmatter = &rest[..end];
    let body = rest[end + "\n---\n".len()..].to_owned();
    let field = |key: &str| {
        frontmatter
            .lines()
            .find_map(|line| line.strip_prefix(key))
            .map(|value| value.trim().trim_matches('"').to_owned())
    };
    let name = field("name:").ok_or_else(|| malformed(path, "the frontmatter has no name"))?;
    validate_name(&name)?;
    let description = field("description:")
        .ok_or_else(|| malformed(path, "the frontmatter has no description"))?;
    if description.is_empty() || description.chars().count() > MAX_DESCRIPTION_CHARS {
        return Err(malformed(
            path,
            "the description must hold 1 through 200 characters",
        ));
    }
    Ok((name, description, body))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "ditto-skills-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).expect("scratch");
        path
    }

    fn install(root: &Path, name: &str, description: &str, body: &str) {
        let directory = root.join(SKILLS_DIR).join(name);
        std::fs::create_dir_all(&directory).expect("skill dir");
        std::fs::write(
            directory.join(SKILL_FILE),
            format!("---\nname: {name}\ndescription: {description}\n---\n{body}"),
        )
        .expect("skill file");
    }

    #[test]
    fn a_profile_lists_its_skills_by_header_and_reads_a_body_on_demand() {
        let root = scratch("list");
        install(
            &root,
            "photo-triage",
            "Sort a folder of photos by date.",
            "# Steps\n",
        );
        install(
            &root,
            "invoice-pdf",
            "Turn a PDF invoice into rows.",
            "body\n",
        );
        let headers = list(&root).expect("list");
        assert_eq!(
            headers
                .iter()
                .map(|header| header.name.as_str())
                .collect::<Vec<_>>(),
            ["invoice-pdf", "photo-triage"]
        );
        assert!(headers[1].description.starts_with("Sort a folder"));
        let skill = read(&root, "photo-triage").expect("read");
        assert_eq!(skill.body, "# Steps\n");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_document_without_frontmatter_is_reported_not_skipped() {
        let root = scratch("malformed");
        let directory = root.join(SKILLS_DIR).join("broken");
        std::fs::create_dir_all(&directory).expect("dir");
        std::fs::write(directory.join(SKILL_FILE), "# just a body\n").expect("write");
        let error = list(&root).expect_err("a malformed skill must not pass silently");
        assert!(
            error.to_string().contains("frontmatter"),
            "the reason names the format: {error}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_name_that_disagrees_with_its_directory_is_refused() {
        let root = scratch("mismatch");
        let directory = root.join(SKILLS_DIR).join("alpha");
        std::fs::create_dir_all(&directory).expect("dir");
        std::fs::write(
            directory.join(SKILL_FILE),
            "---\nname: beta\ndescription: elsewhere\n---\nbody\n",
        )
        .expect("write");
        let error = list(&root).expect_err("mismatch");
        assert!(error.to_string().contains("does not match"), "{error}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_skill_name_cannot_leave_the_skills_directory() {
        let root = scratch("traversal");
        for name in ["../etc", "a/b", ".hidden", "Skill"] {
            assert!(validate_name(name).is_err(), "{name:?} must be refused");
            assert!(read(&root, name).is_err(), "{name:?} must not resolve");
        }
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn an_overlong_description_or_body_is_refused_with_its_limit() {
        let root = scratch("limits");
        install(&root, "long-description", &"x".repeat(201), "body\n");
        let error = list(&root).expect_err("description limit");
        assert!(error.to_string().contains("200"), "{error}");
        std::fs::remove_dir_all(&root).ok();

        let root = scratch("limits-body");
        install(&root, "long-body", "fine", &"x".repeat(MAX_BODY_BYTES + 1));
        let error = list(&root).expect_err("body limit");
        assert!(error.to_string().contains("64 KiB"), "{error}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_misnamed_skill_directory_is_reported_and_hidden_entries_are_not_skills() {
        let root = scratch("misnamed");
        install(&root, "photo-triage", "Sort photos.", "body\n");
        // Not attempts at a skill: a hidden directory and a plain file.
        std::fs::create_dir_all(root.join(SKILLS_DIR).join(".git")).expect("hidden dir");
        std::fs::write(root.join(SKILLS_DIR).join("README.md"), "notes\n").expect("file");
        assert_eq!(list(&root).expect("list").len(), 1);

        // A directory meant as a skill whose name the format refuses.
        let misnamed = root.join(SKILLS_DIR).join("Photo Triage");
        std::fs::create_dir_all(&misnamed).expect("misnamed dir");
        std::fs::write(
            misnamed.join(SKILL_FILE),
            "---\nname: photo-triage\ndescription: copy\n---\nbody\n",
        )
        .expect("write");
        let error = list(&root).expect_err("a misnamed skill must not vanish");
        assert!(error.to_string().contains("Photo Triage"), "{error}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_document_is_read_no_further_than_its_bound() {
        let root = scratch("bound");
        let directory = root.join(SKILLS_DIR).join("huge");
        std::fs::create_dir_all(&directory).expect("dir");
        let mut text = String::from("---\nname: huge\ndescription: big\n---\n");
        text.push_str(&"x".repeat(MAX_DOCUMENT_BYTES));
        std::fs::write(directory.join(SKILL_FILE), text).expect("write");
        let error = list(&root).expect_err("an oversized document is refused");
        assert!(error.to_string().contains("68 KiB"), "{error}");
        let error = read(&root, "huge").expect_err("and cannot be read either");
        assert!(error.to_string().contains("68 KiB"), "{error}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_document_that_is_not_text_or_not_a_file_is_malformed() {
        let root = scratch("binary");
        let directory = root.join(SKILLS_DIR).join("binary");
        std::fs::create_dir_all(&directory).expect("dir");
        std::fs::write(directory.join(SKILL_FILE), [0xff, 0xfe, 0x00]).expect("write");
        let error = read(&root, "binary").expect_err("not UTF-8");
        assert!(error.to_string().contains("UTF-8"), "{error}");

        let directory = root.join(SKILLS_DIR).join("folder");
        std::fs::create_dir_all(directory.join(SKILL_FILE)).expect("a directory named SKILL.md");
        let error = read(&root, "folder").expect_err("not a regular file");
        assert!(error.to_string().contains("regular file"), "{error}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_profile_without_skills_has_an_empty_catalogue() {
        let root = scratch("empty");
        assert_eq!(list(&root).expect("list"), Vec::new());
        std::fs::remove_dir_all(&root).ok();
    }
}

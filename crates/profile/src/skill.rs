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

/// Every skill in a profile's skills directory, by name, without reading bodies
/// beyond their first lines.
///
/// A malformed skill is reported rather than skipped: a skill that silently does
/// not load is worse than one that says why.
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
        if validate_name(&name).is_err() {
            continue;
        }
        skills.push(read_header(&name, &path)?);
    }
    skills.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(skills)
}

/// Read one skill by name.
pub fn read(root: &Path, name: &str) -> Result<Skill, SkillError> {
    validate_name(name)?;
    let directory = root.join(SKILLS_DIR).join(name);
    let header = read_header(name, &directory)?;
    let text = std::fs::read_to_string(&header.path).map_err(|_| SkillError::Missing {
        path: header.path.display().to_string(),
    })?;
    let (_, body) = parse(&header.path, &text)?;
    Ok(Skill { header, body })
}

fn read_header(name: &str, directory: &Path) -> Result<SkillHeader, SkillError> {
    let path = directory.join(SKILL_FILE);
    let text = std::fs::read_to_string(&path).map_err(|_| SkillError::Missing {
        path: path.display().to_string(),
    })?;
    let (declared, body) = parse(&path, &text)?;
    if declared != name {
        return Err(malformed(
            &path,
            "the declared name does not match its directory",
        ));
    }
    if body.len() > MAX_BODY_BYTES {
        return Err(malformed(&path, "the body is larger than 64 KiB"));
    }
    let description = description_of(&path, &text)?;
    Ok(SkillHeader {
        name: name.to_owned(),
        description,
        path,
    })
}

fn malformed(path: &Path, reason: &'static str) -> SkillError {
    SkillError::Malformed {
        path: path.display().to_string(),
        reason,
    }
}

/// The `name` from the frontmatter, and the body after it.
fn parse(path: &Path, text: &str) -> Result<(String, String), SkillError> {
    let rest = text
        .strip_prefix("---\n")
        .ok_or_else(|| malformed(path, "it does not start with a --- frontmatter block"))?;
    let Some(end) = rest.find("\n---\n") else {
        return Err(malformed(path, "the frontmatter block is not closed"));
    };
    let frontmatter = &rest[..end];
    let body = rest[end + "\n---\n".len()..].to_owned();
    let name = frontmatter
        .lines()
        .find_map(|line| line.strip_prefix("name:"))
        .map(|value| value.trim().trim_matches('"').to_owned())
        .ok_or_else(|| malformed(path, "the frontmatter has no name"))?;
    validate_name(&name)?;
    Ok((name, body))
}

fn description_of(path: &Path, text: &str) -> Result<String, SkillError> {
    let rest = text.strip_prefix("---\n").unwrap_or_default();
    let Some(end) = rest.find("\n---\n") else {
        return Err(malformed(path, "the frontmatter block is not closed"));
    };
    let description = rest[..end]
        .lines()
        .find_map(|line| line.strip_prefix("description:"))
        .map(|value| value.trim().trim_matches('"').to_owned())
        .ok_or_else(|| malformed(path, "the frontmatter has no description"))?;
    if description.is_empty() || description.chars().count() > MAX_DESCRIPTION_CHARS {
        return Err(malformed(
            path,
            "the description must hold 1 through 200 characters",
        ));
    }
    if description.contains('\n') {
        return Err(malformed(path, "the description must be one line"));
    }
    Ok(description)
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
    fn a_profile_without_skills_has_an_empty_catalogue() {
        let root = scratch("empty");
        assert_eq!(list(&root).expect("list"), Vec::new());
        std::fs::remove_dir_all(&root).ok();
    }
}

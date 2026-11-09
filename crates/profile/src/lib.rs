//! Profile boundaries (ADR 0044).
//!
//! A profile is a directory. The daemon serves exactly one, so nothing can read
//! across profiles by forgetting a filter: the path either is this profile's data
//! root or it is not reachable at all.

use std::path::{Path, PathBuf};

pub mod skill;

/// The profile an installation has before anyone asks for a second one. Its root
/// is the data directory itself, so an existing install keeps its data.
pub const DEFAULT_PROFILE: &str = "default";

/// Named profiles live under this directory inside the data root.
pub const PROFILES_DIR: &str = "profiles";

/// Longest accepted profile name. Names reach the filesystem, so they stay short,
/// lowercase and boring.
pub const MAX_NAME_LEN: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileError {
    /// The name is not a usable directory name.
    InvalidName { name: String, reason: &'static str },
    /// The name is valid but no such profile exists.
    NotFound { name: String },
}

impl std::fmt::Display for ProfileError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidName { name, reason } => {
                write!(formatter, "profile name {name:?} is not usable: {reason}")
            }
            Self::NotFound { name } => write!(formatter, "no profile named {name:?}"),
        }
    }
}

impl std::error::Error for ProfileError {}

/// Whether `name` can be a directory under the data root.
///
/// Lowercase ASCII, digits, `-` and `_`, starting with a letter or digit, at most
/// [`MAX_NAME_LEN`] characters. No dots, so `..` and hidden names are impossible
/// by construction rather than by a check that someone must remember.
pub fn validate(name: &str) -> Result<(), ProfileError> {
    if name.is_empty() {
        return Err(invalid(name, "it is empty"));
    }
    if name.len() > MAX_NAME_LEN {
        return Err(invalid(name, "it is longer than 32 characters"));
    }
    let mut characters = name.chars();
    let first = characters.next().expect("checked non-empty");
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

fn invalid(name: &str, reason: &'static str) -> ProfileError {
    ProfileError::InvalidName {
        name: name.to_owned(),
        reason,
    }
}

/// The data root for `name`, whether or not it exists yet.
///
/// `default` resolves to `base` itself: the first profile is the installation.
pub fn root(base: &Path, name: &str) -> Result<PathBuf, ProfileError> {
    validate(name)?;
    if name == DEFAULT_PROFILE {
        return Ok(base.to_path_buf());
    }
    Ok(base.join(PROFILES_DIR).join(name))
}

/// Every profile this data root can serve: `default`, then named profiles in
/// name order.
pub fn list(base: &Path) -> Result<Vec<String>, ProfileError> {
    let mut names = vec![DEFAULT_PROFILE.to_owned()];
    let directory = base.join(PROFILES_DIR);
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(names),
        Err(_) => {
            let shown = directory.display().to_string();
            return Err(invalid(&shown, "the profiles directory is unreadable"));
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if validate(name).is_ok() {
            names.push(name.to_owned());
        }
    }
    names[1..].sort();
    Ok(names)
}

/// Create `name` and return its root.
///
/// Creating an existing profile is not an error: the caller asked for the
/// directory to be there, and it is.
pub fn create(base: &Path, name: &str) -> Result<PathBuf, ProfileError> {
    let root = root(base, name)?;
    std::fs::create_dir_all(&root)
        .map_err(|_| invalid(name, "the directory could not be created"))?;
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "ditto-profile-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).expect("scratch");
        path
    }

    #[test]
    fn the_default_profile_is_the_data_root_itself() {
        let base = Path::new("/var/lib/ditto");
        assert_eq!(root(base, DEFAULT_PROFILE).expect("valid"), base);
        assert_eq!(
            root(base, "minchae").expect("valid"),
            base.join("profiles").join("minchae")
        );
    }

    #[test]
    fn a_name_that_could_climb_out_of_the_data_root_is_refused() {
        let base = Path::new("/var/lib/ditto");
        for name in [
            "",
            ".",
            "..",
            "a/b",
            "../etc",
            "Profile",
            "선우",
            "a b",
            &"x".repeat(33),
        ] {
            assert!(
                root(base, name).is_err(),
                "{name:?} must not resolve to a path"
            );
        }
        assert!(
            root(base, "min-chae_2").is_ok(),
            "the allowed alphabet works"
        );
    }

    #[test]
    fn two_profiles_live_in_two_directories() {
        let base = scratch("two");
        let first = create(&base, "default").expect("default");
        let second = create(&base, "minchae").expect("minchae");
        assert_ne!(first, second);
        assert!(first.exists() && second.exists());
        std::fs::write(first.join("journal"), b"a memory of one person").expect("write");
        assert!(
            !second.join("journal").exists(),
            "what one profile writes the other cannot see"
        );
        assert_eq!(
            list(&base).expect("list"),
            vec!["default".to_owned(), "minchae".to_owned()]
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn listing_an_untouched_data_root_reports_only_the_default_profile() {
        let base = scratch("fresh");
        assert_eq!(list(&base).expect("list"), vec![DEFAULT_PROFILE.to_owned()]);
        std::fs::remove_dir_all(&base).ok();
    }
}

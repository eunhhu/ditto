//! The profile's skills, as the kernel sees them (ADR 0046).
//!
//! The format lives in `ditto_profile::skill`; this module is the kernel's two
//! questions about it: what is in the catalogue, and what does one body say.
//! Both read the directory through, because an installation changes a few times
//! a year and a cache would only add a way to be stale.

use std::path::Path;

use ditto_profile::skill::{self, Skill, SkillHeader};

use crate::{DittoKernel, KernelError};

impl DittoKernel {
    /// Every skill header in this profile, ordered by name.
    pub fn skill_headers(&self) -> Result<Vec<SkillHeader>, KernelError> {
        skill::list(&skills_root(&self.data_dir)).map_err(|error| {
            KernelError::InvalidCommand(format!("the skill catalogue is not readable: {error}"))
        })
    }

    /// One skill's header and body.
    pub fn skill_body(&self, name: &str) -> Result<Skill, KernelError> {
        skill::read(&skills_root(&self.data_dir), name).map_err(|error| {
            KernelError::InvalidCommand(format!("skill {name:?} is not readable: {error}"))
        })
    }
}

fn skills_root(data_dir: &Path) -> std::path::PathBuf {
    data_dir.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::KernelConfig;
    use std::path::PathBuf;

    fn kernel(root: &Path) -> DittoKernel {
        DittoKernel::open(KernelConfig::new(
            root.join("data"),
            root.join("capabilities"),
        ))
        .expect("kernel")
    }

    fn install(root: &Path, name: &str, description: &str, body: &str) {
        let directory = root.join("data").join("skills").join(name);
        std::fs::create_dir_all(&directory).expect("skill dir");
        std::fs::write(
            directory.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\n{body}"),
        )
        .expect("skill file");
    }

    #[test]
    fn the_kernel_answers_the_catalogue_and_one_body_from_the_profile_directory() {
        let root = tempfile::tempdir().expect("root");
        let kernel = kernel(root.path());
        assert_eq!(kernel.skill_headers().expect("empty catalogue"), Vec::new());

        install(root.path(), "photo-triage", "Sort photos.", "# Steps\n");
        let headers = kernel.skill_headers().expect("catalogue");
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].name, "photo-triage");
        assert_eq!(
            kernel.skill_body("photo-triage").expect("body").body,
            "# Steps\n"
        );

        // A read of something that is not a skill is refused by name, not by a
        // path, so no argument can point outside the profile.
        let error = kernel.skill_body("../journal").expect_err("refused");
        assert!(error.to_string().contains("not usable"), "{error}");
    }

    #[test]
    fn a_malformed_skill_makes_the_catalogue_unreadable_rather_than_silently_shorter() {
        let root = tempfile::tempdir().expect("root");
        let kernel = kernel(root.path());
        let directory = root.path().join("data/skills/broken");
        std::fs::create_dir_all(&directory).expect("dir");
        std::fs::write(directory.join("SKILL.md"), "# no frontmatter\n").expect("write");
        let error = kernel.skill_headers().expect_err("must not skip");
        assert!(error.to_string().contains("catalogue"), "{error}");
    }

    #[test]
    fn the_skills_root_is_the_data_directory_of_the_served_profile() {
        assert_eq!(
            skills_root(Path::new("/srv/ditto/profiles/minchae")),
            PathBuf::from("/srv/ditto/profiles/minchae")
        );
    }
}

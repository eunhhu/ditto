//! Local skill administration (ADR 0046).
//!
//! Like profiles, this reads the data root on this machine: "what skills does
//! this profile carry" has an answer even when no daemon is running.
use std::path::PathBuf;

use anyhow::Context;
use clap::Subcommand;

use super::presentation::{View, safe};

#[derive(Debug, Subcommand)]
pub(super) enum SkillCommand {
    /// List the skills in a profile's catalogue, by header only.
    List(SkillRoot),
    /// Print one skill's body.
    Show {
        name: String,
        #[command(flatten)]
        root: SkillRoot,
    },
}

#[derive(Debug, clap::Args)]
pub(super) struct SkillRoot {
    /// The profile whose skills to read.
    #[arg(long, env = "DITTO_PROFILE", default_value = ditto_profile::DEFAULT_PROFILE)]
    profile: String,
    /// The data root the daemon was given (`DITTO_DATA_DIR`).
    #[arg(long, env = "DITTO_DATA_DIR", default_value = ".ditto")]
    data_dir: PathBuf,
}

pub(super) async fn run(command: SkillCommand, view: View<'_>) -> anyhow::Result<()> {
    match command {
        SkillCommand::List(root) => {
            let directory = profile_root(&root)?;
            let headers = ditto_profile::skill::list(&directory)
                .with_context(|| format!("reading {}/skills", directory.display()))?;
            if view.human {
                if headers.is_empty() {
                    println!("no skills");
                }
                for header in &headers {
                    println!("{}\n  {}", safe(&header.name), safe(&header.description));
                }
            } else {
                let listing: Vec<serde_json::Value> = headers
                    .iter()
                    .map(|header| {
                        serde_json::json!({
                            "name": header.name,
                            "description": header.description,
                        })
                    })
                    .collect();
                println!("{}", serde_json::json!({ "skills": listing }));
            }
        }
        SkillCommand::Show { name, root } => {
            let directory = profile_root(&root)?;
            let skill = ditto_profile::skill::read(&directory, &name)
                .with_context(|| format!("reading skill {name:?}"))?;
            if view.human {
                println!("{}", safe(&skill.body));
            } else {
                println!(
                    "{}",
                    serde_json::json!({
                        "name": skill.header.name,
                        "description": skill.header.description,
                        "body": skill.body,
                    })
                );
            }
        }
    }
    Ok(())
}

fn profile_root(root: &SkillRoot) -> anyhow::Result<PathBuf> {
    ditto_profile::root(&root.data_dir, &root.profile).map_err(|error| anyhow::anyhow!("{error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_profile_name_in_the_folder_arg_still_resolves_to_one_root() {
        let root = SkillRoot {
            profile: ditto_profile::DEFAULT_PROFILE.to_owned(),
            data_dir: PathBuf::from("/tmp/ditto-skills-test"),
        };
        assert_eq!(
            profile_root(&root).expect("default profile"),
            PathBuf::from("/tmp/ditto-skills-test")
        );
        let named = SkillRoot {
            profile: "minchae".to_owned(),
            data_dir: PathBuf::from("/tmp/ditto-skills-test"),
        };
        assert_eq!(
            profile_root(&named).expect("named profile"),
            PathBuf::from("/tmp/ditto-skills-test/profiles/minchae")
        );
    }
}

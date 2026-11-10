//! Local profile administration (ADR 0044).
//!
//! These commands read the data root on this machine rather than the daemon's
//! API, because they answer "what can this host serve" - a question that has an
//! answer even when no daemon is running.

use std::path::PathBuf;

use anyhow::Context;
use clap::Subcommand;

use super::presentation::{View, safe};

#[derive(Debug, Subcommand)]
pub(super) enum ProfileCommand {
    /// List every profile this data root can serve.
    List(ProfileRoot),
    /// Print where a profile's data lives.
    Path {
        #[arg(default_value = ditto_profile::DEFAULT_PROFILE)]
        name: String,
        #[command(flatten)]
        root: ProfileRoot,
    },
}

#[derive(Debug, clap::Args)]
pub(super) struct ProfileRoot {
    /// The data root the daemon was given (`DITTO_DATA_DIR`).
    #[arg(long, env = "DITTO_DATA_DIR", default_value = ".ditto")]
    data_dir: PathBuf,
}

pub(super) async fn run(command: ProfileCommand, view: View<'_>) -> anyhow::Result<()> {
    match command {
        ProfileCommand::List(root) => {
            let names = ditto_profile::list(&root.data_dir)
                .with_context(|| format!("reading {}", root.data_dir.display()))?;
            if view.human {
                for name in &names {
                    let path = ditto_profile::root(&root.data_dir, name)
                        .expect("a listed name is a valid name");
                    println!("{}\t{}", safe(name), path.display());
                }
            } else {
                println!("{}", serde_json::json!({ "profiles": names }));
            }
        }
        ProfileCommand::Path { name, root } => {
            let path = ditto_profile::root(&root.data_dir, &name)
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            if view.human {
                println!("{}", path.display());
            } else {
                println!(
                    "{}",
                    serde_json::json!({ "profile": name, "data_dir": path.display().to_string() })
                );
            }
        }
    }
    Ok(())
}

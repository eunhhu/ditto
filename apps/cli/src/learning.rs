//! The learning ledger (ADR 0050): what Ditto learned from its own searches,
//! and the user's undo.
use anyhow::{Context, bail};
use clap::Subcommand;
use ditto_protocol::{LearnedKey, LearnedState, LearningLedger, RetireLearnedCommand};
use serde_json::Value;

use super::presentation::{View, safe};

#[derive(Debug, Subcommand)]
pub(super) enum LearningCommand {
    /// List every learned retrieval key of a session and where it stands.
    List {
        #[arg(long, default_value = "personal")]
        session: String,
    },
    /// Undo one learned key; it stops bringing its memory into context.
    Retire {
        key_id: String,
        #[arg(long, default_value = "personal")]
        session: String,
    },
}

pub(super) async fn run(
    client: &reqwest::Client,
    view: View<'_>,
    command: LearningCommand,
) -> anyhow::Result<()> {
    match command {
        LearningCommand::List { session } => {
            let ledger: LearningLedger = client
                .get(format!("{}/v1/learning", view.api))
                .query(&[("session_id", session.as_str())])
                .send()
                .await
                .context("asking the daemon for the learning ledger")?
                .error_for_status()?
                .json()
                .await?;
            if !view.human {
                println!("{}", serde_json::to_string(&ledger)?);
            } else if ledger.keys.is_empty() {
                println!("nothing learned in {}", safe(&ledger.session_id));
            } else {
                for key in &ledger.keys {
                    println!("{}", line(key));
                }
            }
        }
        LearningCommand::Retire { key_id, session } => {
            let response = client
                .post(format!("{}/v1/commands/learning/retire", view.api))
                .json(&RetireLearnedCommand {
                    session_id: session,
                    key_id,
                })
                .send()
                .await
                .context("asking the daemon to retire the key")?;
            if !response.status().is_success() {
                let body: Value = response.json().await.unwrap_or(Value::Null);
                bail!(
                    "{}",
                    body.get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("the daemon refused to retire the key")
                );
            }
            let key: LearnedKey = response.json().await?;
            if view.human {
                println!("retired {}", line(&key));
            } else {
                println!("{}", serde_json::to_string(&key)?);
            }
        }
    }
    Ok(())
}

fn line(key: &LearnedKey) -> String {
    let state = match key.state {
        LearnedState::Candidate => "candidate",
        LearnedState::Blocked => "blocked",
        LearnedState::Active => "active",
        LearnedState::Expired => "expired",
        LearnedState::Inert => "inert",
        LearnedState::Retired => "retired",
    };
    let memory = key
        .target_text
        .as_deref()
        .unwrap_or("(memory no longer active)");
    let mut text = format!(
        "{} {state}: [{}] -> {} ({} episodes, {} uses)",
        safe(&key.key_id),
        safe(&key.words.join(", ")),
        safe(memory),
        key.evidence.len(),
        key.uses
    );
    if let Some(reason) = &key.blocked {
        text.push_str(&format!(" - {}", safe(reason)));
    }
    text
}

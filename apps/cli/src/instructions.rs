//! The profile's standing instructions (ADR 0045).
use anyhow::{Context, bail};
use clap::Subcommand;
use serde_json::Value;

use super::presentation::{View, safe};

#[derive(Debug, Subcommand)]
pub(super) enum InstructionsCommand {
    /// Print the standing instructions in force.
    Show,
    /// Replace them. Larger than 2000 characters is refused, not truncated.
    Set(InstructionsText),
    /// Record an empty document, which pins nothing.
    Clear,
}

#[derive(Debug, clap::Args)]
pub(super) struct InstructionsText {
    /// The text. Prefer `--file` for anything long or multi-line.
    #[arg(long, conflicts_with = "file")]
    text: Option<String>,
    /// Read the text from this file.
    #[arg(long)]
    file: Option<std::path::PathBuf>,
}

pub(super) async fn run(
    client: &reqwest::Client,
    view: View<'_>,
    command: InstructionsCommand,
) -> anyhow::Result<()> {
    let text = match command {
        InstructionsCommand::Show => {
            let body: Value = client
                .get(format!("{}/v1/instructions", view.api))
                .send()
                .await
                .context("asking the daemon for the instructions")?
                .error_for_status()?
                .json()
                .await?;
            if view.human {
                match body.get("text").and_then(Value::as_str) {
                    Some(text) if !text.trim().is_empty() => println!("{}", safe(text)),
                    _ => println!("no standing instructions"),
                }
            } else {
                println!("{body}");
            }
            return Ok(());
        }
        InstructionsCommand::Clear => String::new(),
        InstructionsCommand::Set(args) => match (args.text, args.file) {
            (Some(text), _) => text,
            (None, Some(path)) => std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?,
            (None, None) => bail!("give --text or --file, or use `instructions clear`"),
        },
    };

    let response = client
        .put(format!("{}/v1/instructions", view.api))
        .json(&serde_json::json!({ "text": text }))
        .send()
        .await
        .context("sending the instructions")?;
    if !response.status().is_success() {
        let body: Value = response.json().await.unwrap_or(Value::Null);
        bail!(
            "{}",
            body.get("error")
                .and_then(Value::as_str)
                .unwrap_or("the daemon refused the instructions")
        );
    }
    let document: Value = response.json().await?;
    if view.human {
        let id = document
            .get("event_id")
            .and_then(Value::as_str)
            .unwrap_or("no id");
        println!("instructions recorded ({})", safe(id));
    } else {
        println!("{document}");
    }
    Ok(())
}

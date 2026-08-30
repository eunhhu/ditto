use anyhow::Context;
use clap::{Parser, Subcommand};
use ditto_protocol::{CapabilitySearchQuery, EventQuery, SubmitInputCommand};
use serde_json::Value;

#[derive(Debug, Parser)]
#[command(name = "ditto", version, about = "Operate the local Ditto daemon")]
struct Cli {
    #[arg(long, env = "DITTO_API", default_value = "http://127.0.0.1:8787")]
    api: String,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Check daemon health.
    Ping,
    /// Submit trusted user input. The daemon chooses actor and event kind.
    Input {
        text: String,
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        task: Option<String>,
    },
    /// Query durable events.
    Events {
        #[arg(long)]
        after_seq: Option<i64>,
        #[arg(long, default_value_t = 100)]
        limit: usize,
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        task: Option<String>,
    },
    /// List or search capability cards.
    Capabilities {
        query: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let client = reqwest::Client::new();
    let api = cli.api.trim_end_matches('/');

    match cli.command {
        Command::Ping => {
            let value = client
                .get(format!("{api}/health"))
                .send()
                .await
                .context("failed to reach Ditto daemon")?
                .error_for_status()
                .context("Ditto daemon returned an error")?
                .json::<Value>()
                .await
                .context("invalid health response")?;
            print_json(&value)?;
        }
        Command::Input {
            text,
            session,
            task,
        } => {
            let command = SubmitInputCommand {
                text,
                session_id: session,
                task_id: task,
            };
            let value = client
                .post(format!("{api}/v1/commands/input"))
                .json(&command)
                .send()
                .await
                .context("failed to submit input")?
                .error_for_status()
                .context("input command failed")?
                .json::<Value>()
                .await
                .context("invalid input response")?;
            print_json(&value)?;
        }
        Command::Events {
            after_seq,
            limit,
            session,
            task,
        } => {
            let query = EventQuery {
                after_seq,
                limit: Some(limit),
                session_id: session,
                task_id: task,
            };
            let value = client
                .get(format!("{api}/v1/events"))
                .query(&query)
                .send()
                .await
                .context("failed to query events")?
                .error_for_status()
                .context("event query failed")?
                .json::<Value>()
                .await
                .context("invalid event response")?;
            print_json(&value)?;
        }
        Command::Capabilities { query, limit } => {
            let query = CapabilitySearchQuery {
                query,
                limit: Some(limit),
            };
            let value = client
                .get(format!("{api}/v1/capabilities"))
                .query(&query)
                .send()
                .await
                .context("failed to query capabilities")?
                .error_for_status()
                .context("capability query failed")?
                .json::<Value>()
                .await
                .context("invalid capability response")?;
            print_json(&value)?;
        }
    }

    Ok(())
}

fn print_json(value: &Value) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

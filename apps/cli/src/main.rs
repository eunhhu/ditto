use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use ditto_protocol::{CapabilitySearchQuery, EventActor, EventQuery, NewEvent};
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
    /// Record a user input event.
    Input {
        text: String,
        #[arg(long, default_value = "local")]
        session: String,
        #[arg(long)]
        task: Option<String>,
    },
    /// Append an arbitrary typed event.
    Emit {
        #[arg(long)]
        kind: String,
        #[arg(long, default_value = "user")]
        actor: EventActor,
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        task: Option<String>,
        #[arg(long, default_value = "{}")]
        payload: String,
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
            let mut event = NewEvent::input(session, text);
            event.task_id = task;
            let value = append(&client, api, &event).await?;
            print_json(&value)?;
        }
        Command::Emit {
            kind,
            actor,
            session,
            task,
            payload,
        } => {
            if kind.trim().is_empty() {
                bail!("--kind must not be empty");
            }
            let payload = serde_json::from_str(&payload).context("--payload must be valid JSON")?;
            let event = NewEvent {
                session_id: session,
                task_id: task,
                actor,
                kind,
                payload,
                causation_id: None,
                correlation_id: None,
                span_id: None,
            };
            let value = append(&client, api, &event).await?;
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

async fn append(client: &reqwest::Client, api: &str, event: &NewEvent) -> anyhow::Result<Value> {
    client
        .post(format!("{api}/v1/events"))
        .json(event)
        .send()
        .await
        .context("failed to append event")?
        .error_for_status()
        .context("event append failed")?
        .json::<Value>()
        .await
        .context("invalid append response")
}

fn print_json(value: &Value) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

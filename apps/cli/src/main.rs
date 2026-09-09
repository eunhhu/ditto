use anyhow::Context;
use clap::{Parser, Subcommand};
use ditto_protocol::{CapabilitySearchQuery, EventQuery, SubmitInputCommand};
use serde_json::Value;
mod memory;
mod repeats;
mod runs;
mod schedules;
mod sorts;

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
    /// Repeat a read-only request at a fixed interval for a finite number of occurrences.
    Repeat(repeats::RepeatArgs),
    /// Inspect repeat progress and the latest occurrence result.
    RepeatStatus(runs::RunIdentity),
    /// Stop future occurrences and cancel this repeat's active execution.
    RepeatCancel(runs::RunIdentity),
    /// List active repeat definitions without loading their run outputs.
    RepeatList {
        #[arg(long, default_value = "personal")]
        session: String,
    },
    /// Schedule one future read-only model request; return after durable acceptance.
    Schedule(schedules::ScheduleArgs),
    /// Inspect a schedule and its original execution result.
    ScheduleStatus(runs::RunIdentity),
    /// Cancel a pending schedule or its active execution.
    ScheduleCancel(runs::RunIdentity),
    /// List pending schedules in a session (maximum 100 per data directory).
    ScheduleList {
        #[arg(long, default_value = "personal")]
        session: String,
    },
    /// Sort a UTF-8 file locally, optionally removing duplicates. No model call.
    Sort(sorts::SortArgs),
    /// Inspect a local sort request and its verified output.
    SortStatus(runs::RunIdentity),
    /// Cancel an active local sort request.
    SortCancel(runs::RunIdentity),
    /// Ask the configured model; wait for its answer unless detached.
    Run(runs::RunArgs),
    /// Inspect a run using its original request ID.
    RunStatus(runs::RunIdentity),
    /// Request cancellation of an active run.
    RunCancel(runs::RunIdentity),
    /// Save, inspect, or correct explicit user memory.
    Memory {
        #[command(subcommand)]
        command: memory::Command,
    },
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
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let api = cli.api.trim_end_matches('/');

    match cli.command {
        Command::Repeat(args) => repeats::create(&client, api, args).await?,
        Command::RepeatStatus(identity) => repeats::inspect(&client, api, identity).await?,
        Command::RepeatCancel(identity) => repeats::cancel(&client, api, identity).await?,
        Command::RepeatList { session } => repeats::active(&client, api, session).await?,
        Command::Schedule(args) => schedules::create(&client, api, args).await?,
        Command::ScheduleStatus(identity) => schedules::inspect(&client, api, identity).await?,
        Command::ScheduleCancel(identity) => schedules::cancel(&client, api, identity).await?,
        Command::ScheduleList { session } => schedules::pending(&client, api, session).await?,
        Command::Sort(args) => sorts::start(&client, api, args).await?,
        Command::SortStatus(identity) => sorts::inspect(&client, api, identity).await?,
        Command::SortCancel(identity) => sorts::cancel(&client, api, identity).await?,
        Command::Run(args) => runs::start(&client, api, args).await?,
        Command::RunStatus(identity) => runs::inspect(&client, api, identity).await?,
        Command::RunCancel(identity) => runs::cancel(&client, api, identity).await?,
        Command::Memory { command } => memory::run(&client, api, command).await?,
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

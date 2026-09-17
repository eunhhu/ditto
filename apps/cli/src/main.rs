use anyhow::Context;
use clap::{Parser, Subcommand};
use ditto_protocol::{CapabilitySearchQuery, EventQuery, SubmitInputCommand};
use serde_json::Value;
mod memory;
mod presentation;
mod repeats;
mod runs;
mod schedules;
mod sorts;

#[derive(Debug, Parser)]
#[command(name = "ditto", version, about = "Operate the local Ditto daemon")]
struct Cli {
    #[arg(
        long,
        env = "DITTO_API",
        hide_env_values = true,
        default_value = "http://127.0.0.1:8787"
    )]
    api: String,
    /// Human-readable run/sort/schedule/repeat output (JSON remains the default).
    #[arg(long, global = true)]
    human: bool,
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
async fn main() -> std::process::ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            if error.use_stderr() {
                eprintln!("{}", presentation::safe(&error.to_string()));
            } else {
                // Help/version text contains only the static command definition.
                print!("{error}");
            }
            return std::process::ExitCode::from(error.exit_code() as u8);
        }
    };
    match run(cli).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            // Transport/decoding diagnostics can contain untrusted server text.
            eprintln!("Error: {}", presentation::safe(&format!("{error:#}")));
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> anyhow::Result<()> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let api = cli.api.trim_end_matches('/');

    let view = presentation::View {
        api,
        human: cli.human,
    };
    match cli.command {
        Command::Repeat(args) => repeats::create(&client, view, args).await?,
        Command::RepeatStatus(identity) => repeats::inspect(&client, view, identity).await?,
        Command::RepeatCancel(identity) => repeats::cancel(&client, view, identity).await?,
        Command::RepeatList { session } => repeats::active(&client, view, session).await?,
        Command::Schedule(args) => schedules::create(&client, view, args).await?,
        Command::ScheduleStatus(identity) => schedules::inspect(&client, view, identity).await?,
        Command::ScheduleCancel(identity) => schedules::cancel(&client, view, identity).await?,
        Command::ScheduleList { session } => schedules::pending(&client, view, session).await?,
        Command::Sort(args) => sorts::start(&client, view, args).await?,
        Command::SortStatus(identity) => sorts::inspect(&client, view, identity).await?,
        Command::SortCancel(identity) => sorts::cancel(&client, view, identity).await?,
        Command::Run(args) => runs::start(&client, view, args).await?,
        Command::RunStatus(identity) => runs::inspect(&client, view, identity).await?,
        Command::RunCancel(identity) => runs::cancel(&client, view, identity).await?,
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

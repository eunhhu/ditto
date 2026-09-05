use anyhow::{Context, bail};
use clap::Subcommand;
use ditto_protocol::{
    MAX_USER_MEMORY_BYTES, MemoryPage, MemoryQuery, MemoryWriteOutcome, RememberInputCommand,
    RememberInputResponse, SubmitInputCommand, SubmitInputResponse,
};

#[derive(Debug, Subcommand)]
pub(super) enum Command {
    /// Save exact user text; use --replaces to correct one active memory.
    Save {
        text: String,
        #[arg(long, default_value = "personal")]
        session: String,
        #[arg(long)]
        replaces: Option<String>,
    },
    /// Promote or retry an input that has already been recorded.
    FromInput {
        input_event_id: String,
        #[arg(long, default_value = "personal")]
        session: String,
        #[arg(long)]
        replaces: Option<String>,
    },
    /// Inspect current memories, with an optional cursor from the previous page.
    List {
        #[arg(long, default_value = "personal")]
        session: String,
        #[arg(long)]
        after_id: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
}

pub(super) async fn run(
    client: &reqwest::Client,
    api: &str,
    command: Command,
) -> anyhow::Result<()> {
    match command {
        Command::Save {
            text,
            session,
            replaces,
        } => {
            if text.trim().is_empty() || text.trim().len() > MAX_USER_MEMORY_BYTES {
                bail!("memory text must contain 1 through 4096 UTF-8 bytes");
            }
            let input = client
                .post(format!("{api}/v1/commands/input"))
                .json(&SubmitInputCommand {
                    text,
                    session_id: Some(session.clone()),
                    task_id: None,
                })
                .send()
                .await
                .context("input capture was not confirmed")?
                .error_for_status()
                .context("input capture failed")?
                .json::<SubmitInputResponse>()
                .await
                .context("input capture response was invalid")?;
            let response = promote(client, api, &RememberInputCommand {
                session_id: session, input_event_id: input.event.event_id.clone(), replaces,
            }).await.with_context(|| format!(
                "input {} was captured, but memory saving was not confirmed; retry with 'ditto memory from-input {}' and the same --session and --replaces options",
                input.event.event_id, input.event.event_id
            ))?;
            print_write(response)?;
        }
        Command::FromInput {
            input_event_id,
            session,
            replaces,
        } => {
            print_write(
                promote(
                    client,
                    api,
                    &RememberInputCommand {
                        session_id: session,
                        input_event_id,
                        replaces,
                    },
                )
                .await?,
            )?;
        }
        Command::List {
            session,
            after_id,
            limit,
        } => {
            let page = client
                .get(format!("{api}/v1/memories"))
                .query(&MemoryQuery {
                    session_id: session,
                    after_id,
                    limit: Some(limit),
                })
                .send()
                .await
                .context("failed to reach memory service")?
                .error_for_status()
                .context("memory query failed")?
                .json::<MemoryPage>()
                .await
                .context("invalid memory page")?;
            println!("{}", serde_json::to_string_pretty(&page)?);
        }
    }
    Ok(())
}

async fn promote(
    client: &reqwest::Client,
    api: &str,
    command: &RememberInputCommand,
) -> anyhow::Result<RememberInputResponse> {
    client
        .post(format!("{api}/v1/commands/memory"))
        .json(command)
        .send()
        .await
        .context("memory response was not received")?
        .error_for_status()
        .context("memory promotion failed")?
        .json()
        .await
        .context("invalid memory response")
}

fn print_write(response: RememberInputResponse) -> anyhow::Result<()> {
    if response.outcome == MemoryWriteOutcome::CommittedButProjectionUnavailable {
        eprintln!(
            "Memory was saved durably, but its searchable projection is not ready; retry promotion with the same input ID."
        );
    }
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
}

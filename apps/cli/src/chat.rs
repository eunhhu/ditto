//! Interactive conversation: each line is one run in the session's current
//! thread; the daemon replays the thread's recent exchanges to the model.
use std::io::{BufRead, Write};

use anyhow::Context;
use ditto_protocol::{
    AgentRunStatus, ConversationResetResponse, RememberInputCommand, ResetConversationCommand,
    SubmitInputCommand, SubmitInputResponse,
};

use super::presentation::safe;

#[derive(Debug, clap::Args)]
pub(super) struct ChatArgs {
    #[arg(long, default_value = "personal")]
    session: String,
}

pub(super) async fn chat(
    client: &reqwest::Client,
    api: &str,
    args: ChatArgs,
) -> anyhow::Result<()> {
    eprintln!(
        "Ditto chat (session {}). /new starts a new thread, /remember <fact> saves a memory, /exit quits; Ctrl+C cancels a reply.",
        safe(&args.session)
    );
    loop {
        print!("you> ");
        std::io::stdout().flush()?;
        let line = tokio::task::spawn_blocking(|| {
            let mut line = String::new();
            std::io::stdin()
                .lock()
                .read_line(&mut line)
                .map(|read| (read, line))
        })
        .await??;
        let (read, line) = line;
        let text = line.trim();
        if read == 0 || matches!(text, "/exit" | "/quit") {
            return Ok(());
        }
        if text.is_empty() {
            continue;
        }
        if text == "/new" {
            reset(client, api, &args.session).await?;
            println!("-- new conversation --");
            continue;
        }
        if let Some(fact) = text.strip_prefix("/remember") {
            match remember(client, api, &args.session, fact.trim()).await {
                Ok(()) => println!("-- saved to memory --"),
                Err(error) => eprintln!("Error: {}", safe(&format!("{error:#}"))),
            }
            continue;
        }
        match super::runs::ask(client, api, &args.session, text).await {
            Ok(result) => match (result.status, result.response) {
                (AgentRunStatus::Unverified, Some(response)) => {
                    // Keep line breaks but escape controls within each line.
                    for (index, part) in response.split('\n').enumerate() {
                        println!(
                            "{}{}",
                            if index == 0 { "ditto> " } else { "       " },
                            safe(part)
                        );
                    }
                }
                (status, _) => eprintln!(
                    "(no answer: {} {})",
                    safe(&format!("{status:?}").to_lowercase()),
                    safe(result.failure_code.as_deref().unwrap_or(""))
                ),
            },
            Err(error) => eprintln!("Error: {}", safe(&format!("{error:#}"))),
        }
    }
}

/// Save exact user text as a memory through the typed input and memory commands.
async fn remember(
    client: &reqwest::Client,
    api: &str,
    session: &str,
    fact: &str,
) -> anyhow::Result<()> {
    if fact.is_empty() {
        anyhow::bail!("usage: /remember <fact>");
    }
    let input: SubmitInputResponse = client
        .post(format!("{api}/v1/commands/input"))
        .json(&SubmitInputCommand {
            text: fact.to_owned(),
            session_id: Some(session.to_owned()),
            task_id: None,
        })
        .send()
        .await
        .context("could not reach the Ditto daemon")?
        .error_for_status()
        .context("input was not recorded")?
        .json()
        .await
        .context("invalid input response")?;
    client
        .post(format!("{api}/v1/commands/memory"))
        .json(&RememberInputCommand {
            session_id: session.to_owned(),
            input_event_id: input.event.event_id,
            replaces: None,
        })
        .send()
        .await
        .context("could not reach the Ditto daemon")?
        .error_for_status()
        .context("memory was not saved")?;
    Ok(())
}

pub(super) async fn reset(
    client: &reqwest::Client,
    api: &str,
    session: &str,
) -> anyhow::Result<ConversationResetResponse> {
    client
        .post(format!("{api}/v1/commands/conversation/reset"))
        .json(&ResetConversationCommand {
            session_id: session.to_owned(),
        })
        .send()
        .await
        .context("could not reach the Ditto daemon")?
        .error_for_status()
        .context("conversation reset failed")?
        .json()
        .await
        .context("invalid conversation reset response")
}

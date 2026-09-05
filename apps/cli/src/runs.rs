use std::time::Duration;

use anyhow::{Context, bail};
use ditto_protocol::{
    AgentRunQuery, AgentRunResponse, AgentRunStatus, AgentSortPermission, EventQuery,
    StartAgentRunCommand,
};
use futures_util::StreamExt;

#[derive(Debug, clap::Args)]
pub(super) struct RunArgs {
    text: String,
    /// Permit one sort of this exact file during the run (64 KiB / 4096 lines).
    #[arg(long)]
    sort_file: Option<std::path::PathBuf>,
    /// Also permit removing exact duplicate lines from the attached file.
    #[arg(long, requires = "sort_file")]
    allow_deduplicate: bool,
    #[arg(long, default_value = "personal")]
    session: String,
    /// Reuse this ID only for an identical request retry.
    #[arg(long)]
    request_id: Option<String>,
    /// Return after durable acceptance instead of waiting for the answer.
    #[arg(long)]
    detach: bool,
}

#[derive(Debug, clap::Args)]
pub(super) struct RunIdentity {
    request_id: String,
    #[arg(long, default_value = "personal")]
    session: String,
}

impl From<RunIdentity> for AgentRunQuery {
    fn from(identity: RunIdentity) -> Self {
        Self {
            request_id: identity.request_id,
            session_id: identity.session,
        }
    }
}

pub(super) async fn start(
    client: &reqwest::Client,
    api: &str,
    args: RunArgs,
) -> anyhow::Result<()> {
    let sort = args
        .sort_file
        .as_ref()
        .map(|path| {
            let text = super::sorts::read_input(path)?;
            eprintln!(
                "Permission: sort attached file once; deduplication {} (expires with this run).",
                if args.allow_deduplicate {
                    "allowed"
                } else {
                    "not allowed"
                }
            );
            Ok::<_, anyhow::Error>(AgentSortPermission {
                text,
                allow_deduplicate: args.allow_deduplicate,
            })
        })
        .transpose()?;
    let request_id = args
        .request_id
        .unwrap_or_else(|| ulid::Ulid::new().to_string());
    // Print before transport: an uncertain POST must be recoverable without
    // guessing a new identity or automatically repeating potentially paid work.
    eprintln!("Run request: {request_id} (session: {})", args.session);
    let query = AgentRunQuery {
        request_id: request_id.clone(),
        session_id: args.session.clone(),
    };
    let result = decode(
        client
            .post(format!("{api}/v1/commands/run"))
            .timeout(Duration::from_secs(30))
            .json(&StartAgentRunCommand {
                request_id,
                session_id: args.session,
                text: args.text,
                sort,
            })
            .send()
            .await
            .context("run submission was not confirmed; inspect or retry the same request ID")?,
    )
    .await?;
    if args.detach || result.status != AgentRunStatus::Running {
        return print_result(result);
    }
    let wait = wait_for_terminal(client, api, &query, &result.task_id);
    tokio::pin!(wait);
    tokio::select! {
        result = &mut wait => print_result(result?),
        signal = tokio::signal::ctrl_c() => {
            signal.context("could not listen for Ctrl+C")?;
            let result = request_cancel(client, api, &query).await?;
            eprintln!("Cancellation requested; inspect the same request ID for the terminal state.");
            print_result(result)
        }
    }
}

pub(super) async fn inspect(
    client: &reqwest::Client,
    api: &str,
    identity: RunIdentity,
) -> anyhow::Result<()> {
    print_result(get_status(client, api, &identity.into()).await?)
}

pub(super) async fn cancel(
    client: &reqwest::Client,
    api: &str,
    identity: RunIdentity,
) -> anyhow::Result<()> {
    print_result(request_cancel(client, api, &identity.into()).await?)
}

async fn request_cancel(
    client: &reqwest::Client,
    api: &str,
    query: &AgentRunQuery,
) -> anyhow::Result<AgentRunResponse> {
    decode(
        client
            .post(format!("{api}/v1/commands/run/cancel"))
            .json(query)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .context("cancellation was not confirmed")?,
    )
    .await
}

async fn get_status(
    client: &reqwest::Client,
    api: &str,
    query: &AgentRunQuery,
) -> anyhow::Result<AgentRunResponse> {
    decode(
        client
            .get(format!("{api}/v1/runs"))
            .query(query)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .context("could not inspect run")?,
    )
    .await
}

async fn decode(response: reqwest::Response) -> anyhow::Result<AgentRunResponse> {
    if !response.status().is_success() {
        let status = response.status();
        // Protocol errors are path-free and never contain provider credentials.
        let value: serde_json::Value = response
            .json()
            .await
            .context("invalid run error response")?;
        bail!(
            "run command failed ({status}): {}",
            value
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("invalid command")
        );
    }
    response
        .json()
        .await
        .context("invalid run response; inspect the same request ID")
}

fn print_result(result: AgentRunResponse) -> anyhow::Result<()> {
    let failed = matches!(
        result.status,
        AgentRunStatus::Failed | AgentRunStatus::Interrupted
    );
    super::print_json(&serde_json::to_value(&result)?)?;
    if failed {
        bail!("run ended without a model answer; see status above");
    }
    Ok(())
}

async fn wait_for_terminal(
    client: &reqwest::Client,
    api: &str,
    query: &AgentRunQuery,
    task: &str,
) -> anyhow::Result<AgentRunResponse> {
    let stream = client
        .get(format!("{api}/v1/stream"))
        .query(&EventQuery {
            session_id: Some(query.session_id.clone()),
            task_id: Some(task.to_owned()),
            after_seq: Some(0),
            limit: Some(100),
        })
        .timeout(Duration::from_secs(6 * 60))
        .send()
        .await
        .context("could not follow run; inspect the same request ID")?
        .error_for_status()
        .context("run event stream failed")?;
    let mut stream = stream.bytes_stream();
    let mut notice = TerminalNotice::default();
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(chunk) if notice.push(&chunk) => {
                let status = get_status(client, api, query).await?;
                if status.status != AgentRunStatus::Running {
                    return Ok(status);
                }
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    let status = get_status(client, api, query).await?;
    if status.status == AgentRunStatus::Running {
        bail!("event connection ended while run is active; inspect the same request ID");
    }
    Ok(status)
}

/// Event payloads may be large. Retain only a bounded event-name line, never
/// model requests/results or a growing transcript; canonical status is fetched
/// once a terminal event wakes the client.
#[derive(Default)]
pub(super) struct TerminalNotice {
    line: Vec<u8>,
    overflow: bool,
    terminal: bool,
    sort: bool,
}

impl TerminalNotice {
    pub(super) fn sort() -> Self {
        Self {
            sort: true,
            ..Default::default()
        }
    }

    pub(super) fn push(&mut self, chunk: &[u8]) -> bool {
        let mut noticed = false;
        for &byte in chunk {
            if byte == b'\n' {
                if self.line.last() == Some(&b'\r') {
                    self.line.pop();
                }
                if self.line.is_empty() && !self.overflow {
                    noticed |= self.terminal;
                    self.terminal = false;
                } else if !self.overflow && self.line.starts_with(b"event:") {
                    let event = self.line[6..].strip_prefix(b" ").unwrap_or(&self.line[6..]);
                    self.terminal = if self.sort {
                        event == b"task.completed" || event == b"sort.failed"
                    } else {
                        event == b"turn.finished" || event == b"turn.failed"
                    };
                }
                self.line.clear();
                self.overflow = false;
            } else if self.line.len() < 128 {
                self.line.push(byte);
            } else {
                self.overflow = true;
            }
        }
        noticed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_notice_handles_split_crlf_and_discards_large_payloads() {
        let mut notice = TerminalNotice::default();
        assert!(!notice.push(b"event: model.output\r\ndata: "));
        for _ in 0..10_000 {
            assert!(!notice.push(b"large content "));
        }
        assert!(notice.line.len() <= 128);
        assert!(!notice.push(b"\r\n\r\nevent: turn.fi"));
        assert!(!notice.push(b"nished\r\ndata: {}\r"));
        assert!(notice.push(b"\n\r\n"));
        assert!(!notice.push(b": keep-alive\n\n"));
    }
}

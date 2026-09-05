use std::{io::Read, path::PathBuf, time::Duration};

use anyhow::{Context, bail};
use ditto_protocol::{AgentRunQuery, EventQuery, SortRunResponse, SortRunStatus, StartSortCommand};
use futures_util::StreamExt;

use super::runs::{RunIdentity, TerminalNotice};

#[derive(Debug, clap::Args)]
pub(super) struct SortArgs {
    file: PathBuf,
    #[arg(long)]
    unique: bool,
    #[arg(long, default_value = "personal")]
    session: String,
    #[arg(long)]
    request_id: Option<String>,
    #[arg(long)]
    detach: bool,
}

pub(super) async fn start(
    client: &reqwest::Client,
    api: &str,
    args: SortArgs,
) -> anyhow::Result<()> {
    // Read only a bounded regular file through the descriptor actually opened.
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Opening a FIFO must not block before the descriptor-type check below.
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options
        .open(&args.file)
        .context("could not open sort input file")?;
    anyhow::ensure!(
        file.metadata()?.is_file(),
        "sort input must be a regular file"
    );
    let mut bytes = Vec::new();
    file.take(64 * 1024 + 1)
        .read_to_end(&mut bytes)
        .context("could not read sort input")?;
    anyhow::ensure!(bytes.len() <= 64 * 1024, "sort input exceeds 64 KiB");
    let text = String::from_utf8(bytes).context("sort input must be UTF-8")?;
    let query = AgentRunQuery {
        request_id: args
            .request_id
            .unwrap_or_else(|| ulid::Ulid::new().to_string()),
        session_id: args.session,
    };
    eprintln!(
        "Sort request: {} (session: {})",
        query.request_id, query.session_id
    );
    let result = decode(
        client
            .post(format!("{api}/v1/commands/sort"))
            .json(&StartSortCommand {
                request_id: query.request_id.clone(),
                session_id: query.session_id.clone(),
                text,
                unique: args.unique,
            })
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .context("sort submission was not confirmed; inspect or retry the same request ID")?,
    )
    .await?;
    if args.detach || result.status != SortRunStatus::Running {
        return print_result(result);
    }
    let wait = follow(client, api, &query, &result.task_id);
    tokio::pin!(wait);
    tokio::select! {
        result = &mut wait => print_result(result?),
        signal = tokio::signal::ctrl_c() => {
            signal.context("could not listen for Ctrl+C")?;
            let result = request(client, api, &query, true).await?;
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
    print_result(request(client, api, &identity.into(), false).await?)
}

pub(super) async fn cancel(
    client: &reqwest::Client,
    api: &str,
    identity: RunIdentity,
) -> anyhow::Result<()> {
    print_result(request(client, api, &identity.into(), true).await?)
}

async fn request(
    client: &reqwest::Client,
    api: &str,
    query: &AgentRunQuery,
    cancel: bool,
) -> anyhow::Result<SortRunResponse> {
    let request = if cancel {
        client
            .post(format!("{api}/v1/commands/sort/cancel"))
            .json(query)
    } else {
        client.get(format!("{api}/v1/sorts")).query(query)
    };
    decode(
        request
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .context("could not inspect or cancel sort")?,
    )
    .await
}

async fn decode(response: reqwest::Response) -> anyhow::Result<SortRunResponse> {
    if !response.status().is_success() {
        let code = response.status();
        let body: serde_json::Value = response
            .json()
            .await
            .context("invalid sort error response")?;
        bail!(
            "sort command failed ({code}): {}",
            body["error"].as_str().unwrap_or("invalid command")
        );
    }
    response
        .json()
        .await
        .context("invalid sort response; inspect the same request ID")
}

fn print_result(result: SortRunResponse) -> anyhow::Result<()> {
    let failed = matches!(
        result.status,
        SortRunStatus::Failed | SortRunStatus::Interrupted
    );
    super::print_json(&serde_json::to_value(&result)?)?;
    if failed {
        bail!("sort ended without verified output; see status above");
    }
    Ok(())
}

async fn follow(
    client: &reqwest::Client,
    api: &str,
    query: &AgentRunQuery,
    task: &str,
) -> anyhow::Result<SortRunResponse> {
    let stream = client
        .get(format!("{api}/v1/stream"))
        .query(&EventQuery {
            session_id: Some(query.session_id.clone()),
            task_id: Some(task.into()),
            after_seq: Some(0),
            limit: Some(100),
        })
        .timeout(Duration::from_secs(40))
        .send()
        .await
        .context("could not follow sort; inspect the same request ID")?
        .error_for_status()
        .context("sort event stream failed")?;
    let mut stream = stream.bytes_stream();
    let mut notice = TerminalNotice::sort();
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(chunk) if notice.push(&chunk) => break,
            Ok(_) => {}
            Err(_) => break,
        }
    }
    let result = request(client, api, query, false).await?;
    if result.status == SortRunStatus::Running {
        bail!("sort is still active; inspect the same request ID");
    }
    Ok(result)
}

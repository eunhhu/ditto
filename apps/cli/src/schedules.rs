use anyhow::{Context, bail};
use std::time::Duration;

#[derive(Debug, clap::Args)]
pub(super) struct ScheduleArgs {
    text: String,
    /// Future RFC 3339 instant with explicit offset (for example, +09:00).
    #[arg(long)]
    at: String,
    /// Exclusive latest-start instant. At most 24 hours after --at.
    #[arg(long)]
    expires: String,
    #[arg(long, default_value = "personal")]
    session: String,
    /// Reuse only for an identical schedule retry.
    #[arg(long)]
    request_id: Option<String>,
}

pub(super) async fn create(
    client: &reqwest::Client,
    api: &str,
    args: ScheduleArgs,
) -> anyhow::Result<()> {
    let request_id = args
        .request_id
        .unwrap_or_else(|| ulid::Ulid::new().to_string());
    // Decode through the wire contract without adding another date parser.
    let command: ditto_protocol::ScheduleRunCommand = serde_json::from_value(serde_json::json!({
        "request_id":request_id,"session_id":args.session,"text":args.text,
        "due_at":args.at,"expires_at":args.expires,
    }))
    .context("--at and --expires require RFC 3339 timestamps with an explicit offset")?;
    eprintln!(
        "Schedule request: {} (session: {}). Execution requires an explicitly enabled daemon provider.",
        command.request_id, command.session_id
    );
    let response = client
        .post(format!("{api}/v1/commands/schedule"))
        .json(&command);
    show(response).await
}
pub(super) async fn inspect(
    client: &reqwest::Client,
    api: &str,
    identity: super::runs::RunIdentity,
) -> anyhow::Result<()> {
    let query: ditto_protocol::AgentRunQuery = identity.into();
    show(client.get(format!("{api}/v1/schedules")).query(&query)).await
}
pub(super) async fn cancel(
    client: &reqwest::Client,
    api: &str,
    identity: super::runs::RunIdentity,
) -> anyhow::Result<()> {
    let query: ditto_protocol::AgentRunQuery = identity.into();
    show(
        client
            .post(format!("{api}/v1/commands/schedule/cancel"))
            .json(&query),
    )
    .await
}
pub(super) async fn pending(
    client: &reqwest::Client,
    api: &str,
    session: String,
) -> anyhow::Result<()> {
    show(client.get(format!("{api}/v1/schedules/pending")).query(
        &ditto_protocol::ScheduleListQuery {
            session_id: session,
        },
    ))
    .await
}
async fn show(request: reqwest::RequestBuilder) -> anyhow::Result<()> {
    let response = request
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .context("schedule operation was not confirmed; inspect or retry the same request ID")?;
    let status = response.status();
    let value: serde_json::Value = response.json().await.context("invalid schedule response")?;
    if !status.is_success() {
        bail!(
            "schedule command failed ({status}): {}",
            value["error"].as_str().unwrap_or("invalid command")
        );
    }
    super::print_json(&value)?;
    if matches!(
        value["status"].as_str(),
        Some("failed" | "interrupted" | "missed")
    ) {
        bail!("scheduled request did not finish with an answer; see status above");
    }
    Ok(())
}

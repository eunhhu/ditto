use ditto_protocol::{AgentRunQuery, RepeatScheduleCommand, ScheduleListQuery};

#[derive(Debug, clap::Args)]
pub(super) struct RepeatArgs {
    #[command(flatten)]
    schedule: super::schedules::ScheduleArgs,
    /// Fixed elapsed-time interval (60 seconds through 31 days).
    #[arg(long)]
    every_seconds: u32,
    /// Finite occurrence count (2 through 1000, within one year).
    #[arg(long)]
    occurrences: u32,
}

pub(super) async fn create(
    client: &reqwest::Client,
    api: &str,
    args: RepeatArgs,
) -> anyhow::Result<()> {
    let base = super::schedules::command(args.schedule)?;
    let command = RepeatScheduleCommand {
        request_id: base.request_id,
        session_id: base.session_id,
        text: base.text,
        due_at: base.due_at,
        expires_at: base.expires_at,
        every_seconds: args.every_seconds,
        occurrences: args.occurrences,
    };
    eprintln!(
        "Repeat request: {} (session: {}). Expired occurrences are skipped; execution requires an enabled daemon provider.",
        command.request_id, command.session_id
    );
    super::schedules::show(
        client
            .post(format!("{api}/v1/commands/repeat"))
            .json(&command),
    )
    .await
}
pub(super) async fn inspect(
    client: &reqwest::Client,
    api: &str,
    identity: super::runs::RunIdentity,
) -> anyhow::Result<()> {
    let query: AgentRunQuery = identity.into();
    super::schedules::show(client.get(format!("{api}/v1/repeats")).query(&query)).await
}
pub(super) async fn cancel(
    client: &reqwest::Client,
    api: &str,
    identity: super::runs::RunIdentity,
) -> anyhow::Result<()> {
    let query: AgentRunQuery = identity.into();
    super::schedules::show(
        client
            .post(format!("{api}/v1/commands/repeat/cancel"))
            .json(&query),
    )
    .await
}
pub(super) async fn active(
    client: &reqwest::Client,
    api: &str,
    session: String,
) -> anyhow::Result<()> {
    super::schedules::show(client.get(format!("{api}/v1/repeats/active")).query(
        &ScheduleListQuery {
            session_id: session,
        },
    ))
    .await
}

use super::presentation::{Kind, View};
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
    view: View<'_>,
    args: RepeatArgs,
) -> anyhow::Result<()> {
    let api = view.api;
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
    view.submitting(Kind::Repeat, &command.request_id, &command.session_id);
    eprintln!("Expired occurrences are skipped; execution requires an enabled daemon provider.");
    super::schedules::show(
        client
            .post(format!("{api}/v1/commands/repeat"))
            .json(&command),
        view,
        Kind::Repeat,
    )
    .await
}
pub(super) async fn inspect(
    client: &reqwest::Client,
    view: View<'_>,
    identity: super::runs::RunIdentity,
) -> anyhow::Result<()> {
    let api = view.api;
    let query: AgentRunQuery = identity.into();
    super::schedules::show(
        client.get(format!("{api}/v1/repeats")).query(&query),
        view,
        Kind::Repeat,
    )
    .await
}
pub(super) async fn cancel(
    client: &reqwest::Client,
    view: View<'_>,
    identity: super::runs::RunIdentity,
) -> anyhow::Result<()> {
    let api = view.api;
    let query: AgentRunQuery = identity.into();
    super::schedules::show(
        client
            .post(format!("{api}/v1/commands/repeat/cancel"))
            .json(&query),
        view,
        Kind::Repeat,
    )
    .await
}
pub(super) async fn active(
    client: &reqwest::Client,
    view: View<'_>,
    session: String,
) -> anyhow::Result<()> {
    let api = view.api;
    super::schedules::show(
        client
            .get(format!("{api}/v1/repeats/active"))
            .query(&ScheduleListQuery {
                session_id: session,
            }),
        view,
        Kind::Repeat,
    )
    .await
}

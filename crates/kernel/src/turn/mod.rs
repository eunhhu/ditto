pub(crate) mod fetch;
pub(crate) mod recall;
mod replay;
mod run;
mod shared;
pub(crate) mod sort;
mod thread;
mod types;

pub use fetch::{
    FetchToolError, FetchToolOutput, FetchToolRequested, FetchToolResult, FetchToolStarted,
    ReplayedFetchCall,
};
pub use recall::{
    RecallToolOutput, RecallToolRequested, RecallToolResult, RecalledMemory, ReplayedRecallCall,
};
pub use replay::replay_artifact_read_turn;
pub(crate) use run::ToolContracts;
pub use shared::request_sha256;
pub use sort::{
    ReplayedSortCall, SortToolError, SortToolOutput, SortToolRequested, SortToolResult,
    SortToolStarted,
};
pub(crate) use thread::ThreadReuse;
pub use types::*;

/// What `memory.search` reads in a turn: the compiled nodes, then the
/// candidates the compilation left out only as irrelevant or over budget.
/// Only those are copied.
pub(crate) fn recall_space<'a>(
    compiled: &ditto_context::CompiledContext,
    candidates: impl IntoIterator<Item = &'a ditto_context::ContextNode>,
) -> Vec<ditto_context::ContextNode> {
    let excluded = compiled
        .receipt
        .excluded
        .iter()
        .filter(|exclusion| recall::searchable(&exclusion.reason))
        .map(|exclusion| exclusion.node_id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let mut space = compiled.nodes.clone();
    space.extend(
        candidates
            .into_iter()
            .filter(|node| excluded.contains(node.id.as_str()))
            .cloned(),
    );
    space
}

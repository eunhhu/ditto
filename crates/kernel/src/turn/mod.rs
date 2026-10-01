pub(crate) mod memory;
mod replay;
mod run;
mod shared;
pub(crate) mod sort;
mod thread;
pub(crate) mod tool;
mod types;
pub(crate) mod web;

pub use memory::{
    MemoryAction, MemoryRefusal, MemoryResult, MemoryWrite, MemoryWrittenPayload, RecalledMemory,
};
pub use replay::replay_artifact_read_turn;
pub(crate) use run::ToolContracts;
pub use shared::request_sha256;
pub use sort::{SortToolError, SortToolResult};
pub(crate) use thread::ThreadReuse;
pub use tool::{ReplayedToolCall, ToolOutput, ToolRequested, ToolStarted};
pub use types::*;
pub use web::{WebResult, WebToolError};

/// What a memory search reads in a turn: the compiled nodes, then the
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
        .filter(|exclusion| memory::searchable(&exclusion.reason))
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

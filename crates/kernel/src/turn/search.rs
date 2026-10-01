//! `web.search` inside a turn (ADR 0033): the model searches on its own, only
//! at the operator's endpoint, at most three times a turn. Replay validates
//! the recorded results without network I/O.
use chrono::{DateTime, Utc};
use ditto_model::ProviderCallId;
use ditto_protocol::{EventActor, EventRecord, event_kind};
use ditto_web_fetch::search::{
    self as web, MAX_SEARCHES, SearchArguments, SearchError, SearchHit, SearchResults,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The lease that bounds a turn's searches.
pub(crate) const LEASE_ID: &str = "agent-search";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchToolRequested {
    pub event_version: u16,
    pub turn_id: String,
    pub request_index: u8,
    pub call_id: ProviderCallId,
    pub arguments: Value,
    pub normalized: Option<SearchArguments>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchToolStarted {
    pub event_version: u16,
    pub turn_id: String,
    pub request_index: u8,
    pub call_id: ProviderCallId,
    pub epoch_id: String,
    pub invocation_digest: String,
    pub claim_id: String,
    pub permit_id: String,
    pub claimed_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub normalized: SearchArguments,
    /// What the query became at the operator's endpoint.
    pub request_url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchToolError {
    InvalidArguments,
    /// The query looks like it holds a secret, so it is never sent.
    Credential,
    LeaseExhausted,
    LeaseExpired,
    Cancelled,
    Deadline,
    Connection,
    Timeout,
    HttpStatus,
    InvalidResponse,
}

impl SearchToolError {
    /// Errors a started search may end with.
    const fn after_start(self) -> bool {
        !matches!(
            self,
            Self::InvalidArguments | Self::Credential | Self::LeaseExhausted | Self::LeaseExpired
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum SearchToolResult {
    Found {
        results: Vec<SearchHit>,
    },
    Error {
        code: SearchToolError,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<u16>,
    },
}

impl SearchToolResult {
    pub(crate) fn from_search(result: Result<SearchResults, SearchError>) -> Self {
        let (code, status) = match result {
            Ok(found) => {
                return Self::Found {
                    results: found.results,
                };
            }
            Err(SearchError::HttpStatus { status }) => (SearchToolError::HttpStatus, Some(status)),
            Err(SearchError::Cancelled) => (SearchToolError::Cancelled, None),
            Err(SearchError::Timeout) => (SearchToolError::Timeout, None),
            Err(SearchError::InvalidResponse) => (SearchToolError::InvalidResponse, None),
            Err(SearchError::Connection | SearchError::Unauthorized) => {
                (SearchToolError::Connection, None)
            }
        };
        Self::Error { code, status }
    }

    /// What the model reads. Results are labeled as untrusted content.
    pub(crate) fn model_value(&self) -> Value {
        match self {
            Self::Found { results } => json!({
                "results": results,
                "content_origin": "untrusted search results",
            }),
            Self::Error { code, status } => match status {
                Some(status) => json!({"error": code, "status": status}),
                None => json!({"error": code}),
            },
        }
    }

    pub(crate) fn is_error(&self) -> bool {
        matches!(self, Self::Error { .. })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchToolOutput {
    pub event_version: u16,
    pub turn_id: String,
    pub request_index: u8,
    pub call_id: ProviderCallId,
    pub claimed: bool,
    pub result: SearchToolResult,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayedSearchCall {
    pub requested: SearchToolRequested,
    pub started: Option<SearchToolStarted>,
    pub output: Option<SearchToolOutput>,
}

/// The replay-side normalization: the raw schema, then the trimmed query.
pub(crate) fn normalize_call(arguments: &Value) -> Option<SearchArguments> {
    ditto_capability::validate_invocation_instance(&web::schema().input_schema, arguments).ok()?;
    web::normalized_query(arguments).map(|query| SearchArguments { query })
}

/// The refusal a search gets before it is authorized, if any: a query that
/// looks like a secret is never sent.
pub(crate) fn refusal(normalized: Option<&SearchArguments>) -> Option<SearchToolError> {
    match normalized {
        None => Some(SearchToolError::InvalidArguments),
        Some(arguments) if super::memory_write::credential_like(&arguments.query) => {
            Some(SearchToolError::Credential)
        }
        Some(_) => None,
    }
}

fn same_scope(event: &EventRecord, input: &EventRecord) -> bool {
    event.session_id == input.session_id
        && event.task_id == input.task_id
        && event.correlation_id == input.correlation_id
        && event.seq > input.seq
}

pub(crate) fn valid_requested(
    request: &SearchToolRequested,
    event: &EventRecord,
    input: &EventRecord,
) -> bool {
    request.event_version == 1
        && request.turn_id == input.correlation_id.as_deref().unwrap_or_default()
        && request.request_index < 7
        && request.normalized == normalize_call(&request.arguments)
        && event.kind == event_kind::AGENT_SEARCH_REQUESTED
        && event.actor == EventActor::Model
        && event.span_id.as_deref() == Some(request.call_id.as_str())
        && same_scope(event, input)
}

pub(crate) fn valid_started(
    start: &SearchToolStarted,
    event: &EventRecord,
    request: &SearchToolRequested,
    requested: &EventRecord,
    input: &EventRecord,
) -> bool {
    let digest = &start.invocation_digest;
    start.event_version == 1
        && start.turn_id == request.turn_id
        && start.request_index == request.request_index
        && start.call_id == request.call_id
        && request.normalized.as_ref() == Some(&start.normalized)
        && refusal(Some(&start.normalized)).is_none()
        && web::query_of(&start.request_url).as_deref() == Some(start.normalized.query.as_str())
        && event.kind == event_kind::AGENT_SEARCH_STARTED
        && event.actor == EventActor::Capability
        && event.span_id.as_deref() == Some(start.call_id.as_str())
        && event.causation_id.as_deref() == Some(&requested.event_id)
        && event.seq > requested.seq
        && same_scope(event, input)
        && !start.epoch_id.is_empty()
        && start.epoch_id.len() <= 256
        && digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        && start.claim_id == format!("claim_{digest}")
        && start.permit_id == format!("permit_{digest}")
        && start.claimed_at.timestamp_millis() <= event.recorded_at.timestamp_millis()
        && start.claimed_at >= input.recorded_at
        && start.expires_at > start.claimed_at
}

/// Structural evidence. Replay performs no network I/O; the recorded results
/// are the search result.
#[allow(clippy::too_many_arguments)]
pub(crate) fn valid_output(
    output: &SearchToolOutput,
    event: &EventRecord,
    input: &EventRecord,
    request: &SearchToolRequested,
    requested: &EventRecord,
    started: Option<&EventRecord>,
    searches_used: u32,
    deadline: DateTime<Utc>,
) -> bool {
    let cause = started.unwrap_or(requested);
    if output.event_version != 1
        || output.turn_id != request.turn_id
        || output.call_id != request.call_id
        || output.request_index != request.request_index
        || output.claimed != started.is_some()
        || event.actor != EventActor::Capability
        || event.kind != event_kind::AGENT_SEARCH_OUTPUT
        || event.span_id.as_deref() != Some(output.call_id.as_str())
        || event.causation_id.as_deref() != Some(&cause.event_id)
        || event.seq <= cause.seq
        || !same_scope(event, input)
    {
        return false;
    }
    match &output.result {
        SearchToolResult::Found { results } => {
            started.is_some()
                && SearchResults {
                    results: results.clone(),
                }
                .is_bounded()
        }
        SearchToolResult::Error { code, status } => {
            if status.is_some() != (*code == SearchToolError::HttpStatus)
                || status.is_some_and(|status| !(100..600).contains(&status))
            {
                return false;
            }
            if started.is_some() {
                return code.after_start();
            }
            match refusal(request.normalized.as_ref()) {
                Some(refused) => *code == refused,
                None => {
                    (searches_used >= MAX_SEARCHES && *code == SearchToolError::LeaseExhausted)
                        || (*code == SearchToolError::LeaseExpired && event.recorded_at >= deadline)
                }
            }
        }
    }
}

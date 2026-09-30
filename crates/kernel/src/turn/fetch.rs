//! `web.fetch` inside a turn (ADR 0027): the user's own message grants the
//! URLs it contains, and nothing else can be fetched.
use chrono::{DateTime, Utc};
use ditto_model::ProviderCallId;
use ditto_protocol::{EventActor, EventRecord, event_kind};
use ditto_web_fetch::{self as web, FetchArguments, FetchError, FetchedPage};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Fetches one turn may start, at most one per granted URL.
pub(crate) const MAX_FETCH_CALLS: usize = 3;

/// The URLs a turn may fetch: those written in the user's own message.
pub(crate) fn grant(text: &str) -> Vec<String> {
    web::user_urls(text)
}

pub(crate) fn call_budget(grant: &[String]) -> u32 {
    grant.len().min(MAX_FETCH_CALLS) as u32
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FetchToolRequested {
    pub event_version: u16,
    pub turn_id: String,
    pub request_index: u8,
    pub call_id: ProviderCallId,
    pub arguments: Value,
    pub normalized: Option<FetchArguments>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FetchToolStarted {
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
    pub normalized: FetchArguments,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FetchToolError {
    InvalidArguments,
    PermissionDenied,
    LeaseExhausted,
    LeaseExpired,
    Cancelled,
    Deadline,
    BlockedAddress,
    Resolution,
    Connection,
    Timeout,
    TooManyRedirects,
    HttpStatus,
    UnsupportedContentType,
    InvalidUrl,
}

impl FetchToolError {
    /// Errors a started fetch may end with.
    const fn after_start(self) -> bool {
        !matches!(
            self,
            Self::InvalidArguments
                | Self::PermissionDenied
                | Self::LeaseExhausted
                | Self::LeaseExpired
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum FetchToolResult {
    Fetched {
        page: FetchedPage,
    },
    Error {
        code: FetchToolError,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<u16>,
    },
}

impl FetchToolResult {
    pub(crate) fn from_fetch(result: Result<FetchedPage, FetchError>) -> Self {
        let (code, status) = match result {
            Ok(page) => return Self::Fetched { page },
            Err(FetchError::HttpStatus { status }) => (FetchToolError::HttpStatus, Some(status)),
            Err(FetchError::Cancelled) => (FetchToolError::Cancelled, None),
            Err(FetchError::Timeout) => (FetchToolError::Timeout, None),
            Err(FetchError::BlockedAddress) => (FetchToolError::BlockedAddress, None),
            Err(FetchError::Resolution) => (FetchToolError::Resolution, None),
            Err(FetchError::TooManyRedirects) => (FetchToolError::TooManyRedirects, None),
            Err(FetchError::UnsupportedContentType) => {
                (FetchToolError::UnsupportedContentType, None)
            }
            Err(FetchError::InvalidUrl) => (FetchToolError::InvalidUrl, None),
            Err(FetchError::Connection | FetchError::Unauthorized) => {
                (FetchToolError::Connection, None)
            }
        };
        Self::Error { code, status }
    }

    /// What the model reads. Page text is labeled as untrusted web content.
    pub(crate) fn model_value(&self) -> Value {
        match self {
            Self::Fetched { page } => {
                let mut value = json!(page);
                value["content_origin"] = json!("untrusted web page");
                value
            }
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
pub struct FetchToolOutput {
    pub event_version: u16,
    pub turn_id: String,
    pub request_index: u8,
    pub call_id: ProviderCallId,
    pub claimed: bool,
    pub result: FetchToolResult,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayedFetchCall {
    pub requested: FetchToolRequested,
    pub started: Option<FetchToolStarted>,
    pub output: Option<FetchToolOutput>,
}

/// The replay-side normalization: the raw schema, then the canonical URL.
pub(crate) fn normalize_call(arguments: &Value) -> Option<FetchArguments> {
    ditto_capability::validate_invocation_instance(&web::schema().input_schema, arguments).ok()?;
    let arguments: FetchArguments = serde_json::from_value(arguments.clone()).ok()?;
    Some(FetchArguments {
        url: web::canonical_url(&arguments.url).ok()?,
    })
}

fn same_scope(event: &EventRecord, input: &EventRecord) -> bool {
    event.session_id == input.session_id
        && event.task_id == input.task_id
        && event.correlation_id == input.correlation_id
        && event.seq > input.seq
}

pub(crate) fn valid_requested(
    request: &FetchToolRequested,
    event: &EventRecord,
    input: &EventRecord,
) -> bool {
    request.event_version == 1
        && request.turn_id == input.correlation_id.as_deref().unwrap_or_default()
        && request.request_index < 7
        && request.normalized == normalize_call(&request.arguments)
        && event.kind == event_kind::AGENT_FETCH_REQUESTED
        && event.actor == EventActor::Model
        && event.span_id.as_deref() == Some(request.call_id.as_str())
        && same_scope(event, input)
}

pub(crate) fn valid_started(
    start: &FetchToolStarted,
    event: &EventRecord,
    request: &FetchToolRequested,
    requested: &EventRecord,
    input: &EventRecord,
    grant: &[String],
) -> bool {
    let digest = &start.invocation_digest;
    start.event_version == 1
        && start.turn_id == request.turn_id
        && start.request_index == request.request_index
        && start.call_id == request.call_id
        && request.normalized.as_ref() == Some(&start.normalized)
        && grant.contains(&start.normalized.url)
        && event.kind == event_kind::AGENT_FETCH_STARTED
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

/// Structural evidence. Replay performs no network I/O; the recorded page is
/// the fetch result.
#[allow(clippy::too_many_arguments)]
pub(crate) fn valid_output(
    output: &FetchToolOutput,
    event: &EventRecord,
    input: &EventRecord,
    grant: &[String],
    request: &FetchToolRequested,
    requested: &EventRecord,
    started: Option<&EventRecord>,
    claims_used: u32,
    deadline: DateTime<Utc>,
) -> bool {
    let cause = started.unwrap_or(requested);
    if output.event_version != 1
        || output.turn_id != request.turn_id
        || output.call_id != request.call_id
        || output.request_index != request.request_index
        || output.claimed != started.is_some()
        || event.actor != EventActor::Capability
        || event.kind != event_kind::AGENT_FETCH_OUTPUT
        || event.span_id.as_deref() != Some(output.call_id.as_str())
        || event.causation_id.as_deref() != Some(&cause.event_id)
        || event.seq <= cause.seq
        || !same_scope(event, input)
    {
        return false;
    }
    match &output.result {
        FetchToolResult::Fetched { page } => {
            started.is_some()
                && page.is_bounded()
                && request
                    .normalized
                    .as_ref()
                    .is_some_and(|arguments| arguments.url == page.url)
                && web::canonical_url(&page.final_url).as_deref() == Ok(page.final_url.as_str())
        }
        FetchToolResult::Error { code, status } => {
            if status.is_some() != (*code == FetchToolError::HttpStatus)
                || status.is_some_and(|status| !(100..600).contains(&status))
            {
                return false;
            }
            if started.is_some() {
                return code.after_start();
            }
            match request.normalized.as_ref() {
                None => *code == FetchToolError::InvalidArguments,
                Some(arguments) if !grant.contains(&arguments.url) => {
                    *code == FetchToolError::PermissionDenied
                }
                Some(_) => {
                    (claims_used >= call_budget(grant) && *code == FetchToolError::LeaseExhausted)
                        || (*code == FetchToolError::LeaseExpired && event.recorded_at >= deadline)
                }
            }
        }
    }
}

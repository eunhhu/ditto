//! `web.browse` inside a turn (ADR 0036): read a page linked in the user's own
//! message (ADR 0027), or search at the operator's endpoint (ADR 0033). Each
//! kind has its own lease; replay checks the recorded result without network
//! I/O.
use ditto_web_fetch::{
    self as web, FetchedPage, WebError, WebOutput, WebRequest,
    search::{MAX_SEARCHES, SearchHit, SearchResults, query_of},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::tool::ToolStarted;

/// Pages one turn may read, at most one per granted URL.
pub(crate) const MAX_READS: usize = 3;
/// The leases that bound a turn's reads and searches.
pub(crate) const READ_LEASE: &str = "agent-web-read";
pub(crate) const SEARCH_LEASE: &str = "agent-web-search";

/// The URLs a turn may read: those written in the user's own message.
pub(crate) fn grant(text: &str) -> Vec<String> {
    web::user_urls(text)
}

pub(crate) fn read_budget(grant: &[String]) -> u32 {
    grant.len().min(MAX_READS) as u32
}

/// The lease a request is authorized under, and how many calls it allows.
pub(crate) fn lease(request: &WebRequest, grant: &[String]) -> (&'static str, u32) {
    match request {
        WebRequest::Read { .. } => (READ_LEASE, read_budget(grant)),
        WebRequest::Search { .. } => (SEARCH_LEASE, MAX_SEARCHES),
    }
}

/// Replay's normalization under the schema the turn offered.
pub(crate) fn normalize_call(arguments: &Value, read: bool, search: bool) -> Option<WebRequest> {
    ditto_capability::validate_invocation_instance(
        &web::schema(read, search).input_schema,
        arguments,
    )
    .ok()?;
    WebRequest::from_arguments(arguments, read, search)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebToolError {
    InvalidArguments,
    /// The URL is not one the user sent in this message.
    PermissionDenied,
    /// The query looks like it holds a secret, so it is never sent.
    Credential,
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
    InvalidResponse,
}

impl WebToolError {
    /// Errors a started call may end with.
    const fn after_start(self) -> bool {
        !matches!(
            self,
            Self::InvalidArguments
                | Self::PermissionDenied
                | Self::Credential
                | Self::LeaseExhausted
                | Self::LeaseExpired
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum WebResult {
    Read {
        page: FetchedPage,
    },
    Found {
        results: Vec<SearchHit>,
    },
    Error {
        code: WebToolError,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<u16>,
    },
}

impl WebResult {
    pub(crate) fn error(code: WebToolError) -> Self {
        Self::Error { code, status: None }
    }

    pub(crate) fn from_output(result: Result<WebOutput, WebError>) -> Self {
        let code = match result {
            Ok(WebOutput::Page(page)) => return Self::Read { page },
            Ok(WebOutput::Results(found)) => {
                return Self::Found {
                    results: found.results,
                };
            }
            Err(WebError::HttpStatus { status }) => {
                return Self::Error {
                    code: WebToolError::HttpStatus,
                    status: Some(status),
                };
            }
            Err(WebError::Cancelled) => WebToolError::Cancelled,
            Err(WebError::Timeout) => WebToolError::Timeout,
            Err(WebError::BlockedAddress) => WebToolError::BlockedAddress,
            Err(WebError::Resolution) => WebToolError::Resolution,
            Err(WebError::TooManyRedirects) => WebToolError::TooManyRedirects,
            Err(WebError::UnsupportedContentType) => WebToolError::UnsupportedContentType,
            Err(WebError::InvalidUrl) => WebToolError::InvalidUrl,
            Err(WebError::InvalidResponse) => WebToolError::InvalidResponse,
            Err(WebError::Connection | WebError::Unauthorized) => WebToolError::Connection,
        };
        Self::error(code)
    }

    /// What the model reads. Pages and results are labeled as untrusted
    /// content written by others.
    pub(crate) fn model_value(&self) -> Value {
        match self {
            Self::Read { page } => {
                let mut value = json!(page);
                value["content_origin"] = json!("untrusted web page");
                value
            }
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

/// The refusal a call gets before it is authorized, if any: an invalid
/// call, a URL the user did not send, or a query that looks like a secret.
pub(crate) fn refusal(request: Option<&WebRequest>, grant: &[String]) -> Option<WebToolError> {
    match request {
        None => Some(WebToolError::InvalidArguments),
        Some(WebRequest::Read { url }) if !grant.contains(url) => {
            Some(WebToolError::PermissionDenied)
        }
        Some(WebRequest::Search { query }) if super::memory::credential_like(query) => {
            Some(WebToolError::Credential)
        }
        Some(_) => None,
    }
}

/// A start may only follow an allowed request with calls left on its lease,
/// and a search records exactly what its query became.
pub(crate) fn valid_start(
    start: &ToolStarted,
    request: &WebRequest,
    grant: &[String],
    used: u32,
) -> bool {
    refusal(Some(request), grant).is_none()
        && used < lease(request, grant).1
        && match request {
            WebRequest::Read { .. } => start.request_url.is_none(),
            WebRequest::Search { query } => start
                .request_url
                .as_deref()
                .and_then(query_of)
                .is_some_and(|recorded| &recorded == query),
        }
}

/// Replay's check of a recorded result: the page or results a started call
/// returned, or the error its rules allow. `used` counts the started calls
/// of the request's kind before it.
pub(crate) fn valid_result(
    result: &WebResult,
    request: Option<&WebRequest>,
    started: bool,
    grant: &[String],
    used: u32,
    after_deadline: bool,
) -> bool {
    match (result, request) {
        (WebResult::Read { page }, Some(WebRequest::Read { url })) => {
            started
                && page.is_bounded()
                && &page.url == url
                && web::canonical_url(&page.final_url).as_deref() == Ok(page.final_url.as_str())
        }
        (WebResult::Found { results }, Some(WebRequest::Search { .. })) => {
            started
                && SearchResults {
                    results: results.clone(),
                }
                .is_bounded()
        }
        (WebResult::Error { code, status }, request) => {
            if status.is_some() != (*code == WebToolError::HttpStatus)
                || status.is_some_and(|status| !(100..600).contains(&status))
            {
                return false;
            }
            if started {
                return code.after_start();
            }
            match (refusal(request, grant), request) {
                (Some(refused), _) => *code == refused,
                (None, Some(request)) => {
                    (used >= lease(request, grant).1 && *code == WebToolError::LeaseExhausted)
                        || (*code == WebToolError::LeaseExpired && after_deadline)
                }
                (None, None) => false,
            }
        }
        _ => false,
    }
}

//! `web.browse` inside a turn (ADR 0036): read a page linked in the user's own
//! message (ADR 0027) or found by one of the turn's searches (ADR 0037), or
//! search at the operator's endpoint (ADR 0033). Each kind has its own lease;
//! replay checks the recorded result without network I/O.
use ditto_web_fetch::{
    self as web, FetchedPage, WebError, WebOutput, WebRequest,
    search::{MAX_SEARCHES, SearchHit, SearchResults, query_of},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::tool::ToolStarted;

/// Pages one turn may read.
pub(crate) const MAX_READS: u32 = 3;
/// The leases that bound a turn's reads and searches.
pub(crate) const READ_LEASE: &str = "agent-web-read";
pub(crate) const SEARCH_LEASE: &str = "agent-web-search";

/// The pages a turn may read: the URLs written in the user's own message,
/// then those its searches return, and how many reads its lease allows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct WebGrant {
    urls: Vec<String>,
    reads: u32,
}

impl WebGrant {
    /// The grant a turn starts with while reading is enabled. With a search
    /// service, three reads are shared by the user's links and the pages
    /// searches find; otherwise one per link, at most three.
    pub(crate) fn new(text: &str, search: bool) -> Self {
        let urls = web::user_urls(text);
        let reads = if search {
            MAX_READS
        } else {
            (urls.len() as u32).min(MAX_READS)
        };
        Self { urls, reads }
    }

    pub(crate) fn urls(&self) -> &[String] {
        &self.urls
    }

    /// Calls the read lease allows; none while reading is disabled.
    pub(crate) const fn reads(&self) -> u32 {
        self.reads
    }

    /// Grant the pages a search returned, in its order, and return those
    /// newly granted. A grant without reads takes none.
    pub(crate) fn add_found(&mut self, results: &[SearchHit]) -> Vec<String> {
        let mut added = Vec::new();
        if self.reads == 0 {
            return added;
        }
        for hit in results {
            if !self.urls.contains(&hit.url) {
                self.urls.push(hit.url.clone());
                added.push(hit.url.clone());
            }
        }
        added
    }
}

/// The lease a request is authorized under, and how many calls it allows.
pub(crate) fn lease(request: &WebRequest, grant: &WebGrant) -> (&'static str, u32) {
    match request {
        WebRequest::Read { .. } => (READ_LEASE, grant.reads),
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
    /// The URL is neither one the user sent in this message nor one of the
    /// turn's search results.
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
/// call, a URL that is not granted, or a query that looks like a secret.
pub(crate) fn refusal(request: Option<&WebRequest>, grant: &WebGrant) -> Option<WebToolError> {
    match request {
        None => Some(WebToolError::InvalidArguments),
        Some(WebRequest::Read { url }) if !grant.urls.contains(url) => {
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
    grant: &WebGrant,
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
    grant: &WebGrant,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(url: &str) -> SearchHit {
        SearchHit {
            title: String::new(),
            url: url.into(),
            snippet: String::new(),
        }
    }

    #[test]
    fn found_pages_are_granted_once_returned_and_only_while_reading() {
        let link = "https://linked.example/";
        let page = "https://found.example/a";
        let read = WebRequest::Read { url: page.into() };
        let denied = WebResult::error(WebToolError::PermissionDenied);
        let mut grant = WebGrant::new(&format!("see {link}"), true);
        assert_eq!(grant.reads(), MAX_READS);
        // Before a search returns the page, a denial is its only record...
        assert_eq!(
            refusal(Some(&read), &grant),
            Some(WebToolError::PermissionDenied)
        );
        assert!(valid_result(&denied, Some(&read), false, &grant, 0, false));
        assert_eq!(grant.add_found(&[hit(page), hit(link), hit(page)]), [page]);
        assert_eq!(grant.urls(), [link, page]);
        // ...and once it has, a denial contradicts the grant.
        assert_eq!(refusal(Some(&read), &grant), None);
        assert!(!valid_result(&denied, Some(&read), false, &grant, 0, false));
        // Without a search service each link is one read; with reading
        // disabled nothing is granted.
        assert_eq!(WebGrant::new(&format!("see {link}"), false).reads(), 1);
        let mut disabled = WebGrant::default();
        assert!(disabled.add_found(&[hit(page)]).is_empty());
        assert_eq!(
            refusal(Some(&read), &disabled),
            Some(WebToolError::PermissionDenied)
        );
    }
}

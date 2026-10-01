//! Searching through the operator's SearXNG-compatible endpoint, the
//! `query` side of `web.browse` (ADRs 0033 and 0036). The endpoint is
//! configuration, never the model's choice, and it is the resource every
//! search is authorized for, so a query cannot be sent anywhere else.
use std::time::Duration;

use ditto_capability::CanonicalResource;
use ditto_model::CancellationToken;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{FetchPolicy, UrlError, WebError, canonical_url, get_within};

pub const MAX_QUERY_CHARS: usize = 200;
/// Results one search returns at most.
pub const MAX_RESULTS: usize = 5;
pub const MAX_TITLE_CHARS: usize = 200;
pub const MAX_SNIPPET_CHARS: usize = 400;
/// Searches one turn may run.
pub const MAX_SEARCHES: u32 = 3;

/// The search service of a deployment, such as `http://127.0.0.1:8888`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchEndpoint(Url);

impl SearchEndpoint {
    /// An http(s) base URL without credentials, query or fragment.
    pub fn new(base: &str) -> Result<Self, UrlError> {
        let mut url = Url::parse(&canonical_url(base)?).map_err(|_| UrlError::Invalid)?;
        if url.query().is_some() {
            return Err(UrlError::Invalid);
        }
        let path = url.path().trim_end_matches('/').to_owned();
        url.set_path(&path);
        Ok(Self(url))
    }

    /// The exact request a query becomes: `<base>/search?q=<query>&format=json`.
    /// Built from a canonical endpoint, it is canonical too, whatever the
    /// query's length.
    pub fn request_url(&self, query: &str) -> String {
        let mut url = self.0.clone();
        let path = format!("{}/search", url.path().trim_end_matches('/'));
        url.set_path(&path);
        url.query_pairs_mut()
            .clear()
            .append_pair("q", query)
            .append_pair("format", "json");
        url.into()
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// The resource every search at this endpoint is authorized for.
    pub fn resource(&self) -> Result<CanonicalResource, UrlError> {
        CanonicalResource::url(self.0.as_str()).map_err(|_| UrlError::Invalid)
    }
}

/// The query a recorded request URL carries, when the URL is exactly what
/// that query becomes at some endpoint: replay's check of a recorded search.
pub fn query_of(request_url: &str) -> Option<String> {
    let url = Url::parse(request_url).ok()?;
    let pairs = url.query_pairs().into_owned().collect::<Vec<_>>();
    let [(q, query), (format, json)] = pairs.as_slice() else {
        return None;
    };
    if q != "q" || format != "format" || json != "json" {
        return None;
    }
    let mut base = url.clone();
    base.set_query(None);
    base.set_path(url.path().strip_suffix("/search")?);
    let endpoint = SearchEndpoint::new(base.as_str()).ok()?;
    (endpoint.request_url(query) == request_url).then(|| query.clone())
}

/// One result, as returned to the model and journaled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchHit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchResults {
    pub results: Vec<SearchHit>,
}

impl SearchResults {
    /// The results satisfy the bounds a recorded result must meet.
    pub fn is_bounded(&self) -> bool {
        self.results.len() <= MAX_RESULTS
            && self.results.iter().all(|hit| {
                hit.title.chars().count() <= MAX_TITLE_CHARS
                    && hit.snippet.chars().count() <= MAX_SNIPPET_CHARS
                    && canonical_url(&hit.url).is_ok_and(|url| url == hit.url)
            })
    }

    /// The bounded results of a SearXNG JSON body: http(s) links only, in
    /// the service's order.
    pub fn from_searxng(body: &Value) -> Option<Self> {
        let results = body
            .get("results")?
            .as_array()?
            .iter()
            .filter_map(|result| {
                let url = canonical_url(result.get("url")?.as_str()?).ok()?;
                let text = |field: &str, limit: usize| {
                    let text = result
                        .get(field)
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ");
                    text.chars().take(limit).collect::<String>()
                };
                Some(SearchHit {
                    title: text("title", MAX_TITLE_CHARS),
                    url,
                    snippet: text("content", MAX_SNIPPET_CHARS),
                })
            })
            .take(MAX_RESULTS)
            .collect();
        Some(Self { results })
    }
}

/// GET a search request URL and read its results. The operator chose the
/// endpoint, so it may be on the local network.
pub async fn search(
    request_url: &str,
    cancellation: &CancellationToken,
    deadline: Duration,
) -> Result<SearchResults, WebError> {
    let response = get_within(
        request_url,
        FetchPolicy::allow_private_addresses(),
        "application/json",
        cancellation,
        deadline,
    )
    .await?;
    if response.over {
        return Err(WebError::InvalidResponse);
    }
    let body: Value =
        serde_json::from_slice(&response.body).map_err(|_| WebError::InvalidResponse)?;
    SearchResults::from_searxng(&body).ok_or(WebError::InvalidResponse)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_urls_round_trip_and_nothing_else_does() {
        let endpoint = SearchEndpoint::new("http://127.0.0.1:8888/").unwrap();
        let url = endpoint.request_url("rust 1.88 release & notes");
        assert_eq!(
            url,
            "http://127.0.0.1:8888/search?q=rust+1.88+release+%26+notes&format=json"
        );
        assert_eq!(query_of(&url).as_deref(), Some("rust 1.88 release & notes"));
        let nested = SearchEndpoint::new("https://example.org/searx").unwrap();
        let url = nested.request_url("날씨");
        assert_eq!(query_of(&url).as_deref(), Some("날씨"));
        for other in [
            "http://127.0.0.1:8888/search?format=json&q=x",
            "http://127.0.0.1:8888/search?q=x&format=html",
            "http://127.0.0.1:8888/find?q=x&format=json",
            "http://127.0.0.1:8888/search?q=x&format=json&page=2",
        ] {
            assert_eq!(query_of(other), None, "{other}");
        }
        assert!(SearchEndpoint::new("http://127.0.0.1:8888/?x=1").is_err());
        assert!(SearchEndpoint::new("ftp://example.org").is_err());
    }

    #[test]
    fn searxng_results_are_bounded_links_in_order() {
        let body = json!({"results": [
            {"url": "https://a.example/x", "title": "A", "content": "  first   result "},
            {"url": "javascript:alert(1)", "title": "bad"},
            {"url": "https://b.example/", "title": "B".repeat(500), "content": "c".repeat(900)},
            {"url": "https://c.example/"}, {"url": "https://d.example/"},
            {"url": "https://e.example/"}, {"url": "https://f.example/"}
        ]});
        let results = SearchResults::from_searxng(&body).unwrap();
        assert!(results.is_bounded());
        assert_eq!(results.results.len(), MAX_RESULTS);
        assert_eq!(results.results[0].snippet, "first result");
        assert_eq!(results.results[1].url, "https://b.example/");
        assert_eq!(results.results[1].title.chars().count(), MAX_TITLE_CHARS);
        assert_eq!(results.results[4].url, "https://e.example/");
        assert_eq!(SearchResults::from_searxng(&json!({"answers": []})), None);
    }
}

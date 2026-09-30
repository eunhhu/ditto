//! `web.fetch`: read the text of a web page the user linked (ADR 0027).
//!
//! Only URLs that appear in the user's own message may be fetched, so a model
//! cannot choose a destination. Every hop of a fetch resolves to public
//! addresses only and connects to exactly the addresses it checked.
use std::{collections::BTreeSet, net::SocketAddr, time::Duration};

use ditto_capability::{
    CanonicalInvocation, CanonicalResource, CapabilityDeriver, CapabilityManifest,
    CapabilityRevision, CapabilitySchema, DataAccess, DerivationBudget, DeriverError,
    DeriverRevision, EffectProfile, Externality, Mutation, Privilege, ResolvedPlacement,
    canonical_manifest_digest,
};
use ditto_model::CancellationToken;
use ditto_policy::ExecutionClaim;
use futures_util::StreamExt;
use reqwest::{Url, header};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

mod address;
mod html;

pub use address::is_public;
pub use html::extract as extract_html;

pub const ID: &str = "web.fetch";
pub const VERSION: &str = "0.1.0";
/// URLs taken from one user message; later ones are ignored.
pub const MAX_USER_URLS: usize = 5;
pub const MAX_URL_BYTES: usize = 2_048;
/// Bytes read from one response body; the rest is not downloaded.
pub const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;
/// Characters of extracted text returned to the model.
pub const MAX_TEXT_CHARS: usize = 24_000;
pub const MAX_REDIRECTS: usize = 5;
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(20);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const USER_AGENT: &str = "Ditto/0.1 (personal assistant; +https://github.com/eunhhu/ditto)";

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum UrlError {
    #[error("not an absolute http or https URL")]
    Invalid,
    #[error("URL carries credentials")]
    Credentials,
    #[error("URL exceeds its length bound")]
    TooLong,
}

/// The one canonical spelling of a fetchable URL: http(s), a host, no
/// credentials, no fragment, at most 2 KiB.
pub fn canonical_url(input: &str) -> Result<String, UrlError> {
    let mut url = Url::parse(input.trim()).map_err(|_| UrlError::Invalid)?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none_or(str::is_empty) {
        return Err(UrlError::Invalid);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(UrlError::Credentials);
    }
    url.set_fragment(None);
    let canonical = url.to_string();
    if canonical.len() > MAX_URL_BYTES {
        return Err(UrlError::TooLong);
    }
    Ok(canonical)
}

/// The canonical http(s) URLs written in a user's message, in order, without
/// duplicates, at most [`MAX_USER_URLS`]. A URL ends at whitespace, a quote,
/// an angle bracket or a non-ASCII character; trailing punctuation and
/// unbalanced closing brackets are not part of it.
pub fn user_urls(text: &str) -> Vec<String> {
    let lower = text.to_ascii_lowercase();
    let mut urls = Vec::new();
    let mut from = 0;
    while urls.len() < MAX_USER_URLS {
        let Some(start) = ["http://", "https://"]
            .iter()
            .filter_map(|scheme| lower[from..].find(scheme).map(|index| from + index))
            .min()
        else {
            break;
        };
        let length = text[start..]
            .find(|c: char| {
                !c.is_ascii()
                    || c.is_ascii_whitespace()
                    || matches!(c, '"' | '\'' | '<' | '>' | '`')
            })
            .unwrap_or(text.len() - start);
        let mut candidate = &text[start..start + length];
        from = start + length.max(1);
        loop {
            let trimmed = candidate.trim_end_matches(['.', ',', ';', ':', '!', '?']);
            let trimmed = [(')', '('), (']', '['), ('}', '{')]
                .iter()
                .find(|(close, open)| {
                    trimmed.ends_with(*close)
                        && trimmed.matches(*close).count() > trimmed.matches(*open).count()
                })
                .map_or(trimmed, |_| &trimmed[..trimmed.len() - 1]);
            if trimmed.len() == candidate.len() {
                break;
            }
            candidate = trimmed;
        }
        if let Ok(url) = canonical_url(candidate)
            && !urls.contains(&url)
        {
            urls.push(url);
        }
    }
    urls
}

pub fn effect() -> EffectProfile {
    EffectProfile {
        access: DataAccess::Content,
        mutation: Mutation::None,
        externality: Externality::Network,
        privilege: Privilege::User,
    }
}

pub fn manifest() -> CapabilityManifest {
    toml::from_str(include_str!(
        "../../../capabilities/core/web-fetch/capability.toml"
    ))
    .expect("packaged web.fetch manifest is valid")
}

/// The installed package must be exactly the one this code implements.
pub fn validate_manifest(installed: &CapabilityManifest) -> bool {
    canonical_manifest_digest(installed) == canonical_manifest_digest(&manifest())
}

pub fn schema() -> CapabilitySchema {
    CapabilitySchema {
        id: ID.into(),
        version: VERSION.into(),
        summary: manifest().summary,
        input_schema: json!({
            "type": "object", "additionalProperties": false, "required": ["url"],
            "properties": {
                "url": {"type": "string", "minLength": 10, "maxLength": MAX_URL_BYTES,
                        "pattern": "^[Hh][Tt][Tt][Pp][Ss]?://"}
            }
        }),
        output_schema: json!({
            "type": "object", "additionalProperties": false,
            "required": ["url", "final_url", "status", "content_type", "text", "truncated"],
            "properties": {
                "url": {"type": "string", "maxLength": MAX_URL_BYTES},
                "final_url": {"type": "string", "maxLength": MAX_URL_BYTES},
                "status": {"type": "integer", "minimum": 200, "maximum": 299},
                "content_type": {"type": "string", "maxLength": 256},
                "title": {"type": "string", "maxLength": 1_024},
                "text": {"type": "string", "maxLength": MAX_TEXT_CHARS * 4},
                "truncated": {"type": "boolean"}
            }
        }),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FetchArguments {
    pub url: String,
}

pub struct FetchDeriver(DeriverRevision);

impl Default for FetchDeriver {
    fn default() -> Self {
        Self(DeriverRevision::new("web-fetch-v1").expect("static deriver revision is valid"))
    }
}

impl CapabilityDeriver for FetchDeriver {
    fn capability_id(&self) -> &str {
        ID
    }
    fn revision(&self) -> &DeriverRevision {
        &self.0
    }
    fn normalize(
        &self,
        arguments: &Value,
        budget: &mut DerivationBudget,
    ) -> Result<Value, DeriverError> {
        budget.charge(1)?;
        let arguments: FetchArguments = serde_json::from_value(arguments.clone())
            .map_err(|_| DeriverError::new("invalid web.fetch arguments"))?;
        let url =
            canonical_url(&arguments.url).map_err(|error| DeriverError::new(error.to_string()))?;
        Ok(json!(FetchArguments { url }))
    }
    fn derive_effect(
        &self,
        _: &Value,
        budget: &mut DerivationBudget,
    ) -> Result<EffectProfile, DeriverError> {
        budget.charge(1)?;
        Ok(effect())
    }
    fn derive_resources(
        &self,
        arguments: &Value,
        budget: &mut DerivationBudget,
    ) -> Result<BTreeSet<CanonicalResource>, DeriverError> {
        budget.charge(1)?;
        let arguments: FetchArguments = serde_json::from_value(arguments.clone())
            .map_err(|_| DeriverError::new("invalid web.fetch arguments"))?;
        let resource = CanonicalResource::url(arguments.url)
            .map_err(|_| DeriverError::new("invalid URL resource"))?;
        Ok(BTreeSet::from([resource]))
    }
}

/// Which addresses a fetch may reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FetchPolicy {
    allow_private_addresses: bool,
}

impl FetchPolicy {
    /// Production: globally routable addresses only.
    pub const fn public_only() -> Self {
        Self {
            allow_private_addresses: false,
        }
    }

    /// Tests and explicitly configured local deployments: loopback and
    /// private networks are reachable too.
    pub const fn allow_private_addresses() -> Self {
        Self {
            allow_private_addresses: true,
        }
    }
}

impl Default for FetchPolicy {
    fn default() -> Self {
        Self::public_only()
    }
}

/// Text of one fetched page, as returned to the model and journaled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FetchedPage {
    pub url: String,
    pub final_url: String,
    pub status: u16,
    pub content_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub text: String,
    pub truncated: bool,
}

impl FetchedPage {
    /// The page satisfies the output bounds a recorded result must meet.
    pub fn is_bounded(&self) -> bool {
        self.url.len() <= MAX_URL_BYTES
            && self.final_url.len() <= MAX_URL_BYTES
            && (200..300).contains(&self.status)
            && self.content_type.len() <= 256
            && self
                .title
                .as_ref()
                .is_none_or(|title| title.chars().count() <= 1_024)
            && self.text.chars().count() <= MAX_TEXT_CHARS + 1
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "error", rename_all = "snake_case")]
pub enum FetchError {
    #[error("the invocation or execution claim is invalid")]
    Unauthorized,
    #[error("URL is not fetchable")]
    InvalidUrl,
    #[error("the URL resolves to a private or reserved address")]
    BlockedAddress,
    #[error("the host could not be resolved")]
    Resolution,
    #[error("the connection failed")]
    Connection,
    #[error("the fetch timed out")]
    Timeout,
    #[error("the fetch was cancelled")]
    Cancelled,
    #[error("too many redirects")]
    TooManyRedirects,
    #[error("the server answered with HTTP status {status}")]
    HttpStatus { status: u16 },
    #[error("the content type is not text")]
    UnsupportedContentType,
}

/// Execute one authorized `web.fetch` invocation. The claim is consumed here:
/// it must match the invocation, whose contract, placement, effect and
/// resource must be exactly this capability's.
pub async fn execute(
    invocation: CanonicalInvocation,
    claim: ExecutionClaim,
    policy: FetchPolicy,
    cancellation: CancellationToken,
) -> Result<FetchedPage, FetchError> {
    let now = chrono::Utc::now();
    claim
        .validate(&invocation, now)
        .map_err(|_| FetchError::Unauthorized)?;
    let revision =
        CapabilityRevision::from_contract(&manifest(), &schema(), FetchDeriver::default().0)
            .map_err(|_| FetchError::Unauthorized)?;
    let arguments: FetchArguments =
        serde_json::from_value(invocation.normalized_arguments().clone())
            .map_err(|_| FetchError::Unauthorized)?;
    let resource = CanonicalResource::url(&arguments.url).map_err(|_| FetchError::Unauthorized)?;
    if invocation.capability_revision() != &revision
        || invocation.placement() != ResolvedPlacement::LocalBuiltin
        || invocation.effect() != effect()
        || invocation.resources() != &BTreeSet::from([resource])
    {
        return Err(FetchError::Unauthorized);
    }
    let remaining = (claim.expires_at() - now)
        .to_std()
        .map_err(|_| FetchError::Timeout)?;
    fetch(&arguments.url, policy, &cancellation, remaining).await
}

/// Fetch `url` with GET and return its readable text. Stops at the first of
/// cancellation, the fetch timeout or `deadline`.
pub async fn fetch(
    url: &str,
    policy: FetchPolicy,
    cancellation: &CancellationToken,
    deadline: Duration,
) -> Result<FetchedPage, FetchError> {
    let requested = canonical_url(url).map_err(|_| FetchError::InvalidUrl)?;
    let limit = deadline.min(FETCH_TIMEOUT);
    tokio::select! {
        biased;
        () = cancellation.cancelled() => Err(FetchError::Cancelled),
        result = tokio::time::timeout(limit, follow(requested, policy)) => {
            result.unwrap_or(Err(FetchError::Timeout))
        }
    }
}

async fn follow(requested: String, policy: FetchPolicy) -> Result<FetchedPage, FetchError> {
    let mut current = Url::parse(&requested).map_err(|_| FetchError::InvalidUrl)?;
    for _ in 0..=MAX_REDIRECTS {
        let client = client_for(&current, policy).await?;
        let response = client
            .get(current.clone())
            .header(
                header::ACCEPT,
                "text/html,application/xhtml+xml,text/plain;q=0.9,application/json;q=0.8,*/*;q=0.1",
            )
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    FetchError::Timeout
                } else {
                    FetchError::Connection
                }
            })?;
        let status = response.status();
        if status.is_redirection() {
            let location = response
                .headers()
                .get(header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or(FetchError::Connection)?;
            let next = current.join(location).map_err(|_| FetchError::InvalidUrl)?;
            current =
                Url::parse(&canonical_url(next.as_str()).map_err(|_| FetchError::InvalidUrl)?)
                    .map_err(|_| FetchError::InvalidUrl)?;
            continue;
        }
        if !status.is_success() {
            return Err(FetchError::HttpStatus {
                status: status.as_u16(),
            });
        }
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let (body, over) = read_bounded(response).await?;
        let body = String::from_utf8_lossy(&body);
        let media = content_type
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_owned();
        let html = match media.as_str() {
            "text/html" | "application/xhtml+xml" => true,
            "" => body.trim_start().starts_with('<'),
            media
                if media.starts_with("text/")
                    || matches!(
                        media,
                        "application/json"
                            | "application/xml"
                            | "application/rss+xml"
                            | "application/atom+xml"
                            | "application/ld+json"
                    ) =>
            {
                false
            }
            _ => return Err(FetchError::UnsupportedContentType),
        };
        let (title, text) = if html {
            html::extract(&body)
        } else {
            (None, body.trim().to_owned())
        };
        let (text, cut) = bounded_chars(&text, MAX_TEXT_CHARS);
        return Ok(FetchedPage {
            url: requested,
            final_url: current.to_string(),
            status: status.as_u16(),
            content_type: media.chars().take(256).collect(),
            title: title.map(|title| title.chars().take(1_024).collect()),
            text,
            truncated: over || cut,
        });
    }
    Err(FetchError::TooManyRedirects)
}

/// A client whose only route to the URL's host is the set of addresses that
/// passed the policy, so DNS cannot change between the check and the connect.
async fn client_for(url: &Url, policy: FetchPolicy) -> Result<reqwest::Client, FetchError> {
    let host = url.host_str().ok_or(FetchError::InvalidUrl)?;
    let port = url.port_or_known_default().ok_or(FetchError::InvalidUrl)?;
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let addresses: Vec<SocketAddr> = match bare.parse::<std::net::IpAddr>() {
        Ok(ip) => vec![SocketAddr::new(ip, port)],
        Err(_) => tokio::net::lookup_host((bare, port))
            .await
            .map_err(|_| FetchError::Resolution)?
            .collect(),
    };
    if addresses.is_empty() {
        return Err(FetchError::Resolution);
    }
    if !policy.allow_private_addresses && addresses.iter().any(|address| !is_public(address.ip())) {
        return Err(FetchError::BlockedAddress);
    }
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
        .user_agent(USER_AGENT)
        .no_proxy();
    if bare.parse::<std::net::IpAddr>().is_err() {
        builder = builder.resolve_to_addrs(bare, &addresses);
    }
    builder.build().map_err(|_| FetchError::Connection)
}

async fn read_bounded(response: reqwest::Response) -> Result<(Vec<u8>, bool), FetchError> {
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| {
            if error.is_timeout() {
                FetchError::Timeout
            } else {
                FetchError::Connection
            }
        })?;
        let room = MAX_BODY_BYTES - body.len();
        if chunk.len() > room {
            body.extend_from_slice(&chunk[..room]);
            return Ok((body, true));
        }
        body.extend_from_slice(&chunk);
    }
    Ok((body, false))
}

/// At most `limit` characters, cut at a line or word break when one is near,
/// and whether anything was cut.
fn bounded_chars(text: &str, limit: usize) -> (String, bool) {
    let Some((cut, _)) = text.char_indices().nth(limit) else {
        return (text.to_owned(), false);
    };
    let window = &text[..cut];
    let end = window
        .rfind('\n')
        .or_else(|| window.rfind(' '))
        .filter(|&index| index >= cut * 3 / 4)
        .unwrap_or(cut);
    (format!("{}…", window[..end].trim_end()), true)
}

#[cfg(test)]
mod tests;

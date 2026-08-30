use std::{
    fmt,
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{Duration, SystemTime},
};

use futures_core::Stream;
use futures_util::StreamExt;
use reqwest::{
    Client,
    header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue},
    redirect::Policy,
};
use serde_json::Value;
use thiserror::Error;

use crate::{
    MAX_HTTP_ERROR_BODY_BYTES, MAX_PROVIDER_CODE_BYTES, MAX_PROVIDER_MESSAGE_BYTES,
    OPENAI_RESPONSES_URL,
};

const MAX_API_KEY_BYTES: usize = 8 * 1024;
const MAX_TENANT_ID_BYTES: usize = 512;
const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);
const REDACTED_CREDENTIAL: &str = "<redacted>";

/// A bearer credential that cannot be serialized and is always debug-redacted.
///
/// ```compile_fail
/// use ditto_model_openai::OpenAiApiKey;
/// let key = OpenAiApiKey::new("sk-example").unwrap();
/// let _ = serde_json::to_string(&key);
/// ```
pub struct OpenAiApiKey(String);

impl OpenAiApiKey {
    pub fn new(value: impl Into<String>) -> Result<Self, OpenAiConfigError> {
        let value = value.into();
        if value.is_empty() || value.len() > MAX_API_KEY_BYTES {
            return Err(OpenAiConfigError::InvalidApiKey);
        }
        if value.chars().any(char::is_whitespace) {
            return Err(OpenAiConfigError::InvalidApiKey);
        }
        Ok(Self(value))
    }

    fn authorization_value(&self) -> Result<HeaderValue, OpenAiTransportError> {
        HeaderValue::from_str(&format!("Bearer {}", self.0)).map_err(|_| {
            OpenAiTransportError::protocol("configured API key is not a valid authorization header")
        })
    }

    fn sanitize_error(&self, value: &str) -> String {
        sanitize_credential_text(value, &self.0)
    }
}

impl fmt::Debug for OpenAiApiKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenAiApiKey")
            .field("value", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum OpenAiConfigError {
    #[error("OpenAI API key must be non-empty, bounded, and contain no whitespace controls")]
    InvalidApiKey,
    #[error("{field} must be a non-empty bounded HTTP header value")]
    InvalidTenantId { field: &'static str },
    #[error("failed to construct the fixed-origin OpenAI HTTPS client: {message}")]
    HttpClient { message: String },
    #[error("OpenAI retry policy is outside its bounded attempt/delay limits")]
    InvalidRetryPolicy,
}

/// Non-secret transport headers scoped to one OpenAI organization/project.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpenAiTransportConfig {
    organization: Option<String>,
    project: Option<String>,
}

impl OpenAiTransportConfig {
    pub fn with_organization(
        mut self,
        organization: impl Into<String>,
    ) -> Result<Self, OpenAiConfigError> {
        self.organization = Some(validate_tenant_id("organization", organization.into())?);
        Ok(self)
    }

    pub fn with_project(mut self, project: impl Into<String>) -> Result<Self, OpenAiConfigError> {
        self.project = Some(validate_tenant_id("project", project.into())?);
        Ok(self)
    }
}

fn validate_tenant_id(field: &'static str, value: String) -> Result<String, OpenAiConfigError> {
    if value.is_empty()
        || value.len() > MAX_TENANT_ID_BYTES
        || value.trim() != value
        || value.chars().any(char::is_control)
        || HeaderValue::from_str(&value).is_err()
    {
        return Err(OpenAiConfigError::InvalidTenantId { field });
    }
    Ok(value)
}

/// Credential-free request passed through the injectable transport boundary.
#[derive(Clone)]
pub struct OpenAiHttpRequest {
    body: Arc<[u8]>,
}

impl OpenAiHttpRequest {
    pub(crate) fn new(body: Vec<u8>) -> Self {
        Self { body: body.into() }
    }

    pub fn body(&self) -> &[u8] {
        &self.body
    }
}

impl fmt::Debug for OpenAiHttpRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenAiHttpRequest")
            .field("body_bytes", &self.body.len())
            .finish()
    }
}

pub(crate) type OpenAiByteStream =
    Pin<Box<dyn Stream<Item = Result<Vec<u8>, OpenAiTransportError>> + Send + 'static>>;

/// A successful streaming HTTP handshake.
pub struct OpenAiHttpResponse {
    body: OpenAiByteStream,
}

impl OpenAiHttpResponse {
    pub fn new<S>(body: S) -> Self
    where
        S: Stream<Item = Result<Vec<u8>, OpenAiTransportError>> + Send + 'static,
    {
        Self {
            body: Box::pin(body),
        }
    }

    pub(crate) fn into_body(self) -> OpenAiByteStream {
        self.body
    }
}

impl fmt::Debug for OpenAiHttpResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenAiHttpResponse")
            .field("body", &"<stream>")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenAiTransportErrorKind {
    Connection,
    Timeout,
    HttpStatus(u16),
    Body,
    Protocol,
}

/// Bounded transport failure exposed to deterministic mock transports.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("{message}")]
pub struct OpenAiTransportError {
    kind: OpenAiTransportErrorKind,
    message: String,
    provider_code: Option<String>,
    retry_after: Option<Duration>,
    quota_or_billing: bool,
}

impl OpenAiTransportError {
    pub fn connection(message: impl AsRef<str>) -> Self {
        Self::new(OpenAiTransportErrorKind::Connection, message)
    }

    pub fn timeout(message: impl AsRef<str>) -> Self {
        Self::new(OpenAiTransportErrorKind::Timeout, message)
    }

    pub fn body(message: impl AsRef<str>) -> Self {
        Self::new(OpenAiTransportErrorKind::Body, message)
    }

    pub fn protocol(message: impl AsRef<str>) -> Self {
        Self::new(OpenAiTransportErrorKind::Protocol, message)
    }

    pub fn http_status(status: u16, body: &[u8], retry_after: Option<Duration>) -> Self {
        Self::http_status_with_api_key(status, body, retry_after, None)
    }

    pub(crate) fn http_status_with_api_key(
        status: u16,
        body: &[u8],
        retry_after: Option<Duration>,
        api_key: Option<&OpenAiApiKey>,
    ) -> Self {
        let bounded = &body[..body.len().min(MAX_HTTP_ERROR_BODY_BYTES)];
        let parsed = serde_json::from_slice::<Value>(bounded).ok();
        let error = parsed.as_ref().and_then(|value| value.get("error"));
        let raw_code = error
            .and_then(|value| value.get("code"))
            .and_then(Value::as_str);
        let provider_type = error
            .and_then(|value| value.get("type"))
            .and_then(Value::as_str);
        let provider_message = error
            .and_then(|value| value.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("OpenAI returned an unsuccessful HTTP status");
        let quota_or_billing = status == 402
            || raw_code.is_some_and(is_quota_or_billing_code)
            || provider_type.is_some_and(is_quota_or_billing_code);
        let sanitize = |value: &str| {
            api_key.map_or_else(
                || sanitize_credential_tokens(value),
                |api_key| api_key.sanitize_error(value),
            )
        };
        Self {
            kind: OpenAiTransportErrorKind::HttpStatus(status),
            message: bounded_string(&sanitize(provider_message), MAX_PROVIDER_MESSAGE_BYTES),
            provider_code: raw_code
                .map(sanitize)
                .map(|value| bounded_string(&value, MAX_PROVIDER_CODE_BYTES)),
            retry_after: retry_after.map(|delay| delay.min(MAX_RETRY_AFTER)),
            quota_or_billing,
        }
    }

    fn new(kind: OpenAiTransportErrorKind, message: impl AsRef<str>) -> Self {
        let message = sanitize_credential_tokens(message.as_ref());
        Self {
            kind,
            message: bounded_string(&message, MAX_PROVIDER_MESSAGE_BYTES),
            provider_code: None,
            retry_after: None,
            quota_or_billing: false,
        }
    }

    pub const fn kind(&self) -> OpenAiTransportErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn provider_code(&self) -> Option<&str> {
        self.provider_code.as_deref()
    }

    pub const fn retry_after(&self) -> Option<Duration> {
        self.retry_after
    }

    pub const fn is_retryable_before_response(&self) -> bool {
        if self.quota_or_billing {
            return false;
        }
        matches!(
            self.kind,
            OpenAiTransportErrorKind::Connection
                | OpenAiTransportErrorKind::Timeout
                | OpenAiTransportErrorKind::HttpStatus(408 | 409 | 429 | 500..=599)
        )
    }
}

fn is_quota_or_billing_code(code: &str) -> bool {
    matches!(
        code,
        "insufficient_quota"
            | "billing_hard_limit_reached"
            | "billing_not_active"
            | "billing_error"
    )
}

pub type OpenAiTransportFuture = Pin<
    Box<dyn Future<Output = Result<OpenAiHttpResponse, OpenAiTransportError>> + Send + 'static>,
>;

/// Injectable handshake boundary shared by production HTTPS and CI mocks.
pub trait OpenAiTransport: Send + Sync {
    fn send(&self, request: OpenAiHttpRequest) -> OpenAiTransportFuture;
}

/// Fixed-origin reqwest/rustls transport. Redirects are disabled at the client.
pub struct OpenAiReqwestTransport {
    client: Client,
    api_key: Arc<OpenAiApiKey>,
    config: OpenAiTransportConfig,
}

impl OpenAiReqwestTransport {
    pub fn new(
        api_key: OpenAiApiKey,
        config: OpenAiTransportConfig,
    ) -> Result<Self, OpenAiConfigError> {
        let client = Client::builder()
            .https_only(true)
            .redirect(Policy::none())
            .build()
            .map_err(|error| OpenAiConfigError::HttpClient {
                message: bounded_string(
                    &sanitize_credential_tokens(&error.to_string()),
                    MAX_PROVIDER_MESSAGE_BYTES,
                ),
            })?;
        Ok(Self {
            client,
            api_key: Arc::new(api_key),
            config,
        })
    }
}

impl fmt::Debug for OpenAiReqwestTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenAiReqwestTransport")
            .field("endpoint", &OPENAI_RESPONSES_URL)
            .field("api_key", &"<redacted>")
            .field("config", &self.config)
            .field("redirects", &"disabled")
            .finish()
    }
}

impl OpenAiTransport for OpenAiReqwestTransport {
    fn send(&self, request: OpenAiHttpRequest) -> OpenAiTransportFuture {
        let client = self.client.clone();
        let api_key = Arc::clone(&self.api_key);
        let config = self.config.clone();
        Box::pin(async move {
            let mut headers = HeaderMap::new();
            headers.insert(AUTHORIZATION, api_key.authorization_value()?);
            headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
            if let Some(organization) = config.organization {
                let value = HeaderValue::from_str(&organization).map_err(|_| {
                    OpenAiTransportError::protocol("configured organization header is invalid")
                })?;
                headers.insert("OpenAI-Organization", value);
            }
            if let Some(project) = config.project {
                let value = HeaderValue::from_str(&project).map_err(|_| {
                    OpenAiTransportError::protocol("configured project header is invalid")
                })?;
                headers.insert("OpenAI-Project", value);
            }

            let response = client
                .post(OPENAI_RESPONSES_URL)
                .headers(headers)
                .body(request.body.to_vec())
                .send()
                .await
                .map_err(|error| classify_reqwest_error(error, &api_key))?;

            let status = response.status();
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| parse_retry_after(value, SystemTime::now()));
            if !status.is_success() {
                let mut body = Vec::new();
                let mut stream = response.bytes_stream();
                while let Some(chunk) = stream.next().await {
                    let chunk = chunk.map_err(|error| classify_reqwest_error(error, &api_key))?;
                    let remaining = MAX_HTTP_ERROR_BODY_BYTES.saturating_sub(body.len());
                    body.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
                    if body.len() == MAX_HTTP_ERROR_BODY_BYTES {
                        break;
                    }
                }
                return Err(OpenAiTransportError::http_status_with_api_key(
                    status.as_u16(),
                    &body,
                    retry_after,
                    Some(&api_key),
                ));
            }

            let is_event_stream = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| {
                    value
                        .split(';')
                        .next()
                        .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/event-stream"))
                });
            if !is_event_stream {
                return Err(OpenAiTransportError::protocol(
                    "successful OpenAI response did not use text/event-stream",
                ));
            }

            let body_api_key = Arc::clone(&api_key);
            let body = response.bytes_stream().map(move |chunk| {
                chunk
                    .map(|bytes| bytes.to_vec())
                    .map_err(|error| classify_reqwest_error(error, &body_api_key))
            });
            Ok(OpenAiHttpResponse::new(body))
        })
    }
}

fn classify_reqwest_error(error: reqwest::Error, api_key: &OpenAiApiKey) -> OpenAiTransportError {
    let message = api_key.sanitize_error(&error.to_string());
    if error.is_timeout() {
        OpenAiTransportError::timeout(message)
    } else if error.is_connect() {
        OpenAiTransportError::connection(message)
    } else {
        OpenAiTransportError::body(message)
    }
}

fn parse_retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    let value = value.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds).min(MAX_RETRY_AFTER));
    }
    let retry_at = httpdate::parse_http_date(value).ok()?;
    Some(
        retry_at
            .duration_since(now)
            .unwrap_or(Duration::ZERO)
            .min(MAX_RETRY_AFTER),
    )
}

fn sanitize_credential_text(value: &str, configured_key: &str) -> String {
    let exact_redacted = value.replace(configured_key, REDACTED_CREDENTIAL);
    sanitize_credential_tokens(&exact_redacted)
}

pub(crate) fn sanitize_credential_tokens(value: &str) -> String {
    let mut redacted = String::with_capacity(value.len());
    let mut cursor = 0;
    while let Some(relative_start) = value[cursor..].find("sk-") {
        let start = cursor + relative_start;
        redacted.push_str(&value[cursor..start]);
        let mut end = start + "sk-".len();
        let suffix_start = end;
        for (offset, character) in value[suffix_start..].char_indices() {
            if character.is_ascii_alphanumeric()
                || matches!(character, '-' | '_' | '.' | '*' | '\u{2026}')
            {
                end = suffix_start + offset + character.len_utf8();
            } else {
                break;
            }
        }
        redacted.push_str(REDACTED_CREDENTIAL);
        cursor = end;
    }
    redacted.push_str(&value[cursor..]);
    redacted
}

pub(crate) fn bounded_string(value: &str, maximum: usize) -> String {
    if value.len() <= maximum {
        return value.to_owned();
    }
    let mut end = maximum;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_key_and_transport_debug_are_redacted() {
        let key = OpenAiApiKey::new("sk-example-secret").expect("valid key");
        let debug = format!("{key:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("sk-example-secret"));

        let transport = OpenAiReqwestTransport::new(key, OpenAiTransportConfig::default())
            .expect("fixed reqwest transport");
        let debug = format!("{transport:?}");
        assert!(debug.contains(OPENAI_RESPONSES_URL));
        assert!(debug.contains("redirects"));
        assert!(!debug.contains("sk-example-secret"));
    }

    #[test]
    fn tenant_headers_reject_controls_and_oversize_values() {
        assert!(
            OpenAiTransportConfig::default()
                .with_organization("org\nleak")
                .is_err()
        );
        assert!(
            OpenAiTransportConfig::default()
                .with_project("x".repeat(MAX_TENANT_ID_BYTES + 1))
                .is_err()
        );
    }

    #[test]
    fn quota_errors_are_not_retryable_and_provider_text_is_bounded() {
        let body = serde_json::to_vec(&serde_json::json!({
            "error": {
                "code": "insufficient_quota",
                "type": "insufficient_quota",
                "message": "x".repeat(MAX_PROVIDER_MESSAGE_BYTES + 100)
            }
        }))
        .expect("serialize fixture");
        let error = OpenAiTransportError::http_status(429, &body, Some(Duration::from_secs(600)));
        assert!(!error.is_retryable_before_response());
        assert_eq!(error.message().len(), MAX_PROVIDER_MESSAGE_BYTES);
        assert_eq!(error.retry_after(), Some(MAX_RETRY_AFTER));
    }

    #[test]
    fn production_http_errors_scrub_exact_and_masked_credentials_everywhere() {
        const SENTINEL: &str = "sk-proj-DITTO_TEST_SENTINEL_9f6e2b";
        let key = OpenAiApiKey::new(SENTINEL).expect("synthetic key");
        let body = serde_json::to_vec(&serde_json::json!({
            "error": {
                "code": format!("invalid_{SENTINEL}"),
                "type": "invalid_request_error",
                "message": format!(
                    "Incorrect key {SENTINEL}; masked sk-proj-...9f6e2b; {}",
                    "x".repeat(MAX_PROVIDER_MESSAGE_BYTES + 100)
                )
            }
        }))
        .expect("serialize fixture");
        let error = OpenAiTransportError::http_status_with_api_key(401, &body, None, Some(&key));
        for exposed in [
            error.to_string(),
            format!("{error:?}"),
            error.message().to_owned(),
            error.provider_code().expect("provider code").to_owned(),
        ] {
            assert!(!exposed.contains(SENTINEL));
            assert!(!exposed.contains("sk-proj-...9f6e2b"));
        }
        assert!(error.message().contains(REDACTED_CREDENTIAL));
        assert!(error.message().len() <= MAX_PROVIDER_MESSAGE_BYTES);
        assert!(error.provider_code().expect("provider code").len() <= MAX_PROVIDER_CODE_BYTES);
    }

    #[test]
    fn retry_after_accepts_delta_seconds_and_http_dates_with_a_hard_cap() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        assert_eq!(parse_retry_after("600", now), Some(MAX_RETRY_AFTER));
        let future = httpdate::fmt_http_date(now + Duration::from_secs(37));
        assert_eq!(
            parse_retry_after(&future, now),
            Some(Duration::from_secs(37))
        );
        let past = httpdate::fmt_http_date(now - Duration::from_secs(1));
        assert_eq!(parse_retry_after(&past, now), Some(Duration::ZERO));
        assert_eq!(parse_retry_after("not-a-date", now), None);
    }

    #[test]
    fn generic_transport_errors_scrub_credential_shaped_tokens() {
        const SHAPED: &str = "sk-proj-DITTO_GENERIC_ERROR_51";
        const MASKED: &str = "sk-proj-...OR_51";
        let error = OpenAiTransportError::connection(format!("failed {SHAPED} and {MASKED}"));
        for exposed in [
            error.to_string(),
            format!("{error:?}"),
            error.message().into(),
        ] {
            assert!(!exposed.contains(SHAPED));
            assert!(!exposed.contains(MASKED));
            assert!(exposed.contains(REDACTED_CREDENTIAL));
        }
    }
}

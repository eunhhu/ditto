//! OpenAI-compatible `/chat/completions` driver for local and hosted models
//! (Ollama, llama.cpp, vLLM, LM Studio, OpenRouter, xAI and others).
mod compile;
mod mapper;
#[cfg(test)]
mod tests;

use std::{collections::BTreeSet, fmt, sync::Arc};

use ditto_model::{
    CancellationToken, DriverDescriptor, DriverId, ModelDriver, ModelEventStream, ModelFeature,
    ModelRequest, ParallelToolCalls, RequestCapabilities, ToolChoiceKind,
};
use futures_util::StreamExt;
use reqwest::{
    Client, Url,
    header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue},
    redirect::Policy,
};

use crate::{
    MAX_HTTP_ERROR_BODY_BYTES, OpenAiApiKey, OpenAiConfigError, OpenAiHttpRequest,
    OpenAiHttpResponse, OpenAiRetryPolicy, OpenAiTransport, OpenAiTransportError,
    OpenAiTransportFuture,
    driver::{compile_failure, provider_stream},
};

/// Driver ID shared by every OpenAI-compatible chat model.
pub const CHAT_COMPLETIONS_DRIVER_ID: &str = "openai-compatible.chat";
const MAX_MODEL_NAME_BYTES: usize = 256;

/// A validated `/chat/completions` endpoint. HTTPS is required except for
/// loopback hosts, which lets local servers run without TLS.
#[derive(Clone, PartialEq, Eq)]
pub struct ChatCompletionsEndpoint(Url);

impl ChatCompletionsEndpoint {
    /// `base_url` is the API root, such as `https://api.x.ai/v1` or
    /// `http://127.0.0.1:11434/v1`; `/chat/completions` is appended.
    pub fn new(base_url: &str) -> Result<Self, OpenAiConfigError> {
        let invalid = |reason: &'static str| OpenAiConfigError::InvalidEndpoint { reason };
        let mut url = Url::parse(base_url.trim()).map_err(|_| invalid("base URL is not a URL"))?;
        if !url.username().is_empty() || url.password().is_some() {
            return Err(invalid("credentials belong in the API key, not the URL"));
        }
        if url.query().is_some() || url.fragment().is_some() {
            return Err(invalid("base URL must not carry a query or fragment"));
        }
        let loopback = url_is_loopback(&url);
        match url.scheme() {
            "https" => {}
            "http" if loopback => {}
            "http" => return Err(invalid("plain HTTP is allowed only for loopback hosts")),
            _ => return Err(invalid("base URL must use HTTPS")),
        }
        let path = format!("{}/chat/completions", url.path().trim_end_matches('/'));
        url.set_path(&path);
        Ok(Self(url))
    }

    pub fn url(&self) -> &Url {
        &self.0
    }

    pub fn is_loopback(&self) -> bool {
        url_is_loopback(&self.0)
    }
}

impl fmt::Debug for ChatCompletionsEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0.as_str())
    }
}

fn url_is_loopback(url: &Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

/// Operator configuration for one OpenAI-compatible chat model.
#[derive(Clone, PartialEq, Eq)]
pub struct ChatCompletionsConfig {
    endpoint: ChatCompletionsEndpoint,
    model: String,
    include_usage: bool,
}

impl ChatCompletionsConfig {
    pub fn new(
        endpoint: ChatCompletionsEndpoint,
        model: impl Into<String>,
    ) -> Result<Self, OpenAiConfigError> {
        let model = model.into();
        if model.is_empty()
            || model.len() > MAX_MODEL_NAME_BYTES
            || model.trim() != model
            || model.chars().any(char::is_control)
        {
            return Err(OpenAiConfigError::InvalidModel);
        }
        Ok(Self {
            endpoint,
            model,
            include_usage: false,
        })
    }

    /// Ask for a final usage chunk (`stream_options.include_usage`). Off by
    /// default because some compatible servers reject the field.
    pub fn with_usage(mut self) -> Self {
        self.include_usage = true;
        self
    }

    pub fn endpoint(&self) -> &ChatCompletionsEndpoint {
        &self.endpoint
    }

    pub fn model(&self) -> &str {
        &self.model
    }
}

impl fmt::Debug for ChatCompletionsConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChatCompletionsConfig")
            .field("endpoint", &self.endpoint)
            .field("model", &self.model)
            .field("include_usage", &self.include_usage)
            .finish()
    }
}

/// reqwest transport bound to one configured endpoint. Redirects are
/// disabled; HTTPS is enforced for every non-loopback endpoint; the optional
/// API key never appears in errors or debug output.
pub struct ChatReqwestTransport {
    client: Client,
    endpoint: ChatCompletionsEndpoint,
    api_key: Option<Arc<OpenAiApiKey>>,
}

impl ChatReqwestTransport {
    pub fn new(
        endpoint: ChatCompletionsEndpoint,
        api_key: Option<OpenAiApiKey>,
    ) -> Result<Self, OpenAiConfigError> {
        let client = Client::builder()
            .https_only(!endpoint.is_loopback())
            .redirect(Policy::none())
            .build()
            .map_err(|error| OpenAiConfigError::HttpClient {
                message: crate::transport::bounded_string(
                    &crate::transport::sanitize_credential_tokens(&error.to_string()),
                    crate::MAX_PROVIDER_MESSAGE_BYTES,
                ),
            })?;
        Ok(Self {
            client,
            endpoint,
            api_key: api_key.map(Arc::new),
        })
    }
}

impl fmt::Debug for ChatReqwestTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChatReqwestTransport")
            .field("endpoint", &self.endpoint)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("redirects", &"disabled")
            .finish()
    }
}

impl OpenAiTransport for ChatReqwestTransport {
    fn send(&self, request: OpenAiHttpRequest) -> OpenAiTransportFuture {
        let client = self.client.clone();
        let url = self.endpoint.url().clone();
        let api_key = self.api_key.clone();
        Box::pin(async move {
            let mut headers = HeaderMap::new();
            headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
            if let Some(api_key) = &api_key {
                headers.insert(AUTHORIZATION, api_key.authorization_value()?);
            }
            let sanitize = |message: String| match &api_key {
                Some(api_key) => api_key.sanitize_error(&message),
                None => crate::transport::sanitize_credential_tokens(&message),
            };
            let response = client
                .post(url)
                .headers(headers)
                .body(request.body().to_vec())
                .send()
                .await
                .map_err(|error| {
                    crate::transport::classify_error(&error, sanitize(error.to_string()))
                })?;
            let status = response.status();
            if !status.is_success() {
                let mut body = Vec::new();
                let mut stream = response.bytes_stream();
                while let Some(chunk) = stream.next().await {
                    let Ok(chunk) = chunk else { break };
                    let remaining = MAX_HTTP_ERROR_BODY_BYTES.saturating_sub(body.len());
                    body.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
                    if body.len() == MAX_HTTP_ERROR_BODY_BYTES {
                        break;
                    }
                }
                return Err(OpenAiTransportError::http_status_with_api_key(
                    status.as_u16(),
                    &body,
                    None,
                    api_key.as_deref(),
                ));
            }
            let body_key = api_key.clone();
            let body = response.bytes_stream().map(move |chunk| {
                chunk.map(|bytes| bytes.to_vec()).map_err(|error| {
                    let message = match &body_key {
                        Some(api_key) => api_key.sanitize_error(&error.to_string()),
                        None => crate::transport::sanitize_credential_tokens(&error.to_string()),
                    };
                    crate::transport::classify_error(&error, message)
                })
            });
            Ok(OpenAiHttpResponse::new(body))
        })
    }
}

/// Driver for any OpenAI-compatible chat model. Text and tool calls stream
/// through the same validated model boundary as the Responses profile.
pub struct ChatCompletionsDriver {
    descriptor: DriverDescriptor,
    transport: Arc<dyn OpenAiTransport>,
    config: ChatCompletionsConfig,
    retry: OpenAiRetryPolicy,
}

impl ChatCompletionsDriver {
    pub fn new(
        config: ChatCompletionsConfig,
        api_key: Option<OpenAiApiKey>,
    ) -> Result<Self, OpenAiConfigError> {
        let transport = Arc::new(ChatReqwestTransport::new(config.endpoint.clone(), api_key)?);
        Ok(Self::with_transport(config, transport))
    }

    pub fn with_transport(
        config: ChatCompletionsConfig,
        transport: Arc<dyn OpenAiTransport>,
    ) -> Self {
        let mut emitted_features = BTreeSet::from([ModelFeature::Text, ModelFeature::ToolCalls]);
        if config.include_usage {
            emitted_features.insert(ModelFeature::Usage);
        }
        Self {
            descriptor: DriverDescriptor {
                id: DriverId::new(CHAT_COMPLETIONS_DRIVER_ID)
                    .expect("static chat driver identifier is valid"),
                request_capabilities: RequestCapabilities {
                    tool_choices: BTreeSet::from([
                        ToolChoiceKind::Auto,
                        ToolChoiceKind::None,
                        ToolChoiceKind::Required,
                        ToolChoiceKind::Specific,
                    ]),
                    parallel_tool_calls: BTreeSet::from([
                        ParallelToolCalls::Allow,
                        ParallelToolCalls::Forbid,
                    ]),
                    ..RequestCapabilities::default()
                },
                emitted_features,
            },
            transport,
            config,
            retry: OpenAiRetryPolicy::default(),
        }
    }
}

impl fmt::Debug for ChatCompletionsDriver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChatCompletionsDriver")
            .field("descriptor", &self.descriptor)
            .field("config", &self.config)
            .field("transport", &"<injected>")
            .finish()
    }
}

impl ModelDriver for ChatCompletionsDriver {
    fn descriptor(&self) -> &DriverDescriptor {
        &self.descriptor
    }

    fn stream(&self, request: ModelRequest, cancellation: CancellationToken) -> ModelEventStream {
        let model = self.config.model.clone();
        let include_usage = self.config.include_usage;
        provider_stream(
            self.descriptor.clone(),
            Arc::clone(&self.transport),
            self.retry,
            request,
            cancellation,
            move |request| {
                let compiled = compile::compile_chat_request(request, &model, include_usage)
                    .map_err(compile_failure)?;
                let mapper = mapper::ChatStreamMapper::new(
                    compiled.reverse_names,
                    request.request_id.as_str(),
                );
                Ok((compiled.http, mapper))
            },
        )
    }
}

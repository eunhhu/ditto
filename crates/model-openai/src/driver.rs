use std::{collections::BTreeSet, fmt, future::Future, sync::Arc, time::Duration};

use async_stream::stream;
use chrono::{DateTime, Utc};
use ditto_model::{
    CancellationToken, DriverDescriptor, DriverId, FailureKind, ModelDriver, ModelEvent,
    ModelEventStream, ModelFailure, ModelFeature, ModelRequest, ParallelToolCalls,
    PromptCacheCapabilities, PromptCacheMode, ProviderStateFormat, RequestCapabilities,
    ToolChoiceKind,
};
use futures_util::StreamExt;

use crate::{
    OPENAI_CONTINUATION_FORMAT, OPENAI_GPT_5_6_DRIVER_ID, OPENAI_PROVIDER, OpenAiApiKey,
    OpenAiConfigError, OpenAiReqwestTransport, OpenAiTransport, OpenAiTransportConfig,
    OpenAiTransportError, OpenAiTransportErrorKind,
    compile::{CompileError, compile_request},
    sse::{ResponseMapper, SseDecoder, SseEvent},
};

const MAX_RETRY_ATTEMPTS: u8 = 8;
const MAX_RETRY_DELAY: Duration = Duration::from_secs(60);

/// Whether OpenAI may retain responses and expose response-ID continuation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenAiStoragePolicy {
    Ephemeral,
    ProviderManaged,
}

impl OpenAiStoragePolicy {
    pub(crate) const fn stores_responses(self) -> bool {
        matches!(self, Self::ProviderManaged)
    }
}

/// Bounded pre-response retry policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenAiRetryPolicy {
    max_attempts: u8,
    base_delay: Duration,
    max_delay: Duration,
    max_retry_after: Duration,
}

impl Default for OpenAiRetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(2),
            max_retry_after: MAX_RETRY_DELAY,
        }
    }
}

impl OpenAiRetryPolicy {
    pub fn new(
        max_attempts: u8,
        base_delay: Duration,
        max_delay: Duration,
        max_retry_after: Duration,
    ) -> Result<Self, OpenAiConfigError> {
        if max_attempts == 0
            || max_attempts > MAX_RETRY_ATTEMPTS
            || base_delay > max_delay
            || max_delay > MAX_RETRY_DELAY
            || max_retry_after > MAX_RETRY_DELAY
        {
            return Err(OpenAiConfigError::InvalidRetryPolicy);
        }
        Ok(Self {
            max_attempts,
            base_delay,
            max_delay,
            max_retry_after,
        })
    }

    pub const fn max_attempts(self) -> u8 {
        self.max_attempts
    }

    fn delay_after(self, failed_attempt: u8, retry_after: Option<Duration>) -> Duration {
        let exponent = u32::from(failed_attempt.saturating_sub(1)).min(31);
        let multiplier = 1_u32.checked_shl(exponent).unwrap_or(u32::MAX);
        let exponential = self
            .base_delay
            .checked_mul(multiplier)
            .unwrap_or(self.max_delay)
            .min(self.max_delay);
        retry_after
            .map(|delay| delay.min(self.max_retry_after))
            .map_or(exponential, |delay| exponential.max(delay))
    }
}

/// Closed `gpt-5.6` OpenAI Responses driver.
pub struct OpenAiResponsesDriver {
    descriptor: DriverDescriptor,
    transport: Arc<dyn OpenAiTransport>,
    storage: OpenAiStoragePolicy,
    retry: OpenAiRetryPolicy,
}

impl OpenAiResponsesDriver {
    /// Construct the production fixed-origin reqwest/rustls driver.
    pub fn gpt_5_6(
        api_key: OpenAiApiKey,
        transport_config: OpenAiTransportConfig,
        storage: OpenAiStoragePolicy,
    ) -> Result<Self, OpenAiConfigError> {
        let transport = Arc::new(OpenAiReqwestTransport::new(api_key, transport_config)?);
        Ok(Self::gpt_5_6_with_transport(transport, storage))
    }

    /// Construct the same closed profile with an injected deterministic transport.
    pub fn gpt_5_6_with_transport(
        transport: Arc<dyn OpenAiTransport>,
        storage: OpenAiStoragePolicy,
    ) -> Self {
        Self::gpt_5_6_with_transport_and_retry(transport, storage, OpenAiRetryPolicy::default())
    }

    pub fn gpt_5_6_with_transport_and_retry(
        transport: Arc<dyn OpenAiTransport>,
        storage: OpenAiStoragePolicy,
        retry: OpenAiRetryPolicy,
    ) -> Self {
        Self {
            descriptor: descriptor(storage),
            transport,
            storage,
            retry,
        }
    }
}

impl fmt::Debug for OpenAiResponsesDriver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenAiResponsesDriver")
            .field("descriptor", &self.descriptor)
            .field("transport", &"<injected>")
            .field("storage", &self.storage)
            .field("retry", &self.retry)
            .finish()
    }
}

impl ModelDriver for OpenAiResponsesDriver {
    fn descriptor(&self) -> &DriverDescriptor {
        &self.descriptor
    }

    fn stream(&self, request: ModelRequest, cancellation: CancellationToken) -> ModelEventStream {
        let descriptor = self.descriptor.clone();
        let transport = Arc::clone(&self.transport);
        let storage = self.storage;
        let retry = self.retry;
        let raw = stream! {
            if cancellation.is_cancelled() {
                yield control_failure(FailureKind::Cancelled, "model request was cancelled before transport");
                return;
            }
            if deadline_expired(request.control.deadline) {
                yield control_failure(FailureKind::DeadlineExceeded, "model request deadline elapsed before transport");
                return;
            }
            if let Err(error) = request.validate() {
                yield ModelEvent::Failed {
                    failure: ModelFailure::new(FailureKind::Protocol, error.to_string()),
                };
                return;
            }
            if let Err(error) = request.validate_against(&descriptor) {
                yield ModelEvent::Failed {
                    failure: ModelFailure::new(FailureKind::UnsupportedFeature, error.to_string()),
                };
                return;
            }
            let usage_required = request.features.required.contains(&ModelFeature::Usage);
            let compiled = match compile_request(&request, storage) {
                Ok(compiled) => compiled,
                Err(error) => {
                    yield compile_failure(error);
                    return;
                }
            };
            let deadline = request.control.deadline;
            let mut attempt = 1_u8;
            let response = loop {
                let future = transport.send(compiled.http.clone());
                match controlled(future, &cancellation, deadline).await {
                    Controlled::Cancelled => {
                        yield control_failure(FailureKind::Cancelled, "model request was cancelled during transport handshake");
                        return;
                    }
                    Controlled::Deadline => {
                        yield control_failure(FailureKind::DeadlineExceeded, "model request deadline elapsed during transport handshake");
                        return;
                    }
                    Controlled::Value(Ok(response)) => break response,
                    Controlled::Value(Err(error)) => {
                        if error.is_retryable_before_response() && attempt < retry.max_attempts {
                            let delay = retry.delay_after(attempt, error.retry_after());
                            match controlled_sleep(delay, &cancellation, deadline).await {
                                ControlledSleep::Elapsed => {
                                    attempt += 1;
                                    continue;
                                }
                                ControlledSleep::Cancelled => {
                                    yield control_failure(FailureKind::Cancelled, "model request was cancelled during retry backoff");
                                    return;
                                }
                                ControlledSleep::Deadline => {
                                    yield control_failure(FailureKind::DeadlineExceeded, "model request deadline elapsed during retry backoff");
                                    return;
                                }
                            }
                        }
                        yield transport_failure(error);
                        return;
                    }
                }
            };

            let mut body = response.into_body();
            let mut decoder = SseDecoder::default();
            let mut mapper = ResponseMapper::new(
                compiled.reverse_names,
                compiled.output_mode,
                storage,
                usage_required,
                compiled.previous_response_id,
            );
            loop {
                match controlled(body.next(), &cancellation, deadline).await {
                    Controlled::Cancelled => {
                        yield control_failure(FailureKind::Cancelled, "model request was cancelled while streaming");
                        return;
                    }
                    Controlled::Deadline => {
                        yield control_failure(FailureKind::DeadlineExceeded, "model request deadline elapsed while streaming");
                        return;
                    }
                    Controlled::Value(Some(Ok(chunk))) => {
                        let (decoded, decode_failure) = decoder.push_preserving_prefix(&chunk);
                        let events = map_decoded_batch(&mut mapper, decoded, decode_failure);
                        let terminal = events.iter().any(ModelEvent::is_terminal);
                        for event in events {
                            yield event;
                        }
                        if terminal {
                            return;
                        }
                    }
                    Controlled::Value(Some(Err(error))) => {
                        // Headers were already accepted. This path is never retried.
                        yield transport_failure(error);
                        return;
                    }
                    Controlled::Value(None) => {
                        let (decoded, decode_failure) = decoder.finish_preserving_prefix();
                        let events = map_decoded_batch(&mut mapper, decoded, decode_failure);
                        let terminal = events.iter().any(ModelEvent::is_terminal);
                        for event in events {
                            yield event;
                        }
                        if terminal {
                            return;
                        }
                        if !mapper.is_terminal() {
                            yield ModelEvent::Failed { failure: mapper.eof_failure() };
                        }
                        return;
                    }
                }
            }
        };
        ModelEventStream::new(raw)
    }
}

fn descriptor(storage: OpenAiStoragePolicy) -> DriverDescriptor {
    let incoming_continuations = if storage == OpenAiStoragePolicy::ProviderManaged {
        BTreeSet::from([
            ProviderStateFormat::new(OPENAI_PROVIDER, OPENAI_CONTINUATION_FORMAT)
                .expect("static OpenAI continuation identifiers are valid"),
        ])
    } else {
        BTreeSet::new()
    };
    let emitted_features = {
        let mut features = BTreeSet::from([
            ModelFeature::Text,
            ModelFeature::ToolCalls,
            ModelFeature::StructuredOutput,
            ModelFeature::Usage,
        ]);
        if storage == OpenAiStoragePolicy::ProviderManaged {
            features.insert(ModelFeature::Continuation);
        }
        features
    };
    DriverDescriptor {
        id: DriverId::new(OPENAI_GPT_5_6_DRIVER_ID)
            .expect("static OpenAI driver identifier is valid"),
        request_capabilities: RequestCapabilities {
            incoming_continuations,
            reasoning: None,
            prompt_cache: Some(PromptCacheCapabilities {
                modes: BTreeSet::from([PromptCacheMode::Disabled, PromptCacheMode::Automatic]),
                ttl_seconds: BTreeSet::new(),
                supports_namespace: true,
            }),
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
        },
        emitted_features,
    }
}

fn compile_failure(error: CompileError) -> ModelEvent {
    let kind = if matches!(error, CompileError::Unsupported(_)) {
        FailureKind::UnsupportedFeature
    } else {
        FailureKind::Protocol
    };
    ModelEvent::Failed {
        failure: ModelFailure::new(kind, error.to_string()),
    }
}

fn control_failure(kind: FailureKind, message: &'static str) -> ModelEvent {
    ModelEvent::Failed {
        failure: ModelFailure::new(kind, message),
    }
}

fn transport_failure(error: OpenAiTransportError) -> ModelEvent {
    let kind = match error.kind() {
        OpenAiTransportErrorKind::Protocol => FailureKind::Protocol,
        OpenAiTransportErrorKind::Connection
        | OpenAiTransportErrorKind::Timeout
        | OpenAiTransportErrorKind::HttpStatus(_)
        | OpenAiTransportErrorKind::Body => FailureKind::Transport,
    };
    let mut failure = ModelFailure::new(kind, error.message());
    failure.provider_code = error.provider_code().map(str::to_owned);
    // The adapter has already applied its complete bounded retry policy before
    // exposing a failure, so an outer layer must not restart the same request.
    failure.retryable = false;
    ModelEvent::Failed { failure }
}

fn map_decoded_batch(
    mapper: &mut ResponseMapper,
    decoded: Vec<SseEvent>,
    trailing_failure: Option<ModelFailure>,
) -> Vec<ModelEvent> {
    let mut prefix = Vec::new();
    let mut terminal_bundle = None;
    for source_event in decoded {
        match mapper.map(source_event) {
            Ok(events) if events.iter().any(ModelEvent::is_terminal) => {
                terminal_bundle = Some(events);
            }
            Ok(events) => prefix.extend(events),
            Err(failure) => {
                prefix.push(ModelEvent::Failed { failure });
                return prefix;
            }
        }
    }
    if let Some(failure) = trailing_failure {
        prefix.push(ModelEvent::Failed { failure });
        return prefix;
    }
    if let Some(terminal_bundle) = terminal_bundle {
        prefix.extend(terminal_bundle);
    }
    prefix
}

fn deadline_expired(deadline: Option<DateTime<Utc>>) -> bool {
    deadline.is_some_and(|deadline| deadline <= Utc::now())
}

enum Controlled<T> {
    Value(T),
    Cancelled,
    Deadline,
}

async fn controlled<F, T>(
    future: F,
    cancellation: &CancellationToken,
    deadline: Option<DateTime<Utc>>,
) -> Controlled<T>
where
    F: Future<Output = T>,
{
    tokio::pin!(future);
    if let Some(deadline) = deadline {
        let Ok(remaining) = (deadline - Utc::now()).to_std() else {
            return Controlled::Deadline;
        };
        tokio::select! {
            biased;
            () = cancellation.cancelled() => Controlled::Cancelled,
            () = tokio::time::sleep(remaining) => Controlled::Deadline,
            value = &mut future => Controlled::Value(value),
        }
    } else {
        tokio::select! {
            biased;
            () = cancellation.cancelled() => Controlled::Cancelled,
            value = &mut future => Controlled::Value(value),
        }
    }
}

enum ControlledSleep {
    Elapsed,
    Cancelled,
    Deadline,
}

async fn controlled_sleep(
    delay: Duration,
    cancellation: &CancellationToken,
    deadline: Option<DateTime<Utc>>,
) -> ControlledSleep {
    match controlled(tokio::time::sleep(delay), cancellation, deadline).await {
        Controlled::Value(()) => ControlledSleep::Elapsed,
        Controlled::Cancelled => ControlledSleep::Cancelled,
        Controlled::Deadline => ControlledSleep::Deadline,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptors_are_exact_for_ephemeral_and_provider_managed_storage() {
        let transport = Arc::new(PanicTransport);
        let ephemeral = OpenAiResponsesDriver::gpt_5_6_with_transport(
            transport.clone(),
            OpenAiStoragePolicy::Ephemeral,
        );
        assert!(
            ephemeral
                .descriptor
                .request_capabilities
                .incoming_continuations
                .is_empty()
        );
        assert!(
            !ephemeral
                .descriptor
                .emitted_features
                .contains(&ModelFeature::Continuation)
        );

        let managed = OpenAiResponsesDriver::gpt_5_6_with_transport(
            transport,
            OpenAiStoragePolicy::ProviderManaged,
        );
        assert_eq!(
            managed
                .descriptor
                .request_capabilities
                .incoming_continuations
                .len(),
            1
        );
        assert!(
            managed
                .descriptor
                .emitted_features
                .contains(&ModelFeature::Continuation)
        );
        assert!(managed.descriptor.request_capabilities.reasoning.is_none());
    }

    struct PanicTransport;

    impl OpenAiTransport for PanicTransport {
        fn send(&self, _request: crate::OpenAiHttpRequest) -> crate::OpenAiTransportFuture {
            panic!("descriptor test must not call transport")
        }
    }

    #[test]
    fn retry_policy_is_bounded() {
        assert!(OpenAiRetryPolicy::new(0, Duration::ZERO, Duration::ZERO, Duration::ZERO).is_err());
        assert!(
            OpenAiRetryPolicy::new(
                2,
                Duration::from_secs(2),
                Duration::from_secs(1),
                Duration::ZERO
            )
            .is_err()
        );
    }

    #[test]
    fn production_scrubbed_http_error_stays_redacted_in_model_failure() {
        const SENTINEL: &str = "sk-proj-DITTO_TEST_SENTINEL_driver_42";
        let key = OpenAiApiKey::new(SENTINEL).expect("synthetic key");
        let body = serde_json::to_vec(&serde_json::json!({
            "error": {
                "code": format!("bad_{SENTINEL}"),
                "message": format!("rejected {SENTINEL} and sk-proj-...er_42")
            }
        }))
        .expect("HTTP error fixture");
        let error = OpenAiTransportError::http_status_with_api_key(401, &body, None, Some(&key));
        let event = transport_failure(error);
        let ModelEvent::Failed { failure } = &event else {
            panic!("transport failure must be semantic failure");
        };
        for exposed in [
            format!("{event:?}"),
            failure.message.clone(),
            failure.provider_code.clone().expect("provider code"),
        ] {
            assert!(!exposed.contains(SENTINEL));
            assert!(!exposed.contains("sk-proj-...er_42"));
        }
        assert!(failure.message.len() <= crate::MAX_PROVIDER_MESSAGE_BYTES);
        assert!(
            failure
                .provider_code
                .as_deref()
                .expect("provider code")
                .len()
                <= crate::MAX_PROVIDER_CODE_BYTES
        );
        assert!(!failure.retryable);
    }
}

//! OpenAI Responses adapter for Ditto's provider-neutral model IR.
//!
//! This crate owns OpenAI wire projection and transport. Provider credentials
//! and raw wire events never enter `ditto-model`.

mod compile;
mod driver;
mod sse;
mod transport;

pub use driver::{OpenAiResponsesDriver, OpenAiRetryPolicy, OpenAiStoragePolicy};
pub use transport::{
    OpenAiApiKey, OpenAiConfigError, OpenAiHttpRequest, OpenAiHttpResponse, OpenAiReqwestTransport,
    OpenAiTransport, OpenAiTransportConfig, OpenAiTransportError, OpenAiTransportErrorKind,
    OpenAiTransportFuture,
};

/// The only remote origin accepted by the production transport.
pub const OPENAI_RESPONSES_URL: &str = "https://api.openai.com/v1/responses";

/// Exact provider namespace used by response-ID continuation state.
pub const OPENAI_PROVIDER: &str = "openai";

/// Versioned response-ID continuation format implemented by this adapter.
pub const OPENAI_CONTINUATION_FORMAT: &str = "responses-previous-response-id-v1";

/// Closed model profile implemented by this adapter version.
pub const OPENAI_GPT_5_6_MODEL: &str = "gpt-5.6";

/// Driver ID for the closed `gpt-5.6` Responses profile.
pub const OPENAI_GPT_5_6_DRIVER_ID: &str = "openai.responses.gpt-5.6";

/// Maximum serialized request accepted before any transport activity.
pub const MAX_COMPILED_REQUEST_BYTES: usize = 8 * 1024 * 1024;

/// Maximum body retained from an unsuccessful HTTP response.
pub const MAX_HTTP_ERROR_BODY_BYTES: usize = 64 * 1024;

/// Maximum bytes accepted in one decoded SSE event.
pub const MAX_SSE_EVENT_BYTES: usize = 2 * 1024 * 1024;

/// Maximum unterminated line/event buffering in the SSE decoder.
pub const MAX_SSE_BUFFER_BYTES: usize = 2 * 1024 * 1024;

/// Maximum concurrently active provider output items.
pub const MAX_ACTIVE_OUTPUT_ITEMS: usize = 128;

/// Maximum unique output-item identities retained for one provider response.
///
/// Tool-call identities are a subset of output items, so this also bounds the
/// total historical call-correlation state while preserving duplicate checks.
pub const MAX_SEEN_OUTPUT_ITEMS: usize = 256;

/// Maximum provider error code retained in a semantic failure.
pub const MAX_PROVIDER_CODE_BYTES: usize = 128;

/// Maximum provider error message retained in a semantic failure.
pub const MAX_PROVIDER_MESSAGE_BYTES: usize = 4 * 1024;

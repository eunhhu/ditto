//! Provider-neutral streaming model contract with explicit feature flags.

use std::pin::Pin;

use ditto_protocol::CapabilityCard;
use futures::{Stream, stream};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub type ModelStream = Pin<Box<dyn Stream<Item = Result<ModelDelta, ModelDriverError>> + Send>>;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct ProviderFeatures {
    pub streaming: bool,
    pub native_tool_calls: bool,
    pub deferred_tool_search: bool,
    pub programmatic_tool_calls: bool,
    pub parallel_tool_calls: bool,
    pub prompt_cache: bool,
    pub response_continuation: bool,
    pub structured_output: bool,
    pub vision: bool,
    pub computer_use: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ModelRequest {
    pub stable_prefix: String,
    pub context_capsule: String,
    pub input: String,
    pub capabilities: Vec<CapabilityCard>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ModelDelta {
    pub text: String,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ModelDriverError {
    #[error("model provider is not configured: {0}")]
    NotConfigured(String),
    #[error("model stream failed: {0}")]
    Stream(String),
}

pub trait ModelDriver: std::fmt::Debug + Send + Sync + 'static {
    fn name(&self) -> &'static str;
    fn features(&self) -> ProviderFeatures;
    fn stream(&self, request: ModelRequest) -> ModelStream;
}

/// Deterministic local driver used only for protocol development and tests.
#[derive(Clone, Debug)]
pub struct DevelopmentDriver {
    prefix: String,
}

impl DevelopmentDriver {
    pub fn new(prefix: impl Into<String>) -> Self {
        Self {
            prefix: prefix.into(),
        }
    }
}

impl Default for DevelopmentDriver {
    fn default() -> Self {
        Self::new("ditto(dev): ")
    }
}

impl ModelDriver for DevelopmentDriver {
    fn name(&self) -> &'static str {
        "development"
    }

    fn features(&self) -> ProviderFeatures {
        ProviderFeatures {
            streaming: true,
            ..ProviderFeatures::default()
        }
    }

    fn stream(&self, request: ModelRequest) -> ModelStream {
        let response = format!("{}{}", self.prefix, request.input);
        let mut chunks = Vec::new();
        let mut chunk = String::new();
        let mut chars = 0;
        for character in response.chars() {
            chunk.push(character);
            chars += 1;
            if chars == 12 {
                chunks.push(Ok(ModelDelta { text: chunk }));
                chunk = String::new();
                chars = 0;
            }
        }
        if !chunk.is_empty() {
            chunks.push(Ok(ModelDelta { text: chunk }));
        }
        Box::pin(stream::iter(chunks))
    }
}

#[cfg(test)]
mod tests {
    use futures::TryStreamExt;

    use super::*;

    #[test]
    fn development_driver_declares_only_real_features() {
        let features = DevelopmentDriver::default().features();
        assert!(features.streaming);
        assert!(!features.native_tool_calls);
    }

    #[tokio::test]
    async fn development_driver_streams_multiple_deltas() {
        let request = ModelRequest {
            stable_prefix: String::new(),
            context_capsule: String::new(),
            input: "a request long enough for chunks".to_owned(),
            capabilities: Vec::new(),
        };
        let deltas = DevelopmentDriver::default()
            .stream(request)
            .try_collect::<Vec<_>>()
            .await
            .unwrap();
        assert!(deltas.len() > 1);
    }
}

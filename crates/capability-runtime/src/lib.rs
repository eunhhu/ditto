//! Process-isolated capability invocation contracts.

use ditto_protocol::{EffectClass, new_id};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EffectClaim {
    pub class: EffectClass,
    pub resources: Vec<String>,
    pub expected_effect: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ResourceLimits {
    pub max_memory_bytes: Option<u64>,
    pub max_output_bytes: usize,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_memory_bytes: None,
            max_output_bytes: 8 * 1024,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExpectedEvidence {
    pub contract: String,
    pub expected_result: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct InvocationEnvelope {
    pub run_id: String,
    pub capability_id: String,
    pub device_id: String,
    pub args: Value,
    pub cwd: Option<String>,
    pub environment_handles: Vec<String>,
    pub effect_claim: EffectClaim,
    pub lease_id: Option<String>,
    pub timeout_ms: u64,
    pub resource_limits: ResourceLimits,
    pub idempotency_key: String,
    pub expected_evidence: ExpectedEvidence,
}

impl InvocationEnvelope {
    pub fn new(
        capability_id: impl Into<String>,
        device_id: impl Into<String>,
        args: Value,
        effect_claim: EffectClaim,
    ) -> Self {
        let run_id = new_id("run");
        Self {
            idempotency_key: run_id.clone(),
            run_id,
            capability_id: capability_id.into(),
            device_id: device_id.into(),
            args,
            cwd: None,
            environment_handles: Vec::new(),
            effect_claim,
            lease_id: None,
            timeout_ms: 30_000,
            resource_limits: ResourceLimits::default(),
            expected_evidence: ExpectedEvidence {
                contract: "exit-code-and-expected-output".to_owned(),
                expected_result: None,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceHandleState {
    Alive,
    Reconnectable,
    Expired,
    Orphaned,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkerMessage {
    Invoke { envelope: Box<InvocationEnvelope> },
    Cancel { run_id: String },
    Health,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkerResponse {
    Started {
        run_id: String,
        resource_handle: String,
    },
    Output {
        run_id: String,
        payload: Value,
    },
    Completed {
        run_id: String,
        evidence: Value,
    },
    Failed {
        run_id: String,
        message: String,
    },
    Healthy,
}

#[cfg(test)]
mod tests {
    use ditto_protocol::EffectClass;
    use serde_json::json;

    use super::*;

    #[test]
    fn envelope_never_contains_raw_credentials() {
        let envelope = InvocationEnvelope::new(
            "device.process.run",
            "local",
            json!({"program": "git", "args": ["status"]}),
            EffectClaim {
                class: EffectClass::Read,
                resources: vec!["device:local".to_owned()],
                expected_effect: "inspect repository".to_owned(),
            },
        );
        let json = serde_json::to_string(&envelope).unwrap();
        assert!(!json.contains("credential"));
        assert!(!json.contains("private_key"));
    }
}

//! Capability execution coordinator. `device.process.run` never invokes a shell.

use std::{path::PathBuf, time::Duration};

use ditto_artifact_store::{ArtifactStore, ArtifactStoreError};
use ditto_capability_runtime::InvocationEnvelope;
use ditto_effect_policy::{CapabilityLease, PolicyError};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::process::Command;

const DEVICE_PROCESS_RUN: &str = "device.process.run";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DeviceProcessArgs {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProcessEvidence {
    pub exit_code: Option<i32>,
    pub expected_output_matched: Option<bool>,
    pub verified: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProcessOutput {
    pub run_id: String,
    pub summary: String,
    pub stdout_inline: Option<String>,
    pub stderr_inline: Option<String>,
    pub stdout_artifact: Option<String>,
    pub stderr_artifact: Option<String>,
    pub duration_ms: u128,
    pub evidence: ProcessEvidence,
}

#[derive(Clone, Debug)]
pub struct ProcessExecutor {
    artifacts: ArtifactStore,
}

#[derive(Debug, Error)]
pub enum ExecutorError {
    #[error("unsupported capability: {0}")]
    UnsupportedCapability(String),
    #[error("remote transport is not active in this scaffold: {0}")]
    UnsupportedDevice(String),
    #[error("invalid process arguments: {0}")]
    InvalidArguments(#[from] serde_json::Error),
    #[error("effect policy denied invocation: {0}")]
    Policy(#[from] PolicyError),
    #[error("process exceeded timeout of {0}ms")]
    Timeout(u64),
    #[error("process execution failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("artifact persistence failed: {0}")]
    Artifact(#[from] ArtifactStoreError),
}

impl ProcessExecutor {
    pub fn new(artifacts: ArtifactStore) -> Self {
        Self { artifacts }
    }

    /// Authorizes and runs one structured local process without a shell.
    ///
    /// # Errors
    ///
    /// Returns an error for unsupported placement, invalid arguments, denied policy,
    /// timeout, process I/O failure, or artifact persistence failure.
    pub async fn run(
        &self,
        envelope: &InvocationEnvelope,
        lease: &mut CapabilityLease,
        now_ms: i64,
    ) -> Result<ProcessOutput, ExecutorError> {
        if envelope.capability_id != DEVICE_PROCESS_RUN {
            return Err(ExecutorError::UnsupportedCapability(
                envelope.capability_id.clone(),
            ));
        }
        if envelope.device_id != "local" {
            return Err(ExecutorError::UnsupportedDevice(envelope.device_id.clone()));
        }

        let process: DeviceProcessArgs = serde_json::from_value(envelope.args.clone())?;
        lease.authorize(envelope, &process.program, now_ms)?;

        let mut command = Command::new(&process.program);
        command.args(&process.args);
        if let Some(cwd) = &envelope.cwd {
            command.current_dir(PathBuf::from(cwd));
        }
        command.kill_on_drop(true);
        command.env_clear();
        if let Some(path) = std::env::var_os("PATH") {
            command.env("PATH", path);
        }
        command.env("LANG", "C");

        let started = std::time::Instant::now();
        let output =
            tokio::time::timeout(Duration::from_millis(envelope.timeout_ms), command.output())
                .await
                .map_err(|_| ExecutorError::Timeout(envelope.timeout_ms))??;
        let duration_ms = started.elapsed().as_millis();

        let (stdout_inline, stdout_artifact) =
            self.project_output(&output.stdout, envelope.resource_limits.max_output_bytes)?;
        let (stderr_inline, stderr_artifact) =
            self.project_output(&output.stderr, envelope.resource_limits.max_output_bytes)?;

        let expected_output_matched =
            envelope
                .expected_evidence
                .expected_result
                .as_ref()
                .map(|expected| {
                    String::from_utf8_lossy(&output.stdout).contains(expected)
                        || String::from_utf8_lossy(&output.stderr).contains(expected)
                });
        let verified = output.status.success() && expected_output_matched.unwrap_or(true);
        let exit_code = output.status.code();
        let summary = format!(
            "process exited {} in {duration_ms}ms (stdout={}B, stderr={}B)",
            exit_code.map_or_else(|| "by signal".to_owned(), |code| code.to_string()),
            output.stdout.len(),
            output.stderr.len()
        );

        Ok(ProcessOutput {
            run_id: envelope.run_id.clone(),
            summary,
            stdout_inline,
            stderr_inline,
            stdout_artifact,
            stderr_artifact,
            duration_ms,
            evidence: ProcessEvidence {
                exit_code,
                expected_output_matched,
                verified,
            },
        })
    }

    fn project_output(
        &self,
        output: &[u8],
        inline_limit: usize,
    ) -> Result<(Option<String>, Option<String>), ArtifactStoreError> {
        if output.is_empty() {
            return Ok((None, None));
        }
        if output.len() <= inline_limit {
            return Ok((Some(String::from_utf8_lossy(output).into_owned()), None));
        }
        let artifact = self.artifacts.put(output)?;
        Ok((None, Some(artifact.reference)))
    }
}

#[cfg(test)]
mod tests {
    use ditto_capability_runtime::{EffectClaim, InvocationEnvelope};
    use ditto_effect_policy::CapabilityLease;
    use ditto_protocol::EffectClass;
    use serde_json::json;
    use tempfile::tempdir;

    use super::*;

    fn invocation(program: &str) -> (InvocationEnvelope, CapabilityLease) {
        let mut lease = CapabilityLease::bounded(
            vec!["local".to_owned()],
            vec!["device:local".to_owned()],
            vec![program.to_owned()],
            EffectClass::Read,
            1,
            i64::MAX,
        );
        let mut envelope = InvocationEnvelope::new(
            DEVICE_PROCESS_RUN,
            "local",
            json!({"program": program, "args": ["ditto"]}),
            EffectClaim {
                class: EffectClass::Read,
                resources: vec!["device:local".to_owned()],
                expected_effect: "print deterministic fixture".to_owned(),
            },
        );
        envelope.lease_id = Some(lease.id.clone());
        envelope.expected_evidence.expected_result = Some("ditto".to_owned());
        lease.calls_used = 0;
        (envelope, lease)
    }

    #[tokio::test]
    async fn structured_process_returns_exit_evidence() {
        let directory = tempdir().unwrap();
        let executor = ProcessExecutor::new(ArtifactStore::open(directory.path()).unwrap());
        let (envelope, mut lease) = invocation("printf");
        let output = executor.run(&envelope, &mut lease, 0).await.unwrap();
        assert!(output.evidence.verified);
        assert_eq!(output.stdout_inline.as_deref(), Some("ditto"));
    }

    #[tokio::test]
    async fn program_outside_lease_never_spawns() {
        let directory = tempdir().unwrap();
        let executor = ProcessExecutor::new(ArtifactStore::open(directory.path()).unwrap());
        let (mut envelope, mut lease) = invocation("printf");
        envelope.args = json!({"program": "false", "args": []});
        let error = executor.run(&envelope, &mut lease, 0).await.unwrap_err();
        assert!(matches!(
            error,
            ExecutorError::Policy(PolicyError::ProgramDenied(_))
        ));
    }
}

//! Closed local process profile. No caller-selected executable or ambient input.
use std::collections::{BTreeMap, BTreeSet};

use chrono::Utc;
use ditto_capability::{
    CanonicalInvocation, CanonicalResource, CapabilityDeriver, CapabilityManifest,
    CapabilityRevision, CapabilitySchema, DerivationBudget, DeriverError, DeriverRevision,
    EffectProfile, Mutation, ResolvedPlacement, canonical_manifest_digest,
};
use ditto_model::CancellationToken;
use ditto_policy::ExecutionClaim;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

mod process;

pub const ID: &str = "artifact.sort";
pub const MAX_INPUT_BYTES: usize = 64 * 1024;
pub const MAX_LINES: usize = 4096;
pub const MAX_OUTPUT_BYTES: usize = MAX_INPUT_BYTES + 1;
pub const VERIFIER: &str = "sorted-line-multiplicity-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum SortError {
    #[error("sort input is invalid or exceeds its bound")]
    InvalidInput,
    #[error("sort invocation or execution claim is invalid")]
    Authority,
    #[error("local sort process is unavailable")]
    Unavailable,
    #[error("sort was cancelled")]
    Cancelled,
    #[error("sort deadline elapsed")]
    Deadline,
    #[error("sort output exceeded its bound")]
    OutputLimit,
    #[error("sort process or pipe failed")]
    Process,
    #[error("sort output did not satisfy the requested line contract")]
    Verification,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SortArguments {
    pub reference: String,
    pub unique: bool,
}

pub fn input_reference(bytes: &[u8]) -> String {
    format!("artifact:sha256:{:x}", Sha256::digest(bytes))
}

/// LF is the only separator; CR and all other non-NUL UTF-8 bytes are data.
/// A nonempty final unterminated line gains LF; empty input remains empty.
pub fn validate_input(bytes: &[u8]) -> Result<&str, SortError> {
    if bytes.len() > MAX_INPUT_BYTES || bytes.contains(&0) {
        return Err(SortError::InvalidInput);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| SortError::InvalidInput)?;
    if text.split_terminator('\n').count() > MAX_LINES {
        return Err(SortError::InvalidInput);
    }
    Ok(text)
}

pub fn effect() -> EffectProfile {
    EffectProfile {
        mutation: Mutation::Reversible,
        ..EffectProfile::read_content()
    }
}

pub fn manifest() -> CapabilityManifest {
    toml::from_str(include_str!(
        "../../../capabilities/core/artifact-sort/capability.toml"
    ))
    .expect("packaged artifact.sort manifest is valid")
}

pub fn validate_manifest(installed: &CapabilityManifest) -> Result<(), SortError> {
    if canonical_manifest_digest(installed) != canonical_manifest_digest(&manifest()) {
        return Err(SortError::Authority);
    }
    Ok(())
}

pub fn schema() -> CapabilitySchema {
    CapabilitySchema {
        id: ID.into(),
        version: "0.1.0".into(),
        summary: manifest().summary,
        input_schema: json!({
            "type":"object", "additionalProperties":false,
            "required":["reference","unique"],
            "properties":{
                "reference":{"type":"string","pattern":"^artifact:sha256:[0-9a-f]{64}$"},
                "unique":{"type":"boolean"}
            }
        }),
        output_schema: json!({
            "type":"object", "additionalProperties":false,
            "required":["reference","verifier","input_lines","output_lines"],
            "properties":{
                "reference":{"type":"string","pattern":"^artifact:sha256:[0-9a-f]{64}$"},
                "verifier":{"const":VERIFIER},
                "input_lines":{"type":"integer","minimum":0,"maximum":MAX_LINES},
                "output_lines":{"type":"integer","minimum":0,"maximum":MAX_LINES}
            }
        }),
    }
}

pub struct SortDeriver(DeriverRevision);
impl Default for SortDeriver {
    fn default() -> Self {
        Self(DeriverRevision::new("artifact-sort-v1").unwrap())
    }
}
impl CapabilityDeriver for SortDeriver {
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
        let args: SortArguments = serde_json::from_value(arguments.clone())
            .map_err(|_| DeriverError::new("invalid sort arguments"))?;
        CanonicalResource::artifact(&args.reference)
            .map_err(|_| DeriverError::new("invalid artifact"))?;
        Ok(json!(args))
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
        let args: SortArguments = serde_json::from_value(arguments.clone())
            .map_err(|_| DeriverError::new("invalid sort arguments"))?;
        Ok(BTreeSet::from([CanonicalResource::artifact(
            args.reference,
        )
        .map_err(|_| DeriverError::new("invalid artifact"))?]))
    }
}

/// Constructed only after checking the exact input and output line contract.
/// ```compile_fail
/// use ditto_artifact_sort::VerifiedSortOutput;
/// fn deserialize<T: serde::de::DeserializeOwned>() {}
/// deserialize::<VerifiedSortOutput>();
/// ```
#[derive(Debug)]
pub struct VerifiedSortOutput {
    bytes: Vec<u8>,
    input_lines: usize,
    output_lines: usize,
}
impl VerifiedSortOutput {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn input_lines(&self) -> usize {
        self.input_lines
    }
    pub fn output_lines(&self) -> usize {
        self.output_lines
    }
}

/// Independent checker: no sort invocation, and no regeneration of expected output.
pub fn verify_output(
    input: &[u8],
    output: Vec<u8>,
    unique: bool,
) -> Result<VerifiedSortOutput, SortError> {
    let text = validate_input(input)?;
    if output.len() > MAX_OUTPUT_BYTES || (!output.is_empty() && !output.ends_with(b"\n")) {
        return Err(SortError::Verification);
    }
    let actual = std::str::from_utf8(&output).map_err(|_| SortError::Verification)?;
    let mut counts = BTreeMap::<&str, usize>::new();
    let mut input_lines = 0;
    for line in text.split_terminator('\n') {
        *counts.entry(line).or_default() += 1;
        input_lines += 1;
    }
    let mut previous = None;
    let mut output_lines = 0;
    for line in actual.split_terminator('\n') {
        if previous.is_some_and(|prev| prev > line || (unique && prev == line)) {
            return Err(SortError::Verification);
        }
        previous = Some(line);
        let count = counts.get_mut(line).ok_or(SortError::Verification)?;
        if *count == 0 {
            return Err(SortError::Verification);
        }
        if unique {
            *count = 0;
        } else {
            *count -= 1;
        }
        output_lines += 1;
    }
    if counts.values().any(|count| *count != 0) {
        return Err(SortError::Verification);
    }
    Ok(VerifiedSortOutput {
        bytes: output,
        input_lines,
        output_lines,
    })
}

/// Consumes the sole claim. No permit-only or raw program/argv entry point exists.
pub async fn execute(
    invocation: CanonicalInvocation,
    claim: ExecutionClaim,
    input: &[u8],
    cancellation: CancellationToken,
) -> Result<VerifiedSortOutput, SortError> {
    claim
        .validate(&invocation, Utc::now())
        .map_err(|_| SortError::Authority)?;
    let revision =
        CapabilityRevision::from_contract(&manifest(), &schema(), SortDeriver::default().0)
            .map_err(|_| SortError::Authority)?;
    let args: SortArguments = serde_json::from_value(invocation.normalized_arguments().clone())
        .map_err(|_| SortError::Authority)?;
    if invocation.capability_revision() != &revision
        || invocation.placement() != ResolvedPlacement::LocalProcess
        || invocation.effect() != effect()
        || args.reference != input_reference(input)
        || invocation.resources()
            != &BTreeSet::from([
                CanonicalResource::artifact(&args.reference).map_err(|_| SortError::Authority)?
            ])
    {
        return Err(SortError::Authority);
    }
    validate_input(input)?;
    let remaining = (claim.expires_at() - Utc::now())
        .to_std()
        .map_err(|_| SortError::Authority)?;
    let output = process::run(input, args.unique, cancellation, remaining).await?;
    verify_output(input, output, args.unique)
}

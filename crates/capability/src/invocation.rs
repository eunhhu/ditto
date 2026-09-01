use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use thiserror::Error;
use unicode_normalization::UnicodeNormalization;

use super::{
    CapabilityCard, CapabilityLifecycle, CapabilityManifest, CapabilitySchema, EffectProfile,
    ExecutionEpochEvidence, RuntimeType, valid_capability_id, valid_semver,
};
use crate::schema_instance::validate_invocation_argument_envelope;
use crate::{
    InvocationSchemaError, validate_invocation_instance, validate_invocation_schema_profile,
};

pub const MAX_TOOL_CALL_ID_BYTES: usize = 512;
pub const MAX_DERIVER_REVISION_BYTES: usize = 128;
pub const MAX_DERIVATION_WORK: usize = 1024;
pub const MAX_DERIVED_RESOURCES: usize = 64;
pub const MAX_DERIVER_ERROR_BYTES: usize = 4096;

const ARTIFACT_PREFIX: &str = "artifact:sha256:";
const ARTIFACT_RESOURCE_FAMILY: &str = "artifact:{artifact_id}";

/// Strict, authority-free model tool-call input.
///
/// Unknown fields are rejected during deserialization. In particular, this
/// wire shape has no effect, resource, placement, device, program, lease,
/// approval, verification, permit, or idempotency field.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UntrustedToolCall {
    call_id: ToolCallId,
    capability_id: String,
    arguments: Value,
}

impl UntrustedToolCall {
    pub fn new(
        call_id: impl Into<String>,
        capability_id: impl Into<String>,
        arguments: Value,
    ) -> Result<Self, UntrustedToolCallError> {
        let call_id = ToolCallId::new(call_id)?;
        let capability_id = capability_id.into();
        if !valid_capability_id(&capability_id) {
            return Err(UntrustedToolCallError::InvalidCapabilityId { capability_id });
        }
        if let Err(error) = validate_invocation_argument_envelope(&arguments) {
            return Err(match error {
                InvocationSchemaError::InstanceTooLarge { actual, maximum } => {
                    UntrustedToolCallError::ArgumentsTooLarge { actual, maximum }
                }
                InvocationSchemaError::InstanceDepthExceeded { maximum } => {
                    UntrustedToolCallError::ArgumentsTooDeep { maximum }
                }
                InvocationSchemaError::InstanceWorkExceeded { maximum } => {
                    UntrustedToolCallError::ArgumentsTooComplex { maximum }
                }
                _ => unreachable!("argument envelope returns only envelope failures"),
            });
        }
        Ok(Self {
            call_id,
            capability_id,
            arguments,
        })
    }

    pub fn call_id(&self) -> &ToolCallId {
        &self.call_id
    }

    pub fn capability_id(&self) -> &str {
        &self.capability_id
    }

    pub fn arguments(&self) -> &Value {
        &self.arguments
    }
}

impl<'de> Deserialize<'de> for UntrustedToolCall {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            call_id: String,
            capability_id: String,
            arguments: Value,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.call_id, wire.capability_id, wire.arguments)
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum UntrustedToolCallError {
    #[error("tool call id is invalid")]
    InvalidCallId,
    #[error("tool call capability id is invalid: {capability_id}")]
    InvalidCapabilityId { capability_id: String },
    #[error("tool call arguments are {actual} bytes, exceeding {maximum}")]
    ArgumentsTooLarge { actual: usize, maximum: usize },
    #[error("tool call arguments exceed JSON depth {maximum}")]
    ArgumentsTooDeep { maximum: usize },
    #[error("tool call arguments exceed {maximum} structural work units")]
    ArgumentsTooComplex { maximum: usize },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ToolCallId(String);

impl ToolCallId {
    pub fn new(value: impl Into<String>) -> Result<Self, UntrustedToolCallError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > MAX_TOOL_CALL_ID_BYTES
            || value.trim() != value
            || value.chars().any(char::is_control)
        {
            return Err(UntrustedToolCallError::InvalidCallId);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ToolCallId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ToolCallId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

macro_rules! digest_type {
    ($name:ident) => {
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name([u8; 32]);

        impl $name {
            fn from_bytes(bytes: [u8; 32]) -> Self {
                Self(bytes)
            }

            pub fn to_hex(self) -> String {
                hex(&self.0)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.to_hex())
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.to_hex())
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(&self.to_hex())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                parse_digest(&value)
                    .map(Self)
                    .ok_or_else(|| serde::de::Error::custom("digest is not lowercase SHA-256"))
            }
        }
    };
}

digest_type!(ManifestDigest);
digest_type!(SchemaDigest);
digest_type!(InvocationDigest);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct DeriverRevision(String);

impl DeriverRevision {
    pub fn new(value: impl Into<String>) -> Result<Self, CapabilityRevisionError> {
        let value = value.into();
        let mut characters = value.chars();
        let valid_first = characters
            .next()
            .is_some_and(|character| character.is_ascii_lowercase() || character.is_ascii_digit());
        if !valid_first
            || value.len() > MAX_DERIVER_REVISION_BYTES
            || !characters.all(|character| {
                character.is_ascii_lowercase()
                    || character.is_ascii_digit()
                    || matches!(character, '.' | '_' | '-')
            })
        {
            return Err(CapabilityRevisionError::InvalidDeriverRevision);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DeriverRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for DeriverRevision {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// Exact capability contract bound into an invocable execution epoch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityRevision {
    capability_id: String,
    capability_version: String,
    manifest_digest: ManifestDigest,
    schema_digest: SchemaDigest,
    deriver_revision: DeriverRevision,
}

impl CapabilityRevision {
    pub fn from_contract(
        manifest: &CapabilityManifest,
        schema: &CapabilitySchema,
        deriver_revision: DeriverRevision,
    ) -> Result<Self, CapabilityRevisionError> {
        if !valid_capability_id(&manifest.id) {
            return Err(CapabilityRevisionError::InvalidCapabilityId);
        }
        if !valid_semver(&manifest.version) {
            return Err(CapabilityRevisionError::InvalidCapabilityVersion);
        }
        schema
            .validate()
            .map_err(|error| CapabilityRevisionError::InvalidSchema {
                reason: error.to_string(),
            })?;
        if schema.id != manifest.id {
            return Err(CapabilityRevisionError::SchemaCapabilityMismatch);
        }
        if schema.version != manifest.version {
            return Err(CapabilityRevisionError::SchemaVersionMismatch);
        }
        Ok(Self {
            capability_id: manifest.id.clone(),
            capability_version: manifest.version.clone(),
            manifest_digest: canonical_manifest_digest(manifest),
            schema_digest: canonical_schema_digest(schema),
            deriver_revision,
        })
    }

    pub fn capability_id(&self) -> &str {
        &self.capability_id
    }

    pub fn capability_version(&self) -> &str {
        &self.capability_version
    }

    pub const fn manifest_digest(&self) -> ManifestDigest {
        self.manifest_digest
    }

    pub const fn schema_digest(&self) -> SchemaDigest {
        self.schema_digest
    }

    pub fn deriver_revision(&self) -> &DeriverRevision {
        &self.deriver_revision
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CapabilityRevisionError {
    #[error("capability revision has an invalid capability id")]
    InvalidCapabilityId,
    #[error("capability revision has an invalid capability version")]
    InvalidCapabilityVersion,
    #[error("capability revision has an invalid deriver revision")]
    InvalidDeriverRevision,
    #[error("capability revision schema is invalid: {reason}")]
    InvalidSchema { reason: String },
    #[error("capability revision schema id does not match the manifest")]
    SchemaCapabilityMismatch,
    #[error("capability revision schema version does not match the manifest")]
    SchemaVersionMismatch,
    #[error(
        "execution epoch already contains a different or discovery-only revision for {capability_id}"
    )]
    EpochRevisionConflict { capability_id: String },
    #[error("live epoch binding card does not match its capability revision")]
    BindingCardMismatch,
    #[error("capability input schema is outside the Ditto invocation profile: {reason}")]
    InvocationSchemaProfile { reason: String },
    #[error("live execution epoch is sealed for authorization")]
    EpochAlreadySealed,
    #[error("live execution epoch has no invocable capability binding")]
    EpochHasNoInvocableBinding,
}

/// One process-local execution epoch that alone can issue live invocation
/// bindings.
///
/// The type has private fields and implements neither `Serialize` nor
/// `Deserialize`.
///
/// ```compile_fail
/// use ditto_capability::LiveExecutionEpoch;
/// let _ = LiveExecutionEpoch { evidence: todo!(), bindings: todo!() };
/// ```
///
/// ```compile_fail
/// use ditto_capability::LiveExecutionEpoch;
/// fn requires_deserialize<T: serde::de::DeserializeOwned>() {}
/// requires_deserialize::<LiveExecutionEpoch>();
/// ```
///
/// ```compile_fail
/// use ditto_capability::LiveExecutionEpoch;
/// fn requires_serialize<T: serde::Serialize>() {}
/// requires_serialize::<LiveExecutionEpoch>();
/// ```
#[derive(Debug)]
pub struct LiveExecutionEpoch {
    evidence: ExecutionEpochEvidence,
    bindings: BTreeMap<String, InvocableCapabilityBinding>,
    state: LiveEpochState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LiveEpochState {
    Paging,
    AuthorizationSealed,
}

impl LiveExecutionEpoch {
    pub fn new(max_working_set: usize) -> Self {
        Self {
            evidence: ExecutionEpochEvidence::new(max_working_set),
            bindings: BTreeMap::new(),
            state: LiveEpochState::Paging,
        }
    }

    pub fn id(&self) -> &str {
        self.evidence.id()
    }

    pub fn evidence(&self) -> &ExecutionEpochEvidence {
        &self.evidence
    }

    pub fn into_evidence(self) -> ExecutionEpochEvidence {
        self.evidence
    }

    pub fn page_in(
        &mut self,
        cards: impl IntoIterator<Item = CapabilityCard>,
    ) -> Result<usize, CapabilityRevisionError> {
        self.ensure_paging()?;
        Ok(self.evidence.page_in(cards))
    }

    /// Bind one complete live capability contract into this epoch. Discovery-
    /// only cards cannot be upgraded by replay evidence or by ID alone.
    pub fn page_in_invocable(
        &mut self,
        manifest: &CapabilityManifest,
        schema: &CapabilitySchema,
        deriver_revision: DeriverRevision,
    ) -> Result<usize, CapabilityRevisionError> {
        self.ensure_paging()?;
        validate_invocation_schema_profile(&schema.input_schema).map_err(|error| {
            CapabilityRevisionError::InvocationSchemaProfile {
                reason: error.to_string(),
            }
        })?;
        let revision = CapabilityRevision::from_contract(manifest, schema, deriver_revision)?;
        if let Some(existing) = self.bindings.get(&manifest.id) {
            return if existing.revision == revision {
                Ok(0)
            } else {
                Err(CapabilityRevisionError::EpochRevisionConflict {
                    capability_id: manifest.id.clone(),
                })
            };
        }
        let card = CapabilityCard::from(manifest);
        let inserted = self
            .evidence
            .page_in_bound(card.clone(), revision.clone())?;
        if inserted == 1 {
            self.bindings.insert(
                manifest.id.clone(),
                InvocableCapabilityBinding {
                    epoch_id: self.evidence.id().to_owned(),
                    card,
                    manifest: manifest.clone(),
                    schema: schema.clone(),
                    revision,
                },
            );
        }
        Ok(inserted)
    }

    pub fn invocable_binding(&self, capability_id: &str) -> Option<&InvocableCapabilityBinding> {
        self.bindings.get(capability_id)
    }

    /// Permanently seal paging and issue this epoch's sole authorization
    /// ticket. Dropping the returned ticket never rearms the epoch.
    pub fn seal_for_authorization(
        &mut self,
    ) -> Result<EpochAuthorizationTicket, CapabilityRevisionError> {
        self.ensure_paging()?;
        if self.bindings.is_empty() {
            return Err(CapabilityRevisionError::EpochHasNoInvocableBinding);
        }
        self.state = LiveEpochState::AuthorizationSealed;
        Ok(EpochAuthorizationTicket {
            epoch_id: self.evidence.id().to_owned(),
        })
    }

    fn ensure_paging(&self) -> Result<(), CapabilityRevisionError> {
        if self.state == LiveEpochState::AuthorizationSealed {
            return Err(CapabilityRevisionError::EpochAlreadySealed);
        }
        Ok(())
    }
}

/// Sole non-wire authority to construct one ledger for a live epoch.
///
/// ```compile_fail
/// use ditto_capability::EpochAuthorizationTicket;
/// let _ = EpochAuthorizationTicket { epoch_id: String::new() };
/// ```
///
/// ```compile_fail
/// use ditto_capability::EpochAuthorizationTicket;
/// fn requires_clone<T: Clone>() {}
/// requires_clone::<EpochAuthorizationTicket>();
/// ```
///
/// ```compile_fail
/// use ditto_capability::EpochAuthorizationTicket;
/// fn requires_serialize<T: serde::Serialize>() {}
/// requires_serialize::<EpochAuthorizationTicket>();
/// ```
///
/// ```compile_fail
/// use ditto_capability::EpochAuthorizationTicket;
/// fn requires_deserialize<T: serde::de::DeserializeOwned>() {}
/// requires_deserialize::<EpochAuthorizationTicket>();
/// ```
#[derive(Debug)]
pub struct EpochAuthorizationTicket {
    epoch_id: String,
}

impl EpochAuthorizationTicket {
    pub fn epoch_id(&self) -> &str {
        &self.epoch_id
    }
}

/// Sealed complete contract issued only by a [`LiveExecutionEpoch`].
///
/// ```compile_fail
/// use ditto_capability::InvocableCapabilityBinding;
/// let _ = InvocableCapabilityBinding { epoch_id: String::new() };
/// ```
///
/// ```compile_fail
/// use ditto_capability::InvocableCapabilityBinding;
/// fn requires_deserialize<T: serde::de::DeserializeOwned>() {}
/// requires_deserialize::<InvocableCapabilityBinding>();
/// ```
///
/// ```compile_fail
/// use ditto_capability::InvocableCapabilityBinding;
/// fn requires_serialize<T: serde::Serialize>() {}
/// requires_serialize::<InvocableCapabilityBinding>();
/// ```
#[derive(Debug)]
pub struct InvocableCapabilityBinding {
    epoch_id: String,
    card: CapabilityCard,
    manifest: CapabilityManifest,
    schema: CapabilitySchema,
    revision: CapabilityRevision,
}

impl InvocableCapabilityBinding {
    pub fn epoch_id(&self) -> &str {
        &self.epoch_id
    }

    pub fn card(&self) -> &CapabilityCard {
        &self.card
    }

    pub fn manifest(&self) -> &CapabilityManifest {
        &self.manifest
    }

    pub fn schema(&self) -> &CapabilitySchema {
        &self.schema
    }

    pub fn revision(&self) -> &CapabilityRevision {
        &self.revision
    }
}

pub fn canonical_manifest_digest(manifest: &CapabilityManifest) -> ManifestDigest {
    ManifestDigest::from_bytes(sha256(&canonical_json_bytes(manifest)))
}

pub fn canonical_schema_digest(schema: &CapabilitySchema) -> SchemaDigest {
    SchemaDigest::from_bytes(sha256(&canonical_json_bytes(schema)))
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ArtifactResourceId(String);

impl ArtifactResourceId {
    pub fn new(value: impl Into<String>) -> Result<Self, CanonicalResourceError> {
        let value = value.into();
        let digest = value
            .strip_prefix(ARTIFACT_PREFIX)
            .ok_or(CanonicalResourceError::InvalidArtifact)?;
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(CanonicalResourceError::InvalidArtifact);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ArtifactResourceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Serialize for ArtifactResourceId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CanonicalPathRoot {
    rendered: String,
    components: Vec<String>,
}

impl CanonicalPathRoot {
    pub fn new(value: impl Into<String>) -> Result<Self, CanonicalResourceError> {
        let value = value.into();
        let components = parse_absolute_path(&value)?;
        if components.is_empty() {
            return Err(CanonicalResourceError::RootPathDenied);
        }
        Ok(Self {
            rendered: render_absolute(&components),
            components,
        })
    }

    pub fn as_str(&self) -> &str {
        &self.rendered
    }

    pub fn join_relative(
        &self,
        value: impl Into<String>,
    ) -> Result<CanonicalPathResource, CanonicalResourceError> {
        let value = value.into();
        if value.starts_with('/') {
            return Err(CanonicalResourceError::ExpectedRelativePath);
        }
        let relative = parse_components(&value)?;
        if relative.is_empty() {
            return Err(CanonicalResourceError::EmptyPath);
        }
        let mut components = self.components.clone();
        components.extend(relative);
        Ok(CanonicalPathResource {
            rendered: render_absolute(&components),
            components,
        })
    }

    pub fn contain_absolute(
        &self,
        value: impl Into<String>,
    ) -> Result<CanonicalPathResource, CanonicalResourceError> {
        let value = value.into();
        let components = parse_absolute_path(&value)?;
        if !components.starts_with(&self.components) {
            return Err(CanonicalResourceError::PathOutsideRoot);
        }
        Ok(CanonicalPathResource {
            rendered: render_absolute(&components),
            components,
        })
    }

    pub fn contains(&self, resource: &CanonicalPathResource) -> bool {
        resource.components.starts_with(&self.components)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CanonicalPathResource {
    rendered: String,
    components: Vec<String>,
}

impl CanonicalPathResource {
    pub fn as_str(&self) -> &str {
        &self.rendered
    }
}

impl fmt::Display for CanonicalPathResource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("path:")?;
        formatter.write_str(&self.rendered)
    }
}

impl Serialize for CanonicalPathResource {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(tag = "type", content = "id", rename_all = "snake_case")]
pub enum CanonicalResource {
    Artifact(ArtifactResourceId),
    Path(CanonicalPathResource),
}

impl CanonicalResource {
    pub fn artifact(value: impl Into<String>) -> Result<Self, CanonicalResourceError> {
        ArtifactResourceId::new(value).map(Self::Artifact)
    }

    pub fn as_artifact(&self) -> Option<&ArtifactResourceId> {
        match self {
            Self::Artifact(resource) => Some(resource),
            Self::Path(_) => None,
        }
    }
}

impl fmt::Display for CanonicalResource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Artifact(resource) => resource.fmt(formatter),
            Self::Path(resource) => resource.fmt(formatter),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CanonicalResourceError {
    #[error("artifact resource is not canonical lowercase SHA-256")]
    InvalidArtifact,
    #[error("path resource is empty")]
    EmptyPath,
    #[error("path resource is not NFC")]
    NonCanonicalUnicode,
    #[error("path resource contains a control character or backslash")]
    InvalidPathCharacter,
    #[error("path resource contains an empty, current, or parent component")]
    InvalidPathComponent,
    #[error("path root may not be the filesystem root")]
    RootPathDenied,
    #[error("relative path was required")]
    ExpectedRelativePath,
    #[error("path resource is outside the canonical root")]
    PathOutsideRoot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolvedPlacement {
    LocalBuiltin,
}

/// Fixed work counter supplied to registered deterministic derivers.
#[derive(Debug)]
pub struct DerivationBudget {
    remaining: usize,
}

impl DerivationBudget {
    fn new() -> Self {
        Self {
            remaining: MAX_DERIVATION_WORK,
        }
    }

    pub fn charge(&mut self, amount: usize) -> Result<(), DeriverError> {
        self.remaining = self
            .remaining
            .checked_sub(amount)
            .ok_or_else(|| DeriverError::new("capability derivation work bound exceeded"))?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{detail}")]
pub struct DeriverError {
    detail: String,
}

impl DeriverError {
    pub fn new(detail: impl Into<String>) -> Self {
        let detail = detail.into();
        if detail.len() <= MAX_DERIVER_ERROR_BYTES {
            Self { detail }
        } else {
            Self {
                detail: "capability deriver error exceeds the detail bound".into(),
            }
        }
    }
}

/// Trusted capability-specific derivation code. Implementations must be
/// deterministic, fixed-budget, and I/O-free. Task 005 registers only the
/// builtin `artifact.read` implementation.
pub trait CapabilityDeriver: Send + Sync {
    fn capability_id(&self) -> &str;
    fn revision(&self) -> &DeriverRevision;
    fn normalize(
        &self,
        arguments: &Value,
        budget: &mut DerivationBudget,
    ) -> Result<Value, DeriverError>;
    fn derive_effect(
        &self,
        normalized_arguments: &Value,
        budget: &mut DerivationBudget,
    ) -> Result<EffectProfile, DeriverError>;
    fn derive_resources(
        &self,
        normalized_arguments: &Value,
        budget: &mut DerivationBudget,
    ) -> Result<BTreeSet<CanonicalResource>, DeriverError>;
}

#[derive(Debug, Clone, PartialEq)]
struct CanonicalArguments(Value);

impl CanonicalArguments {
    fn value(&self) -> &Value {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InvocationId(String);

impl InvocationId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for InvocationId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Harness-derived invocation sealed after exact revision resolution, raw and
/// normalized schema validation, and bounded capability derivation.
///
/// The type deliberately has no `Deserialize` implementation and every field
/// is private.
///
/// ```compile_fail
/// use ditto_capability::CanonicalInvocation;
/// let _ = CanonicalInvocation { invocation_id: todo!() };
/// ```
///
/// ```compile_fail
/// use ditto_capability::CanonicalInvocation;
/// fn requires_deserialize<T: serde::de::DeserializeOwned>() {}
/// requires_deserialize::<CanonicalInvocation>();
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct CanonicalInvocation {
    invocation_id: InvocationId,
    source_call_id: ToolCallId,
    epoch_id: String,
    capability_revision: CapabilityRevision,
    normalized_arguments: CanonicalArguments,
    effect: EffectProfile,
    resources: BTreeSet<CanonicalResource>,
    placement: ResolvedPlacement,
    idempotency_key: IdempotencyKey,
    digest: InvocationDigest,
}

impl CanonicalInvocation {
    pub fn invocation_id(&self) -> &InvocationId {
        &self.invocation_id
    }

    pub fn source_call_id(&self) -> &ToolCallId {
        &self.source_call_id
    }

    pub fn epoch_id(&self) -> &str {
        &self.epoch_id
    }

    pub fn capability_revision(&self) -> &CapabilityRevision {
        &self.capability_revision
    }

    pub fn normalized_arguments(&self) -> &Value {
        self.normalized_arguments.value()
    }

    pub const fn effect(&self) -> EffectProfile {
        self.effect
    }

    pub fn resources(&self) -> &BTreeSet<CanonicalResource> {
        &self.resources
    }

    pub const fn placement(&self) -> ResolvedPlacement {
        self.placement
    }

    pub fn idempotency_key(&self) -> &IdempotencyKey {
        &self.idempotency_key
    }

    pub const fn digest(&self) -> InvocationDigest {
        self.digest
    }
}

/// Compiler ingress accepts only a sealed live binding, never replay evidence.
///
/// ```compile_fail
/// use ditto_capability::{ExecutionEpochEvidence, InvocationCompiler};
/// let evidence = ExecutionEpochEvidence::new(1);
/// let _ = InvocationCompiler::compile(&evidence, todo!(), todo!());
/// ```
pub struct InvocationCompiler;

impl InvocationCompiler {
    pub fn compile(
        binding: &InvocableCapabilityBinding,
        call: UntrustedToolCall,
        deriver: &dyn CapabilityDeriver,
    ) -> Result<CanonicalInvocation, InvocationError> {
        let manifest = binding.manifest();
        let schema = binding.schema();
        if call.capability_id != deriver.capability_id() {
            return Err(InvocationError::DeriverCapabilityMismatch);
        }
        if call.capability_id != binding.revision().capability_id() {
            return Err(InvocationError::BindingContractMismatch {
                field: "capability_id",
            });
        }
        if manifest.lifecycle != CapabilityLifecycle::Active {
            return Err(InvocationError::CapabilityInactive);
        }
        if canonical_json_bytes(binding.card())
            != canonical_json_bytes(&CapabilityCard::from(manifest))
        {
            return Err(InvocationError::BindingContractMismatch {
                field: "model_visible_card",
            });
        }
        let expected = binding.revision();
        let before_revision = deriver.revision().clone();
        let current = CapabilityRevision::from_contract(manifest, schema, before_revision.clone())?;
        ensure_same_revision(expected, &current)?;

        validate_invocation_instance(&schema.input_schema, &call.arguments).map_err(|source| {
            InvocationError::ArgumentsSchema {
                stage: ArgumentStage::Raw,
                source,
            }
        })?;

        let mut budget = DerivationBudget::new();
        let normalized = deriver
            .normalize(&call.arguments, &mut budget)
            .map_err(InvocationError::Deriver)?;
        validate_invocation_instance(&schema.input_schema, &normalized).map_err(|source| {
            InvocationError::ArgumentsSchema {
                stage: ArgumentStage::Normalized,
                source,
            }
        })?;

        let effect = deriver
            .derive_effect(&normalized, &mut budget)
            .map_err(InvocationError::Deriver)?;
        let resources = deriver
            .derive_resources(&normalized, &mut budget)
            .map_err(InvocationError::Deriver)?;
        if resources.len() > MAX_DERIVED_RESOURCES {
            return Err(InvocationError::TooManyResources {
                actual: resources.len(),
                maximum: MAX_DERIVED_RESOURCES,
            });
        }
        if deriver.revision() != &before_revision {
            return Err(InvocationError::DeriverRevisionChanged);
        }
        if !effect.permits(manifest.effects.minimum) {
            return Err(InvocationError::EffectBelowMinimum);
        }
        if !manifest.effects.maximum.permits(effect) {
            return Err(InvocationError::EffectAboveMaximum);
        }
        validate_derived_resources(manifest, &resources)?;
        let placement = resolve_placement(manifest)?;

        let invocation_id = InvocationId(format!(
            "inv_{}",
            domain_digest(
                b"ditto.invocation-id.v1",
                &[
                    binding.epoch_id().as_bytes(),
                    call.call_id.as_str().as_bytes(),
                ]
            )
        ));
        let idempotency_key = IdempotencyKey(format!(
            "idem_{}",
            domain_digest(
                b"ditto.idempotency-key.v1",
                &[
                    binding.epoch_id().as_bytes(),
                    call.call_id.as_str().as_bytes(),
                ]
            )
        ));
        #[derive(Serialize)]
        struct DigestProjection<'a> {
            invocation_id: &'a str,
            source_call_id: &'a str,
            epoch_id: &'a str,
            capability_revision: &'a CapabilityRevision,
            normalized_arguments: &'a Value,
            effect: EffectProfile,
            resources: &'a BTreeSet<CanonicalResource>,
            placement: ResolvedPlacement,
            idempotency_key: &'a str,
        }
        let digest =
            InvocationDigest::from_bytes(sha256(&canonical_json_bytes(&DigestProjection {
                invocation_id: invocation_id.as_str(),
                source_call_id: call.call_id.as_str(),
                epoch_id: binding.epoch_id(),
                capability_revision: &current,
                normalized_arguments: &normalized,
                effect,
                resources: &resources,
                placement,
                idempotency_key: idempotency_key.as_str(),
            })));
        Ok(CanonicalInvocation {
            invocation_id,
            source_call_id: call.call_id,
            epoch_id: binding.epoch_id().to_owned(),
            capability_revision: current,
            normalized_arguments: CanonicalArguments(normalized),
            effect,
            resources,
            placement,
            idempotency_key,
            digest,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgumentStage {
    Raw,
    Normalized,
}

#[derive(Debug, Error)]
pub enum InvocationError {
    #[error(transparent)]
    Revision(#[from] CapabilityRevisionError),
    #[error("tool call capability does not match the registered deriver")]
    DeriverCapabilityMismatch,
    #[error("capability is not active")]
    CapabilityInactive,
    #[error("live invocation binding mismatched at {field}")]
    BindingContractMismatch { field: &'static str },
    #[error("execution epoch capability revision mismatched at {field}")]
    RevisionMismatch { field: &'static str },
    #[error("invocation {stage:?} arguments failed JSON Schema validation: {source}")]
    ArgumentsSchema {
        stage: ArgumentStage,
        #[source]
        source: InvocationSchemaError,
    },
    #[error("capability deriver failed: {0}")]
    Deriver(#[source] DeriverError),
    #[error("capability deriver returned {actual} resources, exceeding {maximum}")]
    TooManyResources { actual: usize, maximum: usize },
    #[error("capability deriver revision changed during derivation")]
    DeriverRevisionChanged,
    #[error("derived effect is below the manifest minimum")]
    EffectBelowMinimum,
    #[error("derived effect exceeds the manifest maximum")]
    EffectAboveMaximum,
    #[error("derived resources do not match the manifest declaration")]
    ResourceDeclarationMismatch,
    #[error("Task 005 does not resolve this capability placement")]
    UnsupportedPlacement,
}

fn ensure_same_revision(
    expected: &CapabilityRevision,
    actual: &CapabilityRevision,
) -> Result<(), InvocationError> {
    for (field, same) in [
        (
            "capability_id",
            expected.capability_id == actual.capability_id,
        ),
        (
            "capability_version",
            expected.capability_version == actual.capability_version,
        ),
        (
            "manifest_digest",
            expected.manifest_digest == actual.manifest_digest,
        ),
        (
            "schema_digest",
            expected.schema_digest == actual.schema_digest,
        ),
        (
            "deriver_revision",
            expected.deriver_revision == actual.deriver_revision,
        ),
    ] {
        if !same {
            return Err(InvocationError::RevisionMismatch { field });
        }
    }
    Ok(())
}

fn validate_derived_resources(
    manifest: &CapabilityManifest,
    resources: &BTreeSet<CanonicalResource>,
) -> Result<(), InvocationError> {
    if manifest.effects.resources.is_empty() {
        return if resources.is_empty() {
            Ok(())
        } else {
            Err(InvocationError::ResourceDeclarationMismatch)
        };
    }
    if resources.is_empty() {
        return Err(InvocationError::ResourceDeclarationMismatch);
    }
    for resource in resources {
        let declared = manifest.effects.resources.iter().any(|declaration| {
            declaration == &resource.to_string()
                || matches!(resource, CanonicalResource::Artifact(_))
                    && declaration == ARTIFACT_RESOURCE_FAMILY
        });
        if !declared {
            return Err(InvocationError::ResourceDeclarationMismatch);
        }
    }
    Ok(())
}

fn resolve_placement(manifest: &CapabilityManifest) -> Result<ResolvedPlacement, InvocationError> {
    if manifest.runtime.runtime_type == RuntimeType::Builtin
        && manifest.placement.modes.as_slice() == ["local"]
    {
        Ok(ResolvedPlacement::LocalBuiltin)
    } else {
        Err(InvocationError::UnsupportedPlacement)
    }
}

fn parse_absolute_path(value: &str) -> Result<Vec<String>, CanonicalResourceError> {
    if !value.starts_with('/') {
        return Err(CanonicalResourceError::PathOutsideRoot);
    }
    parse_components(&value[1..])
}

fn parse_components(value: &str) -> Result<Vec<String>, CanonicalResourceError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    let normalized = value.nfc().collect::<String>();
    if normalized != value {
        return Err(CanonicalResourceError::NonCanonicalUnicode);
    }
    if value
        .chars()
        .any(|character| character.is_control() || character == '\\')
    {
        return Err(CanonicalResourceError::InvalidPathCharacter);
    }
    let components = value.split('/').collect::<Vec<_>>();
    if components
        .iter()
        .any(|component| component.is_empty() || matches!(*component, "." | ".."))
    {
        return Err(CanonicalResourceError::InvalidPathComponent);
    }
    Ok(components.into_iter().map(str::to_owned).collect())
}

fn render_absolute(components: &[String]) -> String {
    format!("/{}", components.join("/"))
}

fn canonical_json_bytes(value: &impl Serialize) -> Vec<u8> {
    let value = serde_json::to_value(value).expect("canonical JSON projection must serialize");
    let mut bytes = Vec::new();
    write_canonical_json(&value, &mut bytes);
    bytes
}

fn write_canonical_json(value: &Value, output: &mut Vec<u8>) {
    match value {
        Value::Null => output.extend_from_slice(b"null"),
        Value::Bool(true) => output.extend_from_slice(b"true"),
        Value::Bool(false) => output.extend_from_slice(b"false"),
        Value::Number(number) => output.extend_from_slice(number.to_string().as_bytes()),
        Value::String(string) => output.extend_from_slice(
            serde_json::to_string(string)
                .expect("JSON string serialization cannot fail")
                .as_bytes(),
        ),
        Value::Array(values) => {
            output.push(b'[');
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    output.push(b',');
                }
                write_canonical_json(value, output);
            }
            output.push(b']');
        }
        Value::Object(object) => {
            output.push(b'{');
            let mut entries = object.iter().collect::<Vec<_>>();
            entries.sort_unstable_by(|(left, _), (right, _)| left.as_bytes().cmp(right.as_bytes()));
            for (index, (key, value)) in entries.into_iter().enumerate() {
                if index > 0 {
                    output.push(b',');
                }
                output.extend_from_slice(
                    serde_json::to_string(key)
                        .expect("JSON object key serialization cannot fail")
                        .as_bytes(),
                );
                output.push(b':');
                write_canonical_json(value, output);
            }
            output.push(b'}');
        }
    }
}

fn domain_digest(domain: &[u8], parts: &[&[u8]]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    for part in parts {
        hasher.update([0]);
        hasher.update(part);
    }
    hex(&hasher.finalize())
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn parse_digest(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 {
        return None;
    }
    let mut bytes = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = hex_value(pair[0])?;
        let low = hex_value(pair[1])?;
        bytes[index] = (high << 4) | low;
    }
    Some(bytes)
}

fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use serde_json::{Value, json};

    use super::{
        CanonicalPathRoot, CanonicalResource, CapabilityDeriver, DerivationBudget, DeriverError,
        DeriverRevision, InvocationCompiler, InvocationError, UntrustedToolCall,
    };
    use crate::{
        CapabilityKind, CapabilityLifecycle, CapabilityManifest, CapabilitySchema, EffectProfile,
        EffectSpec, ExecutionEpochEvidence, LiveExecutionEpoch, PlacementSpec, PolicySpec,
        RetrievalSpec, RuntimeSpec, RuntimeType, VerificationSpec,
    };

    struct FixtureDeriver {
        revision: DeriverRevision,
        normalized: Value,
        effect: EffectProfile,
        resource: CanonicalResource,
        normalize_calls: AtomicUsize,
    }

    impl CapabilityDeriver for FixtureDeriver {
        fn capability_id(&self) -> &str {
            "artifact.read"
        }

        fn revision(&self) -> &DeriverRevision {
            &self.revision
        }

        fn normalize(
            &self,
            _arguments: &Value,
            budget: &mut DerivationBudget,
        ) -> Result<Value, DeriverError> {
            self.normalize_calls.fetch_add(1, Ordering::SeqCst);
            budget.charge(1)?;
            Ok(self.normalized.clone())
        }

        fn derive_effect(
            &self,
            _normalized_arguments: &Value,
            budget: &mut DerivationBudget,
        ) -> Result<EffectProfile, DeriverError> {
            budget.charge(1)?;
            Ok(self.effect)
        }

        fn derive_resources(
            &self,
            _normalized_arguments: &Value,
            budget: &mut DerivationBudget,
        ) -> Result<BTreeSet<CanonicalResource>, DeriverError> {
            budget.charge(1)?;
            Ok(BTreeSet::from([self.resource.clone()]))
        }
    }

    fn fixture() -> (CapabilityManifest, CapabilitySchema, FixtureDeriver) {
        let reference = format!("artifact:sha256:{}", "a".repeat(64));
        let arguments = json!({"reference": reference, "offset": 0, "length": 1});
        let effect = EffectProfile::read_content();
        (
            CapabilityManifest {
                id: "artifact.read".into(),
                version: "0.1.0".into(),
                namespace: "artifact".into(),
                kind: CapabilityKind::Tool,
                lifecycle: CapabilityLifecycle::Active,
                summary: "Read".into(),
                runtime: RuntimeSpec {
                    runtime_type: RuntimeType::Builtin,
                    command: None,
                    lazy: true,
                    idle_ttl_ms: 30_000,
                },
                placement: PlacementSpec {
                    modes: vec!["local".into()],
                    requires: vec![],
                },
                retrieval: RetrievalSpec::default(),
                effects: EffectSpec {
                    minimum: effect,
                    maximum: effect,
                    resources: vec!["artifact:{artifact_id}".into()],
                },
                policy: PolicySpec::default(),
                verification: VerificationSpec::default(),
            },
            CapabilitySchema {
                id: "artifact.read".into(),
                version: "0.1.0".into(),
                summary: "Read".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "reference": {"type": "string", "pattern": "^artifact:sha256:[0-9a-f]{64}$"},
                        "offset": {"type": "integer", "minimum": 0},
                        "length": {"type": "integer", "minimum": 1, "maximum": 16384}
                    },
                    "required": ["reference", "offset", "length"],
                    "additionalProperties": false
                }),
                output_schema: json!(true),
            },
            FixtureDeriver {
                revision: DeriverRevision::new("artifact-read-v1").expect("revision"),
                normalized: arguments,
                effect,
                resource: CanonicalResource::artifact(format!(
                    "artifact:sha256:{}",
                    "a".repeat(64)
                ))
                .expect("resource"),
                normalize_calls: AtomicUsize::new(0),
            },
        )
    }

    #[test]
    fn untrusted_wire_rejects_every_authority_field() {
        let base = json!({
            "call_id": "call-1",
            "capability_id": "artifact.read",
            "arguments": {"reference": format!("artifact:sha256:{}", "a".repeat(64)), "offset": 0, "length": 1}
        });
        serde_json::from_value::<UntrustedToolCall>(base.clone()).expect("authority-free call");
        for field in [
            "effect",
            "resources",
            "device",
            "program",
            "placement",
            "lease_id",
            "approval",
            "verification",
            "permit",
            "idempotency_key",
        ] {
            let mut value = base.clone();
            value
                .as_object_mut()
                .expect("object")
                .insert(field.into(), json!("attacker"));
            assert!(serde_json::from_value::<UntrustedToolCall>(value).is_err());
        }
    }

    #[test]
    fn compile_binds_exact_epoch_revision_and_revalidates_normalized_arguments() {
        let (manifest, schema, deriver) = fixture();
        let call = UntrustedToolCall::new("call-1", "artifact.read", deriver.normalized.clone())
            .expect("call");
        let mut epoch = LiveExecutionEpoch::new(1);
        epoch
            .page_in_invocable(&manifest, &schema, deriver.revision.clone())
            .expect("page revision");
        let binding = epoch
            .invocable_binding("artifact.read")
            .expect("live binding");
        let invocation =
            InvocationCompiler::compile(binding, call, &deriver).expect("canonical invocation");
        assert_eq!(invocation.effect(), EffectProfile::read_content());
        assert_eq!(invocation.resources().len(), 1);
        assert_eq!(deriver.normalize_calls.load(Ordering::SeqCst), 1);

        let mut bad_deriver = deriver;
        bad_deriver.normalized = json!({"reference": "invalid", "offset": 0, "length": 1});
        let call = UntrustedToolCall::new(
            "call-3",
            "artifact.read",
            json!({"reference": format!("artifact:sha256:{}", "a".repeat(64)), "offset": 0, "length": 1}),
        )
        .expect("call");
        assert!(matches!(
            InvocationCompiler::compile(binding, call, &bad_deriver),
            Err(InvocationError::ArgumentsSchema {
                stage: super::ArgumentStage::Normalized,
                ..
            })
        ));
    }

    #[test]
    fn untrusted_and_normalized_arguments_are_preflighted_before_canonical_projection() {
        let mut too_deep = Value::Null;
        for _ in 0..=crate::MAX_INVOCATION_VALUE_DEPTH {
            too_deep = Value::Array(vec![too_deep]);
        }
        assert_eq!(
            UntrustedToolCall::new("deep-raw", "artifact.read", too_deep.clone()),
            Err(super::UntrustedToolCallError::ArgumentsTooDeep {
                maximum: crate::MAX_INVOCATION_VALUE_DEPTH
            })
        );
        assert_eq!(
            UntrustedToolCall::new(
                "wide-raw",
                "artifact.read",
                Value::Array(vec![Value::Null; crate::MAX_INVOCATION_VALUE_WORK])
            ),
            Err(super::UntrustedToolCallError::ArgumentsTooComplex {
                maximum: crate::MAX_INVOCATION_VALUE_WORK
            })
        );

        let (manifest, schema, mut deriver) = fixture();
        let raw = deriver.normalized.clone();
        deriver.normalized = too_deep;
        let mut epoch = LiveExecutionEpoch::new(1);
        epoch
            .page_in_invocable(&manifest, &schema, deriver.revision.clone())
            .expect("page revision");
        let call =
            UntrustedToolCall::new("deep-normalized", "artifact.read", raw).expect("raw call");
        assert!(matches!(
            InvocationCompiler::compile(
                epoch.invocable_binding("artifact.read").expect("binding"),
                call,
                &deriver
            ),
            Err(InvocationError::ArgumentsSchema {
                stage: super::ArgumentStage::Normalized,
                source: crate::InvocationSchemaError::InstanceDepthExceeded {
                    maximum: crate::MAX_INVOCATION_VALUE_DEPTH
                }
            })
        ));
    }

    #[test]
    fn derived_effect_must_satisfy_both_manifest_bounds() {
        let (manifest, schema, mut deriver) = fixture();
        let mut epoch = LiveExecutionEpoch::new(1);
        epoch
            .page_in_invocable(&manifest, &schema, deriver.revision.clone())
            .expect("page revision");
        deriver.effect = EffectProfile::default();
        let call = UntrustedToolCall::new("call-low", "artifact.read", deriver.normalized.clone())
            .expect("call");
        assert!(matches!(
            InvocationCompiler::compile(
                epoch.invocable_binding("artifact.read").expect("binding"),
                call,
                &deriver
            ),
            Err(InvocationError::EffectBelowMinimum)
        ));

        let (_, _, mut deriver) = fixture();
        deriver.effect.mutation = crate::Mutation::Reversible;
        let call = UntrustedToolCall::new("call-high", "artifact.read", deriver.normalized.clone())
            .expect("call");
        assert!(matches!(
            InvocationCompiler::compile(
                epoch.invocable_binding("artifact.read").expect("binding"),
                call,
                &deriver
            ),
            Err(InvocationError::EffectAboveMaximum)
        ));
    }

    #[test]
    fn live_epoch_rejects_every_revision_mismatch() {
        let (manifest, schema, deriver) = fixture();

        let mut mismatched_card = crate::CapabilityCard::from(&manifest);
        mismatched_card.summary = "Different model disclosure".into();
        let mut discovery_epoch = LiveExecutionEpoch::new(1);
        assert_eq!(
            discovery_epoch
                .page_in([mismatched_card])
                .expect("page discovery card"),
            1
        );
        assert!(matches!(
            discovery_epoch.page_in_invocable(&manifest, &schema, deriver.revision.clone()),
            Err(super::CapabilityRevisionError::EpochRevisionConflict { .. })
        ));

        let mut epoch = LiveExecutionEpoch::new(2);
        epoch
            .page_in_invocable(&manifest, &schema, deriver.revision.clone())
            .expect("page revision");

        let mut changed_version_manifest = manifest.clone();
        changed_version_manifest.version = "0.2.0".into();
        let mut changed_version_schema = schema.clone();
        changed_version_schema.version = "0.2.0".into();
        assert!(matches!(
            epoch.page_in_invocable(
                &changed_version_manifest,
                &changed_version_schema,
                deriver.revision.clone()
            ),
            Err(super::CapabilityRevisionError::EpochRevisionConflict { .. })
        ));

        let mut changed_manifest = manifest.clone();
        changed_manifest.summary = "Changed".into();
        assert!(matches!(
            epoch.page_in_invocable(&changed_manifest, &schema, deriver.revision.clone()),
            Err(super::CapabilityRevisionError::EpochRevisionConflict { .. })
        ));

        let mut changed_schema = schema.clone();
        changed_schema.input_schema["maxProperties"] = json!(3);
        assert!(matches!(
            epoch.page_in_invocable(&manifest, &changed_schema, deriver.revision.clone()),
            Err(super::CapabilityRevisionError::EpochRevisionConflict { .. })
        ));

        let mut wrong_capability_schema = schema.clone();
        wrong_capability_schema.id = "artifact.other".into();
        assert!(matches!(
            LiveExecutionEpoch::new(1).page_in_invocable(
                &manifest,
                &wrong_capability_schema,
                deriver.revision.clone()
            ),
            Err(super::CapabilityRevisionError::SchemaCapabilityMismatch)
        ));

        let mut changed_deriver = deriver;
        changed_deriver.revision = DeriverRevision::new("artifact-read-v2").expect("revision");
        assert!(matches!(
            epoch.page_in_invocable(&manifest, &schema, changed_deriver.revision.clone()),
            Err(super::CapabilityRevisionError::EpochRevisionConflict { .. })
        ));
        let call = UntrustedToolCall::new(
            "deriver-change",
            "artifact.read",
            changed_deriver.normalized.clone(),
        )
        .expect("call");
        assert!(matches!(
            InvocationCompiler::compile(
                epoch.invocable_binding("artifact.read").expect("binding"),
                call,
                &changed_deriver
            ),
            Err(InvocationError::RevisionMismatch {
                field: "deriver_revision"
            })
        ));
    }

    #[test]
    fn live_binding_projects_replay_evidence_without_round_trip_authority() {
        let (manifest, schema, deriver) = fixture();
        let mut bound = LiveExecutionEpoch::new(1);
        bound
            .page_in_invocable(&manifest, &schema, deriver.revision.clone())
            .expect("page revision");
        let encoded = serde_json::to_value(bound.evidence()).expect("serialize evidence");
        assert_eq!(
            encoded["invocation_revisions"][0]["capability_id"],
            "artifact.read"
        );
        let decoded: ExecutionEpochEvidence =
            serde_json::from_value(encoded).expect("deserialize bound epoch");
        assert_eq!(
            decoded.invocation_revisions(),
            bound.evidence().invocation_revisions()
        );
        let binding = bound
            .invocable_binding("artifact.read")
            .expect("live binding");
        assert_eq!(binding.epoch_id(), bound.id());
        assert_eq!(
            serde_json::to_value(binding.card()).expect("binding card"),
            serde_json::to_value(&bound.evidence().capabilities()[0]).expect("evidence card")
        );
        assert_eq!(
            binding.revision(),
            bound
                .evidence()
                .invocation_revision("artifact.read")
                .expect("evidence revision")
        );

        let mut legacy = ExecutionEpochEvidence::new(1);
        legacy.page_in([crate::CapabilityCard::from(&manifest)]);
        let decoded: ExecutionEpochEvidence =
            serde_json::from_value(serde_json::to_value(&legacy).expect("serialize legacy epoch"))
                .expect("deserialize legacy epoch");
        assert!(decoded.invocation_revisions().is_empty());
    }

    #[test]
    fn live_epoch_issues_one_ticket_and_never_rearms_paging() {
        let (manifest, schema, deriver) = fixture();
        let mut epoch = LiveExecutionEpoch::new(1);
        epoch
            .page_in_invocable(&manifest, &schema, deriver.revision.clone())
            .expect("page revision");

        let ticket = epoch
            .seal_for_authorization()
            .expect("sole authorization ticket");
        assert_eq!(ticket.epoch_id(), epoch.id());
        assert!(matches!(
            epoch.seal_for_authorization(),
            Err(super::CapabilityRevisionError::EpochAlreadySealed)
        ));
        assert!(matches!(
            epoch.page_in([crate::CapabilityCard::from(&manifest)]),
            Err(super::CapabilityRevisionError::EpochAlreadySealed)
        ));
        assert!(matches!(
            epoch.page_in_invocable(&manifest, &schema, deriver.revision.clone()),
            Err(super::CapabilityRevisionError::EpochAlreadySealed)
        ));

        drop(ticket);
        assert!(matches!(
            epoch.seal_for_authorization(),
            Err(super::CapabilityRevisionError::EpochAlreadySealed)
        ));
    }

    #[test]
    fn failed_empty_epoch_seal_leaves_paging_available() {
        let (manifest, _, _) = fixture();
        let mut epoch = LiveExecutionEpoch::new(1);
        assert!(matches!(
            epoch.seal_for_authorization(),
            Err(super::CapabilityRevisionError::EpochHasNoInvocableBinding)
        ));
        assert_eq!(
            epoch
                .page_in([crate::CapabilityCard::from(&manifest)])
                .expect("failed seal does not consume paging authority"),
            1
        );
    }

    #[test]
    fn canonical_paths_reject_traversal_siblings_and_unicode_aliases() {
        let root = CanonicalPathRoot::new("/srv/ditto").expect("root");
        assert!(root.join_relative(".git/config").is_ok());
        assert!(root.join_relative("../secrets").is_err());
        assert!(root.contain_absolute("/srv/ditto-secrets/key").is_err());
        assert!(root.contain_absolute("/srv/ditto/.git/config").is_ok());
        assert!(root.join_relative("cafe\u{301}/file").is_err());
        assert!(root.join_relative("café/file").is_ok());
    }
}

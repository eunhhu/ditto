//! The builtin, read-only `artifact.read` capability.
//!
//! This crate deliberately owns only the capability contract and its bounded
//! local executor.  It accepts canonical content-addressed artifact
//! references, never paths, and turns store failures into a small stable error
//! projection.  The semantic kernel remains responsible for scope checks
//! against the event spine and for durable tool-call ordering.

use std::{borrow::Borrow, collections::BTreeSet, fmt};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use ditto_artifact_store::{ArtifactRef, ArtifactStore, ArtifactStoreError};
use ditto_capability::{
    CanonicalResource, CapabilityDeriver, CapabilityKind, CapabilityManifest, CapabilitySchema,
    DataAccess, DerivationBudget, DeriverError, DeriverRevision, EffectProfile, Externality,
    JSON_SCHEMA_DRAFT_2020_12_URI, Mutation, Privilege, RuntimeType,
};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use serde_json::{Value, json};

/// Stable identifier of the builtin capability.
pub const ARTIFACT_READ_ID: &str = "artifact.read";
/// Version of the builtin capability contract.
pub const ARTIFACT_READ_VERSION: &str = "0.1.0";
/// Stable revision of the deterministic Task 005 argument/effect/resource
/// deriver. Changing its semantics requires a new value.
pub const ARTIFACT_READ_DERIVER_REVISION: &str = "artifact-read-v1";
/// Maximum number of bytes requested by one invocation.
pub const MAX_READ_BYTES: usize = 16 * 1024;

const REFERENCE_PATTERN: &str = r"^artifact:sha256:[0-9a-f]{64}$";
const CAPABILITY_SUMMARY: &str = "Read a bounded range from a content-addressed artifact.";
const ARTIFACT_RESOURCE_TEMPLATE: &str = "artifact:{artifact_id}";
const ARTIFACT_READ_VERIFICATION: &str = "content-hash";
const ARTIFACT_READ_IDLE_TTL_MS: u64 = 30_000;
const RETRIEVAL_INTENTS: [&str; 2] = [
    "inspect the full output of a previous command",
    "read a bounded range from a large log or file",
];
const RETRIEVAL_NEGATIVE_EXAMPLES: [&str; 1] = ["modify a file"];
const RETRIEVAL_ALIASES: [&str; 2] = ["read output", "open artifact"];
const RETRIEVAL_COMPLEMENTS: [&str; 0] = [];
const INVALID_ARGUMENTS_MESSAGE: &str = "artifact.read arguments are invalid";
const INVALID_REFERENCE_MESSAGE: &str = "artifact reference is invalid";
const RANGE_MESSAGE: &str = "artifact offset is beyond the end of the artifact";
const UNAVAILABLE_MESSAGE: &str = "artifact is unavailable";
const INTEGRITY_MESSAGE: &str = "artifact integrity verification failed";

/// Stable validation failure for the installed `artifact.read` manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ArtifactReadManifestError {
    #[error("artifact.read manifest does not match the builtin contract ({field})")]
    Mismatch { field: &'static str },
}

/// Validate the installed level-1 manifest against the builtin contract.
pub fn validate_artifact_read_manifest(
    manifest: &CapabilityManifest,
) -> Result<(), ArtifactReadManifestError> {
    let mismatch = |field| Err(ArtifactReadManifestError::Mismatch { field });

    if manifest.id != ARTIFACT_READ_ID {
        return mismatch("id");
    }
    if manifest.version != ARTIFACT_READ_VERSION {
        return mismatch("version");
    }
    if manifest.namespace != "artifact" {
        return mismatch("namespace");
    }
    if manifest.kind != CapabilityKind::Tool {
        return mismatch("kind");
    }
    if manifest.summary != CAPABILITY_SUMMARY {
        return mismatch("summary");
    }
    if manifest.runtime.runtime_type != RuntimeType::Builtin {
        return mismatch("runtime.type");
    }
    if !manifest.runtime.lazy {
        return mismatch("runtime.lazy");
    }
    if manifest.runtime.command.is_some() {
        return mismatch("runtime.command");
    }
    if manifest.runtime.idle_ttl_ms != ARTIFACT_READ_IDLE_TTL_MS {
        return mismatch("runtime.idle_ttl_ms");
    }
    if manifest.placement.modes != ["local"] {
        return mismatch("placement.modes");
    }
    if !manifest.placement.requires.is_empty() {
        return mismatch("placement.requires");
    }
    if !exact_strings(&manifest.retrieval.intents, &RETRIEVAL_INTENTS) {
        return mismatch("retrieval.intents");
    }
    if !exact_strings(
        &manifest.retrieval.negative_examples,
        &RETRIEVAL_NEGATIVE_EXAMPLES,
    ) {
        return mismatch("retrieval.negative_examples");
    }
    if !exact_strings(&manifest.retrieval.aliases, &RETRIEVAL_ALIASES) {
        return mismatch("retrieval.aliases");
    }
    if !exact_strings(&manifest.retrieval.complements, &RETRIEVAL_COMPLEMENTS) {
        return mismatch("retrieval.complements");
    }

    let content_effect = EffectProfile {
        access: DataAccess::Content,
        mutation: Mutation::None,
        externality: Externality::Local,
        privilege: Privilege::User,
    };
    if manifest.effects.minimum != content_effect {
        return mismatch("effects.minimum");
    }
    if manifest.effects.maximum != content_effect {
        return mismatch("effects.maximum");
    }
    if manifest.effects.resources != [ARTIFACT_RESOURCE_TEMPLATE] {
        return mismatch("effects.resources");
    }
    if manifest.policy.approval.as_deref() != Some("never") {
        return mismatch("policy.approval");
    }
    if !manifest.policy.secret_handles.is_empty() {
        return mismatch("policy.secret_handles");
    }
    if manifest.verification.default.as_deref() != Some(ARTIFACT_READ_VERIFICATION) {
        return mismatch("verification.default");
    }
    Ok(())
}

fn valid_read_length(length: u64) -> bool {
    (1..=MAX_READ_BYTES as u64).contains(&length)
}

fn exact_strings(actual: &[String], expected: &[&str]) -> bool {
    actual.len() == expected.len()
        && actual
            .iter()
            .zip(expected)
            .all(|(actual, expected)| actual == expected)
}

fn serialize_read_arguments<S>(
    reference: &ArtifactRef,
    offset: u64,
    length: u64,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    if !valid_read_length(length) {
        return Err(serde::ser::Error::custom(INVALID_ARGUMENTS_MESSAGE));
    }
    #[derive(Serialize)]
    struct Wire<'a> {
        reference: &'a ArtifactRef,
        offset: u64,
        length: u64,
    }
    Wire {
        reference,
        offset,
        length,
    }
    .serialize(serializer)
}

/// Wire arguments for `artifact.read`.
///
/// Deserialization is intentionally strict: all three fields are required,
/// unknown fields are rejected, references are canonicalized by
/// [`ArtifactRef`], and the length bound is enforced before an executor can
/// touch the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactReadArguments {
    reference: ArtifactRef,
    offset: u64,
    length: u64,
}

impl ArtifactReadArguments {
    pub fn new(
        reference: ArtifactRef,
        offset: u64,
        length: u64,
    ) -> Result<Self, ArtifactReadError> {
        if !valid_read_length(length) {
            return Err(ArtifactReadError::invalid_arguments());
        }
        Ok(Self {
            reference,
            offset,
            length,
        })
    }

    /// Convert checked wire arguments into the normalized resource.
    pub fn normalize(self) -> ArtifactReadResource {
        ArtifactReadResource {
            reference: self.reference,
            offset: self.offset,
            length: self.length,
        }
    }

    pub fn reference(&self) -> &ArtifactRef {
        &self.reference
    }

    pub const fn offset(&self) -> u64 {
        self.offset
    }

    pub const fn length(&self) -> u64 {
        self.length
    }
}

impl Serialize for ArtifactReadArguments {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serialize_read_arguments(&self.reference, self.offset, self.length, serializer)
    }
}

impl<'de> Deserialize<'de> for ArtifactReadArguments {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            reference: String,
            offset: u64,
            length: u64,
        }

        let wire = Wire::deserialize(deserializer)?;
        let reference = ArtifactRef::new(wire.reference)
            .map_err(|_| D::Error::custom(INVALID_ARGUMENTS_MESSAGE))?;
        if !valid_read_length(wire.length) {
            return Err(D::Error::custom(INVALID_ARGUMENTS_MESSAGE));
        }
        Ok(Self {
            reference,
            offset: wire.offset,
            length: wire.length,
        })
    }
}

/// The normalized, canonical resource passed to the bounded executor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactReadResource {
    reference: ArtifactRef,
    offset: u64,
    length: u64,
}

impl Serialize for ArtifactReadResource {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serialize_read_arguments(&self.reference, self.offset, self.length, serializer)
    }
}

impl ArtifactReadResource {
    /// Construct a normalized resource while preserving the legacy checked
    /// constructor surface.
    #[deprecated(note = "use ArtifactReadArguments::new and normalize instead")]
    pub fn new(
        reference: ArtifactRef,
        offset: u64,
        length: u64,
    ) -> Result<Self, ArtifactReadError> {
        ArtifactReadArguments::new(reference, offset, length).map(ArtifactReadArguments::normalize)
    }

    pub fn reference(&self) -> &ArtifactRef {
        &self.reference
    }

    pub const fn offset(&self) -> u64 {
        self.offset
    }

    pub const fn length(&self) -> u64 {
        self.length
    }
}

impl<'de> Deserialize<'de> for ArtifactReadResource {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        ArtifactReadArguments::deserialize(deserializer).map(ArtifactReadArguments::normalize)
    }
}

/// Alias used by callers that model the normalized value as a request.
#[deprecated(note = "use ArtifactReadResource")]
pub type ArtifactReadRequest = ArtifactReadResource;

/// Stable error-code values used in serialized error projections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArtifactReadErrorCode {
    InvalidArguments,
    InvalidReference,
    RangeOutOfBounds,
    ArtifactUnavailable,
    IntegrityFailure,
    UnauthorizedReference,
}

impl ArtifactReadErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidArguments => "invalid_arguments",
            Self::InvalidReference => "invalid_reference",
            Self::RangeOutOfBounds => "range_out_of_bounds",
            Self::ArtifactUnavailable => "artifact_unavailable",
            Self::IntegrityFailure => "integrity_failure",
            Self::UnauthorizedReference => "unauthorized_reference",
        }
    }
}

impl fmt::Display for ArtifactReadErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for ArtifactReadErrorCode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

/// A bounded, path-free error projection suitable for returning to a model.
///
/// The optional reference is present only after a valid canonical reference
/// has been established.  Store paths, calculated hashes, and source errors
/// never cross this type boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactReadError {
    code: String,
    message: String,
    reference: Option<ArtifactRef>,
}

impl ArtifactReadError {
    fn from_code(code: ArtifactReadErrorCode, reference: Option<ArtifactRef>) -> Self {
        Self {
            code: code.as_str().to_owned(),
            message: Self::expected_message(code).to_owned(),
            reference,
        }
    }

    /// Construct a canonical error while preserving the legacy constructor
    /// signature. Invalid reference presence is normalized to a valid
    /// projection rather than creating a value that strict serialization
    /// would reject.
    #[deprecated(note = "use the code-specific constructors instead")]
    pub fn new(
        code: ArtifactReadErrorCode,
        _message: &'static str,
        reference: Option<ArtifactRef>,
    ) -> Self {
        if Self::requires_reference(code) && reference.is_none() {
            return Self::invalid_reference();
        }
        let reference = if Self::requires_reference(code) {
            reference
        } else {
            None
        };
        Self::from_code(code, reference)
    }

    pub fn invalid_arguments() -> Self {
        Self::from_code(ArtifactReadErrorCode::InvalidArguments, None)
    }

    pub fn invalid_reference() -> Self {
        Self::from_code(ArtifactReadErrorCode::InvalidReference, None)
    }

    pub fn range_out_of_bounds(reference: ArtifactRef) -> Self {
        Self::from_code(ArtifactReadErrorCode::RangeOutOfBounds, Some(reference))
    }

    /// Creates the stable scope-denial projection used when the kernel has no
    /// task/session root for the canonical reference.
    pub fn not_authorized(reference: ArtifactRef) -> Self {
        Self::from_code(
            ArtifactReadErrorCode::UnauthorizedReference,
            Some(reference),
        )
    }

    /// Alias for [`not_authorized`](Self::not_authorized).
    #[deprecated(note = "use not_authorized instead")]
    pub fn unauthorized_reference(reference: ArtifactRef) -> Self {
        Self::not_authorized(reference)
    }

    fn unavailable(reference: ArtifactRef) -> Self {
        Self::from_code(ArtifactReadErrorCode::ArtifactUnavailable, Some(reference))
    }

    fn integrity(reference: ArtifactRef) -> Self {
        Self::from_code(ArtifactReadErrorCode::IntegrityFailure, Some(reference))
    }

    pub fn code(&self) -> &str {
        &self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn reference(&self) -> Option<&ArtifactRef> {
        self.reference.as_ref()
    }

    pub fn code_kind(&self) -> Option<ArtifactReadErrorCode> {
        match self.code.as_str() {
            "invalid_arguments" => Some(ArtifactReadErrorCode::InvalidArguments),
            "invalid_reference" => Some(ArtifactReadErrorCode::InvalidReference),
            "range_out_of_bounds" => Some(ArtifactReadErrorCode::RangeOutOfBounds),
            "artifact_unavailable" => Some(ArtifactReadErrorCode::ArtifactUnavailable),
            "integrity_failure" => Some(ArtifactReadErrorCode::IntegrityFailure),
            "unauthorized_reference" => Some(ArtifactReadErrorCode::UnauthorizedReference),
            _ => None,
        }
    }

    fn expected_message(code: ArtifactReadErrorCode) -> &'static str {
        match code {
            ArtifactReadErrorCode::InvalidArguments => INVALID_ARGUMENTS_MESSAGE,
            ArtifactReadErrorCode::InvalidReference => INVALID_REFERENCE_MESSAGE,
            ArtifactReadErrorCode::RangeOutOfBounds => RANGE_MESSAGE,
            ArtifactReadErrorCode::ArtifactUnavailable => UNAVAILABLE_MESSAGE,
            ArtifactReadErrorCode::IntegrityFailure => INTEGRITY_MESSAGE,
            ArtifactReadErrorCode::UnauthorizedReference => {
                "artifact reference is not authorized for this turn"
            }
        }
    }

    fn requires_reference(code: ArtifactReadErrorCode) -> bool {
        matches!(
            code,
            ArtifactReadErrorCode::RangeOutOfBounds
                | ArtifactReadErrorCode::ArtifactUnavailable
                | ArtifactReadErrorCode::IntegrityFailure
                | ArtifactReadErrorCode::UnauthorizedReference
        )
    }

    fn validate_wire(&self) -> Result<ArtifactReadErrorCode, &'static str> {
        let Some(code) = self.code_kind() else {
            return Err("artifact.read error code is unknown");
        };
        if self.message != Self::expected_message(code) {
            return Err("artifact.read error message is not canonical");
        }
        if Self::requires_reference(code) != self.reference.is_some() {
            return Err("artifact.read error reference is inconsistent");
        }
        Ok(code)
    }
}

impl Serialize for ArtifactReadError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.validate_wire().map_err(serde::ser::Error::custom)?;
        #[derive(Serialize)]
        struct Wire<'a> {
            code: &'a str,
            message: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            reference: Option<&'a ArtifactRef>,
        }
        Wire {
            code: &self.code,
            message: &self.message,
            reference: self.reference.as_ref(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ArtifactReadError {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ErrorVisitor;

        impl<'de> serde::de::Visitor<'de> for ErrorVisitor {
            type Value = ArtifactReadError;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an artifact.read error projection object")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                let mut code = None;
                let mut message = None;
                // The outer Option records key presence; the inner Option
                // preserves an explicit JSON null for validation below.
                let mut reference: Option<Option<String>> = None;

                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "code" => {
                            if code.is_some() {
                                return Err(A::Error::custom(
                                    "artifact.read error projection is invalid",
                                ));
                            }
                            code = Some(map.next_value::<String>()?);
                        }
                        "message" => {
                            if message.is_some() {
                                return Err(A::Error::custom(
                                    "artifact.read error projection is invalid",
                                ));
                            }
                            message = Some(map.next_value::<String>()?);
                        }
                        "reference" => {
                            if reference.is_some() {
                                return Err(A::Error::custom(
                                    "artifact.read error projection is invalid",
                                ));
                            }
                            reference = Some(map.next_value::<Option<String>>()?);
                        }
                        _ => {
                            return Err(A::Error::custom(
                                "artifact.read error projection is invalid",
                            ));
                        }
                    }
                }

                let code_value = code
                    .ok_or_else(|| A::Error::custom("artifact.read error projection is invalid"))?;
                let message = message
                    .ok_or_else(|| A::Error::custom("artifact.read error projection is invalid"))?;
                let Some(code) = (match code_value.as_str() {
                    "invalid_arguments" => Some(ArtifactReadErrorCode::InvalidArguments),
                    "invalid_reference" => Some(ArtifactReadErrorCode::InvalidReference),
                    "range_out_of_bounds" => Some(ArtifactReadErrorCode::RangeOutOfBounds),
                    "artifact_unavailable" => Some(ArtifactReadErrorCode::ArtifactUnavailable),
                    "integrity_failure" => Some(ArtifactReadErrorCode::IntegrityFailure),
                    "unauthorized_reference" => Some(ArtifactReadErrorCode::UnauthorizedReference),
                    _ => None,
                }) else {
                    return Err(A::Error::custom(
                        "artifact.read error projection is invalid",
                    ));
                };
                let reference_present = reference.is_some();
                let reference = match reference {
                    Some(Some(value)) => Some(ArtifactRef::new(value).map_err(|_| {
                        A::Error::custom("artifact.read error projection is invalid")
                    })?),
                    Some(None) | None => None,
                };
                let error = ArtifactReadError::from_code(code, reference);
                if message != error.message
                    || ArtifactReadError::requires_reference(code) != reference_present
                    || ArtifactReadError::requires_reference(code) != error.reference.is_some()
                {
                    return Err(A::Error::custom(
                        "artifact.read error projection is invalid",
                    ));
                }
                Ok(error)
            }
        }

        deserializer.deserialize_map(ErrorVisitor)
    }
}

impl fmt::Display for ArtifactReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(reference) = &self.reference {
            write!(formatter, "{}: {} ({reference})", self.code, self.message)
        } else {
            write!(formatter, "{}: {}", self.code, self.message)
        }
    }
}

impl std::error::Error for ArtifactReadError {}

/// Successful binary-safe read projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactReadSuccess {
    reference: ArtifactRef,
    offset: u64,
    requested_bytes: u64,
    returned_bytes: u64,
    total_bytes: u64,
    eof: bool,
    data: String,
}

impl ArtifactReadSuccess {
    pub fn reference(&self) -> &ArtifactRef {
        &self.reference
    }

    pub const fn offset(&self) -> u64 {
        self.offset
    }

    pub const fn requested_bytes(&self) -> u64 {
        self.requested_bytes
    }

    pub const fn returned_bytes(&self) -> u64 {
        self.returned_bytes
    }

    pub const fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    pub const fn eof(&self) -> bool {
        self.eof
    }

    pub fn data(&self) -> &str {
        &self.data
    }

    pub fn decoded_data(&self) -> Result<Vec<u8>, base64::DecodeError> {
        BASE64.decode(&self.data)
    }

    fn validate_wire(&self) -> Result<(), &'static str> {
        if self.requested_bytes == 0 || self.requested_bytes > MAX_READ_BYTES as u64 {
            return Err("artifact.read success requested byte count is invalid");
        }
        if self.returned_bytes > self.requested_bytes
            || self.returned_bytes > MAX_READ_BYTES as u64
            || self.offset > self.total_bytes
        {
            return Err("artifact.read success byte counts are inconsistent");
        }
        let expected_returned = self
            .requested_bytes
            .min(self.total_bytes.saturating_sub(self.offset));
        if self.returned_bytes != expected_returned {
            return Err("artifact.read success byte counts are inconsistent");
        }
        let expected_eof = self.offset + self.returned_bytes == self.total_bytes;
        if self.eof != expected_eof {
            return Err("artifact.read success EOF flag is inconsistent");
        }
        let decoded = BASE64
            .decode(&self.data)
            .map_err(|_| "artifact.read success data is not valid base64")?;
        if decoded.len() as u64 != self.returned_bytes || BASE64.encode(decoded) != self.data {
            return Err("artifact.read success data length is inconsistent");
        }
        Ok(())
    }
}

impl Serialize for ArtifactReadSuccess {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.validate_wire().map_err(serde::ser::Error::custom)?;
        #[derive(Serialize)]
        struct Wire<'a> {
            reference: &'a ArtifactRef,
            offset: u64,
            requested_bytes: u64,
            returned_bytes: u64,
            total_bytes: u64,
            eof: bool,
            data: &'a str,
        }
        Wire {
            reference: &self.reference,
            offset: self.offset,
            requested_bytes: self.requested_bytes,
            returned_bytes: self.returned_bytes,
            total_bytes: self.total_bytes,
            eof: self.eof,
            data: &self.data,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ArtifactReadSuccess {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            reference: String,
            offset: u64,
            requested_bytes: u64,
            returned_bytes: u64,
            total_bytes: u64,
            eof: bool,
            data: String,
        }

        let wire = Wire::deserialize(deserializer)?;
        let reference = ArtifactRef::new(wire.reference)
            .map_err(|_| D::Error::custom("artifact.read success projection is invalid"))?;
        let success = Self {
            reference,
            offset: wire.offset,
            requested_bytes: wire.requested_bytes,
            returned_bytes: wire.returned_bytes,
            total_bytes: wire.total_bytes,
            eof: wire.eof,
            data: wire.data,
        };
        success
            .validate_wire()
            .map_err(D::Error::custom)
            .map(|()| success)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ArtifactReadResultKind {
    Success(ArtifactReadSuccess),
    Error(ArtifactReadError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactReadResult {
    kind: ArtifactReadResultKind,
}

impl ArtifactReadResult {
    pub fn success(value: ArtifactReadSuccess) -> Self {
        Self {
            kind: ArtifactReadResultKind::Success(value),
        }
    }

    pub fn error(value: ArtifactReadError) -> Self {
        Self {
            kind: ArtifactReadResultKind::Error(value),
        }
    }

    pub fn is_error(&self) -> bool {
        matches!(self.kind, ArtifactReadResultKind::Error(_))
    }

    pub fn success_projection(&self) -> Option<&ArtifactReadSuccess> {
        match &self.kind {
            ArtifactReadResultKind::Success(success) => Some(success),
            ArtifactReadResultKind::Error(_) => None,
        }
    }

    pub fn error_projection(&self) -> Option<&ArtifactReadError> {
        match &self.kind {
            ArtifactReadResultKind::Success(_) => None,
            ArtifactReadResultKind::Error(error) => Some(error),
        }
    }

    /// Consume the result and return its success projection, if present.
    #[deprecated(note = "use success_projection for a borrowed projection")]
    pub fn into_success(self) -> Option<ArtifactReadSuccess> {
        match self.kind {
            ArtifactReadResultKind::Success(success) => Some(success),
            ArtifactReadResultKind::Error(_) => None,
        }
    }

    /// Consume the result and return its error projection, if present.
    #[deprecated(note = "use error_projection for a borrowed projection")]
    pub fn into_error(self) -> Option<ArtifactReadError> {
        match self.kind {
            ArtifactReadResultKind::Success(_) => None,
            ArtifactReadResultKind::Error(error) => Some(error),
        }
    }
}

impl Serialize for ArtifactReadResult {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match &self.kind {
            ArtifactReadResultKind::Error(error) => {
                #[derive(Serialize)]
                struct ErrorWire<'a> {
                    is_error: bool,
                    error: &'a ArtifactReadError,
                }
                ErrorWire {
                    is_error: true,
                    error,
                }
                .serialize(serializer)
            }
            ArtifactReadResultKind::Success(success) => {
                #[derive(Serialize)]
                struct SuccessWire<'a> {
                    is_error: bool,
                    #[serde(flatten)]
                    success: &'a ArtifactReadSuccess,
                }
                SuccessWire {
                    is_error: false,
                    success,
                }
                .serialize(serializer)
            }
        }
    }
}

impl<'de> Deserialize<'de> for ArtifactReadResult {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct ErrorWire {
            is_error: bool,
            error: ArtifactReadError,
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct SuccessWire {
            is_error: bool,
            reference: String,
            offset: u64,
            requested_bytes: u64,
            returned_bytes: u64,
            total_bytes: u64,
            eof: bool,
            data: String,
        }

        let value = Value::deserialize(deserializer)?;
        if value.get("is_error") == Some(&Value::Bool(true)) {
            let wire: ErrorWire = serde_json::from_value(value).map_err(D::Error::custom)?;
            if !wire.is_error {
                return Err(D::Error::custom("artifact.read error flag is inconsistent"));
            }
            return Ok(Self::error(wire.error));
        }

        let wire: SuccessWire = serde_json::from_value(value).map_err(D::Error::custom)?;
        if wire.is_error {
            return Err(D::Error::custom(
                "artifact.read success flag is inconsistent",
            ));
        }
        let reference = ArtifactRef::new(wire.reference)
            .map_err(|_| D::Error::custom("artifact.read success projection is invalid"))?;
        let success = ArtifactReadSuccess {
            reference,
            offset: wire.offset,
            requested_bytes: wire.requested_bytes,
            returned_bytes: wire.returned_bytes,
            total_bytes: wire.total_bytes,
            eof: wire.eof,
            data: wire.data,
        };
        success
            .validate_wire()
            .map_err(D::Error::custom)
            .map(|()| Self::success(success))
    }
}

/// Stateless strict parser and canonicalizer for model-provided arguments.
#[derive(Debug, Clone, Copy, Default)]
pub struct ArtifactReadNormalizer;

impl ArtifactReadNormalizer {
    pub fn normalize<T>(&self, arguments: T) -> Result<ArtifactReadResource, ArtifactReadError>
    where
        T: Borrow<Value>,
    {
        let value = arguments.borrow();
        if let Some(reference) = value.get("reference").and_then(Value::as_str)
            && ArtifactRef::new(reference).is_err()
        {
            return Err(ArtifactReadError::invalid_reference());
        }

        let arguments: ArtifactReadArguments = serde_json::from_value(value.clone())
            .map_err(|_| ArtifactReadError::invalid_arguments())?;
        Ok(arguments.normalize())
    }

    /// Parse one JSON object through the strict normalizer.
    #[deprecated(note = "use normalize with a serde_json::Value instead")]
    pub fn parse_json(&self, input: &str) -> Result<ArtifactReadResource, ArtifactReadError> {
        let value: Value =
            serde_json::from_str(input).map_err(|_| ArtifactReadError::invalid_arguments())?;
        self.normalize(value)
    }

    /// Parse UTF-8 JSON bytes through the strict normalizer.
    #[deprecated(note = "use normalize with a serde_json::Value instead")]
    pub fn parse_bytes(&self, input: &[u8]) -> Result<ArtifactReadResource, ArtifactReadError> {
        let value: Value =
            serde_json::from_slice(input).map_err(|_| ArtifactReadError::invalid_arguments())?;
        self.normalize(value)
    }
}

/// Deterministic, I/O-free Task 005 derivation for the builtin contract.
#[derive(Debug, Clone)]
pub struct ArtifactReadDeriver {
    revision: DeriverRevision,
}

impl Default for ArtifactReadDeriver {
    fn default() -> Self {
        Self {
            revision: DeriverRevision::new(ARTIFACT_READ_DERIVER_REVISION)
                .expect("the builtin deriver revision is a valid constant"),
        }
    }
}

impl CapabilityDeriver for ArtifactReadDeriver {
    fn capability_id(&self) -> &str {
        ARTIFACT_READ_ID
    }

    fn revision(&self) -> &DeriverRevision {
        &self.revision
    }

    fn normalize(
        &self,
        arguments: &Value,
        budget: &mut DerivationBudget,
    ) -> Result<Value, DeriverError> {
        budget.charge(1)?;
        let normalized = ArtifactReadNormalizer
            .normalize(arguments)
            .map_err(|error| DeriverError::new(error.code()))?;
        serde_json::to_value(normalized)
            .map_err(|_| DeriverError::new("artifact.read normalization serialization failed"))
    }

    fn derive_effect(
        &self,
        normalized_arguments: &Value,
        budget: &mut DerivationBudget,
    ) -> Result<EffectProfile, DeriverError> {
        budget.charge(1)?;
        serde_json::from_value::<ArtifactReadResource>(normalized_arguments.clone())
            .map_err(|_| DeriverError::new("artifact.read normalized arguments are invalid"))?;
        Ok(EffectProfile::read_content())
    }

    fn derive_resources(
        &self,
        normalized_arguments: &Value,
        budget: &mut DerivationBudget,
    ) -> Result<BTreeSet<CanonicalResource>, DeriverError> {
        budget.charge(1)?;
        let normalized =
            serde_json::from_value::<ArtifactReadResource>(normalized_arguments.clone())
                .map_err(|_| DeriverError::new("artifact.read normalized arguments are invalid"))?;
        let resource = CanonicalResource::artifact(normalized.reference().to_string())
            .map_err(|error| DeriverError::new(error.to_string()))?;
        Ok(BTreeSet::from([resource]))
    }
}

/// Normalize one JSON argument object without constructing an executor.
#[deprecated(note = "use ArtifactReadNormalizer::normalize instead")]
pub fn normalize_arguments<T>(arguments: T) -> Result<ArtifactReadResource, ArtifactReadError>
where
    T: Borrow<Value>,
{
    ArtifactReadNormalizer.normalize(arguments)
}

/// Short spelling for [`normalize_arguments`].
#[deprecated(note = "use ArtifactReadNormalizer::normalize instead")]
pub fn normalize<T>(arguments: T) -> Result<ArtifactReadResource, ArtifactReadError>
where
    T: Borrow<Value>,
{
    ArtifactReadNormalizer.normalize(arguments)
}

/// Returns the complete provider-neutral level-2 capability schema.
pub fn capability_schema() -> CapabilitySchema {
    CapabilitySchema {
        id: ARTIFACT_READ_ID.to_owned(),
        version: ARTIFACT_READ_VERSION.to_owned(),
        summary: CAPABILITY_SUMMARY.to_owned(),
        input_schema: input_schema(),
        output_schema: output_schema(),
    }
}

/// Alias for [`capability_schema`].
#[deprecated(note = "use capability_schema instead")]
pub fn schema() -> CapabilitySchema {
    capability_schema()
}

/// Returns the exact input JSON Schema without the surrounding capability
/// metadata.
pub fn input_schema() -> Value {
    json!({
        "$schema": JSON_SCHEMA_DRAFT_2020_12_URI,
        "type": "object",
        "properties": {
            "reference": {
                "type": "string",
                "pattern": REFERENCE_PATTERN,
            },
            "offset": {
                "type": "integer",
                "minimum": 0,
                "maximum": u64::MAX,
            },
            "length": {
                "type": "integer",
                "minimum": 1,
                "maximum": MAX_READ_BYTES,
            },
        },
        "required": ["reference", "offset", "length"],
        "additionalProperties": false,
    })
}

/// Returns the deterministic success/error output JSON Schema.
pub fn output_schema() -> Value {
    let error_schema = |code: &str, message: &str, has_reference: bool| {
        let mut properties = serde_json::Map::new();
        properties.insert("code".to_owned(), json!({"const": code}));
        properties.insert("message".to_owned(), json!({"const": message}));
        let mut required = vec![json!("code"), json!("message")];
        if has_reference {
            properties.insert(
                "reference".to_owned(),
                json!({
                    "type": "string",
                    "pattern": REFERENCE_PATTERN,
                }),
            );
            required.push(json!("reference"));
        }
        json!({
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false,
        })
    };
    let error_schemas = vec![
        error_schema(
            ArtifactReadErrorCode::InvalidArguments.as_str(),
            INVALID_ARGUMENTS_MESSAGE,
            false,
        ),
        error_schema(
            ArtifactReadErrorCode::InvalidReference.as_str(),
            INVALID_REFERENCE_MESSAGE,
            false,
        ),
        error_schema(
            ArtifactReadErrorCode::RangeOutOfBounds.as_str(),
            RANGE_MESSAGE,
            true,
        ),
        error_schema(
            ArtifactReadErrorCode::ArtifactUnavailable.as_str(),
            UNAVAILABLE_MESSAGE,
            true,
        ),
        error_schema(
            ArtifactReadErrorCode::IntegrityFailure.as_str(),
            INTEGRITY_MESSAGE,
            true,
        ),
        error_schema(
            ArtifactReadErrorCode::UnauthorizedReference.as_str(),
            "artifact reference is not authorized for this turn",
            true,
        ),
    ];
    json!({
        "$schema": JSON_SCHEMA_DRAFT_2020_12_URI,
        "oneOf": [
            {
                "type": "object",
                "properties": {
                    "is_error": {"const": false},
                    "reference": {
                        "type": "string",
                        "pattern": REFERENCE_PATTERN,
                    },
                    "offset": {"type": "integer", "minimum": 0, "maximum": u64::MAX},
                    "requested_bytes": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": MAX_READ_BYTES,
                    },
                    "returned_bytes": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": MAX_READ_BYTES,
                    },
                    "total_bytes": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": u64::MAX,
                    },
                    "eof": {"type": "boolean"},
                    "data": {
                        "type": "string",
                        "contentEncoding": "base64",
                        "contentMediaType": "application/octet-stream",
                    },
                },
                "required": [
                    "is_error",
                    "reference",
                    "offset",
                    "requested_bytes",
                    "returned_bytes",
                    "total_bytes",
                    "eof",
                    "data",
                ],
                "additionalProperties": false,
            },
            {
                "type": "object",
                "properties": {
                    "is_error": {"const": true},
                    "error": {"oneOf": error_schemas},
                },
                "required": ["is_error", "error"],
                "additionalProperties": false,
            },
        ],
    })
}

/// A bounded local authority for the builtin artifact read.
///
/// This type owns no path or process capability.  Its only storage operation
/// is the verified `ArtifactStore::read_verified_range` call after arguments
/// have been normalized.
#[derive(Clone)]
pub struct ArtifactReadAuthority {
    store: ArtifactStore,
}

/// Executor spelling retained for callers that use an execution-oriented
/// name.
#[deprecated(note = "use ArtifactReadAuthority instead")]
pub type ArtifactReadExecutor = ArtifactReadAuthority;

impl ArtifactReadAuthority {
    pub fn new(store: ArtifactStore) -> Self {
        Self { store }
    }

    /// Construct the authority from an artifact store.
    #[deprecated(note = "use ArtifactReadAuthority::new instead")]
    pub fn from_store(store: ArtifactStore) -> Self {
        Self::new(store)
    }

    /// Return the canonical capability schema.
    #[deprecated(note = "use capability_schema instead")]
    pub fn schema(&self) -> CapabilitySchema {
        capability_schema()
    }

    /// Return the stateless strict argument normalizer.
    #[deprecated(note = "construct ArtifactReadNormalizer directly instead")]
    pub const fn normalizer(&self) -> ArtifactReadNormalizer {
        ArtifactReadNormalizer
    }

    /// Normalize model arguments before execution.
    #[deprecated(note = "use ArtifactReadNormalizer::normalize instead")]
    pub fn normalize<T>(&self, arguments: T) -> Result<ArtifactReadResource, ArtifactReadError>
    where
        T: Borrow<Value>,
    {
        ArtifactReadNormalizer.normalize(arguments)
    }

    /// Alias for [`Self::normalize`].
    #[deprecated(note = "use ArtifactReadNormalizer::normalize instead")]
    pub fn normalize_arguments<T>(
        &self,
        arguments: T,
    ) -> Result<ArtifactReadResource, ArtifactReadError>
    where
        T: Borrow<Value>,
    {
        ArtifactReadNormalizer.normalize(arguments)
    }

    /// Normalize and execute one model argument object.
    #[deprecated(note = "normalize explicitly, then call execute instead")]
    pub fn invoke<T>(&self, arguments: T) -> ArtifactReadResult
    where
        T: Borrow<Value>,
    {
        match ArtifactReadNormalizer.normalize(arguments) {
            Ok(resource) => self.execute(&resource),
            Err(error) => ArtifactReadResult::error(error),
        }
    }

    pub fn execute(&self, resource: &ArtifactReadResource) -> ArtifactReadResult {
        if !valid_read_length(resource.length) {
            return ArtifactReadResult::error(ArtifactReadError::invalid_arguments());
        }
        let reference = resource.reference.clone();
        let metadata = match self.store.metadata(&reference) {
            Ok(metadata) => metadata,
            Err(error) => return ArtifactReadResult::error(map_store_error(error, reference)),
        };

        let length = match usize::try_from(resource.length) {
            Ok(length) => length,
            Err(_) => return ArtifactReadResult::error(ArtifactReadError::invalid_arguments()),
        };
        let verified = match self
            .store
            .read_verified_range(&reference, resource.offset, length)
        {
            Ok(verified) => verified,
            Err(error) => return ArtifactReadResult::error(map_store_error(error, reference)),
        };
        let total_bytes = verified.total_bytes();
        if metadata.bytes != total_bytes {
            return ArtifactReadResult::error(ArtifactReadError::integrity(reference));
        }
        if resource.offset > total_bytes {
            return ArtifactReadResult::error(ArtifactReadError::range_out_of_bounds(reference));
        }

        let returned_bytes = verified.bytes().len() as u64;
        let eof = resource.offset + returned_bytes == total_bytes;

        ArtifactReadResult::success(ArtifactReadSuccess {
            reference,
            offset: resource.offset,
            requested_bytes: resource.length,
            returned_bytes,
            total_bytes,
            eof,
            data: BASE64.encode(verified.bytes()),
        })
    }

    /// Normalize and execute one model argument object.
    #[deprecated(note = "normalize explicitly, then call execute instead")]
    pub fn execute_arguments<T>(&self, arguments: T) -> ArtifactReadResult
    where
        T: Borrow<Value>,
    {
        match ArtifactReadNormalizer.normalize(arguments) {
            Ok(resource) => self.execute(&resource),
            Err(error) => ArtifactReadResult::error(error),
        }
    }

    /// Execute a normalized resource.
    #[deprecated(note = "use execute instead")]
    pub fn read(&self, resource: &ArtifactReadResource) -> ArtifactReadResult {
        self.execute(resource)
    }
}

fn map_store_error(error: ArtifactStoreError, reference: ArtifactRef) -> ArtifactReadError {
    match error {
        ArtifactStoreError::Integrity { .. } => ArtifactReadError::integrity(reference),
        ArtifactStoreError::InvalidReference(_) => ArtifactReadError::invalid_reference(),
        ArtifactStoreError::Metadata(_) | ArtifactStoreError::MetadataMismatch(_) => {
            ArtifactReadError::integrity(reference)
        }
        ArtifactStoreError::Io(_)
        | ArtifactStoreError::TooLarge { .. }
        | ArtifactStoreError::NotRegularFile(_) => ArtifactReadError::unavailable(reference),
    }
}

#[cfg(test)]
mod tests {
    use ditto_capability::{
        CapabilityKind, CapabilityLifecycle, CapabilityManifest, DataAccess, EffectProfile,
        EffectSpec, PlacementSpec, PolicySpec, RetrievalSpec, RuntimeSpec, VerificationSpec,
        validate_json_schema,
    };
    use serde_json::Value;
    use tempfile::tempdir;

    use super::{
        ARTIFACT_READ_ID, ARTIFACT_READ_IDLE_TTL_MS, ARTIFACT_READ_VERIFICATION,
        ARTIFACT_READ_VERSION, ARTIFACT_RESOURCE_TEMPLATE, ArtifactReadArguments,
        ArtifactReadAuthority, ArtifactReadErrorCode, ArtifactReadManifestError,
        ArtifactReadNormalizer, ArtifactReadResource, ArtifactReadResult, CAPABILITY_SUMMARY,
        MAX_READ_BYTES, RETRIEVAL_ALIASES, RETRIEVAL_COMPLEMENTS, RETRIEVAL_INTENTS,
        RETRIEVAL_NEGATIVE_EXAMPLES, capability_schema, input_schema, output_schema,
        validate_artifact_read_manifest,
    };

    fn canonical_manifest() -> CapabilityManifest {
        CapabilityManifest {
            id: ARTIFACT_READ_ID.to_owned(),
            version: ARTIFACT_READ_VERSION.to_owned(),
            namespace: "artifact".to_owned(),
            kind: CapabilityKind::Tool,
            lifecycle: CapabilityLifecycle::Active,
            summary: CAPABILITY_SUMMARY.to_owned(),
            runtime: RuntimeSpec {
                runtime_type: ditto_capability::RuntimeType::Builtin,
                command: None,
                lazy: true,
                idle_ttl_ms: ARTIFACT_READ_IDLE_TTL_MS,
            },
            placement: PlacementSpec {
                modes: vec!["local".to_owned()],
                requires: Vec::new(),
            },
            retrieval: RetrievalSpec {
                intents: RETRIEVAL_INTENTS.iter().map(ToString::to_string).collect(),
                negative_examples: RETRIEVAL_NEGATIVE_EXAMPLES
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
                complements: RETRIEVAL_COMPLEMENTS
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
                aliases: RETRIEVAL_ALIASES.iter().map(ToString::to_string).collect(),
            },
            effects: EffectSpec {
                minimum: EffectProfile::read_content(),
                maximum: EffectProfile::read_content(),
                resources: vec![ARTIFACT_RESOURCE_TEMPLATE.to_owned()],
            },
            policy: PolicySpec {
                approval: Some("never".to_owned()),
                secret_handles: Vec::new(),
            },
            verification: VerificationSpec {
                default: Some(ARTIFACT_READ_VERIFICATION.to_owned()),
            },
        }
    }

    fn authority() -> (tempfile::TempDir, ArtifactReadAuthority) {
        let directory = tempdir().expect("temporary directory");
        let store = ditto_artifact_store::ArtifactStore::open(directory.path())
            .expect("open artifact store");
        (directory, ArtifactReadAuthority::new(store))
    }

    fn invoke(authority: &ArtifactReadAuthority, arguments: Value) -> ArtifactReadResult {
        match ArtifactReadNormalizer.normalize(arguments) {
            Ok(resource) => authority.execute(&resource),
            Err(error) => ArtifactReadResult::error(error),
        }
    }

    #[test]
    fn schema_is_exact_and_provider_neutral() {
        let schema = capability_schema();
        assert_eq!(schema.id, ARTIFACT_READ_ID);
        assert_eq!(schema.version, ARTIFACT_READ_VERSION);
        schema.validate().expect("capability schema is valid");
        validate_json_schema(&input_schema()).expect("input schema is valid");
        validate_json_schema(&output_schema()).expect("output schema is valid");

        let input = input_schema();
        assert_eq!(input["additionalProperties"], false);
        assert_eq!(
            input["required"],
            serde_json::json!(["reference", "offset", "length"])
        );
        assert_eq!(
            input["properties"]["reference"]["pattern"],
            "^artifact:sha256:[0-9a-f]{64}$"
        );
        assert_eq!(input["properties"]["offset"]["maximum"], u64::MAX);
        assert_eq!(input["properties"]["length"]["maximum"], MAX_READ_BYTES);
    }

    #[test]
    fn canonical_manifest_is_accepted_and_contract_mutations_are_rejected() {
        assert!(validate_artifact_read_manifest(&canonical_manifest()).is_ok());

        let mut manifest = canonical_manifest();
        manifest.namespace = "other".to_owned();
        assert_eq!(
            validate_artifact_read_manifest(&manifest),
            Err(ArtifactReadManifestError::Mismatch { field: "namespace" })
        );

        let mut manifest = canonical_manifest();
        manifest.runtime.lazy = false;
        assert_eq!(
            validate_artifact_read_manifest(&manifest),
            Err(ArtifactReadManifestError::Mismatch {
                field: "runtime.lazy"
            })
        );

        let mut manifest = canonical_manifest();
        manifest.placement.requires.push("network".to_owned());
        assert_eq!(
            validate_artifact_read_manifest(&manifest),
            Err(ArtifactReadManifestError::Mismatch {
                field: "placement.requires"
            })
        );

        let mut manifest = canonical_manifest();
        manifest.retrieval.intents.swap(0, 1);
        assert_eq!(
            validate_artifact_read_manifest(&manifest),
            Err(ArtifactReadManifestError::Mismatch {
                field: "retrieval.intents"
            })
        );

        let mut manifest = canonical_manifest();
        manifest.retrieval.negative_examples[0] = "read a file".to_owned();
        assert_eq!(
            validate_artifact_read_manifest(&manifest),
            Err(ArtifactReadManifestError::Mismatch {
                field: "retrieval.negative_examples"
            })
        );

        let mut manifest = canonical_manifest();
        manifest.retrieval.aliases.reverse();
        assert_eq!(
            validate_artifact_read_manifest(&manifest),
            Err(ArtifactReadManifestError::Mismatch {
                field: "retrieval.aliases"
            })
        );

        let mut manifest = canonical_manifest();
        manifest
            .retrieval
            .complements
            .push("other.capability".to_owned());
        assert_eq!(
            validate_artifact_read_manifest(&manifest),
            Err(ArtifactReadManifestError::Mismatch {
                field: "retrieval.complements"
            })
        );

        let mut manifest = canonical_manifest();
        manifest.effects.maximum.access = DataAccess::Metadata;
        assert_eq!(
            validate_artifact_read_manifest(&manifest),
            Err(ArtifactReadManifestError::Mismatch {
                field: "effects.maximum"
            })
        );

        let mut manifest = canonical_manifest();
        manifest.effects.resources[0] = "artifact:any".to_owned();
        assert_eq!(
            validate_artifact_read_manifest(&manifest),
            Err(ArtifactReadManifestError::Mismatch {
                field: "effects.resources"
            })
        );

        let mut manifest = canonical_manifest();
        manifest.policy.approval = Some("risk-based".to_owned());
        assert_eq!(
            validate_artifact_read_manifest(&manifest),
            Err(ArtifactReadManifestError::Mismatch {
                field: "policy.approval"
            })
        );

        let mut manifest = canonical_manifest();
        manifest.verification.default = None;
        assert_eq!(
            validate_artifact_read_manifest(&manifest),
            Err(ArtifactReadManifestError::Mismatch {
                field: "verification.default"
            })
        );
    }

    #[test]
    fn wire_argument_serialization_rejects_invalid_lengths() {
        let reference =
            ditto_artifact_store::ArtifactRef::from_sha256("a".repeat(64)).expect("reference");
        let arguments = ArtifactReadArguments {
            reference: reference.clone(),
            offset: 0,
            length: 0,
        };
        assert!(serde_json::to_value(&arguments).is_err());
        let resource = ArtifactReadResource {
            reference,
            offset: 0,
            length: MAX_READ_BYTES as u64 + 1,
        };
        assert!(serde_json::to_value(&resource).is_err());
    }

    #[test]
    fn normalizer_rejects_malformed_shape_before_storage() {
        let normalizer = ArtifactReadNormalizer;
        let reference = format!("artifact:sha256:{}", "a".repeat(64));
        let valid = serde_json::json!({
            "reference": reference,
            "offset": 0,
            "length": 1,
        });
        normalizer.normalize(&valid).expect("valid arguments");

        for invalid in [
            serde_json::json!({"reference": reference, "offset": 0, "length": 1, "extra": true}),
            serde_json::json!({"reference": reference, "offset": 0}),
            serde_json::json!({"reference": reference, "offset": -1, "length": 1}),
            serde_json::json!({"reference": reference, "offset": 0, "length": 0}),
            serde_json::json!({"reference": reference, "offset": 0, "length": MAX_READ_BYTES + 1}),
            serde_json::json!({"reference": 4, "offset": 0, "length": 1}),
        ] {
            let error = normalizer
                .normalize(invalid)
                .expect_err("reject malformed arguments");
            assert_eq!(error.code, "invalid_arguments");
            assert!(!error.message.contains("artifact:sha256:"));
        }

        let malformed = serde_json::json!({
            "reference": "../../state.db",
            "offset": 0,
            "length": 1,
        });
        let error = normalizer
            .normalize(malformed)
            .expect_err("reject malformed reference");
        assert_eq!(error.code, "invalid_reference");
        assert!(error.reference.is_none());
    }

    #[test]
    fn result_projection_round_trips_binary_success_and_errors() {
        let reference =
            ditto_artifact_store::ArtifactRef::from_sha256("a".repeat(64)).expect("reference");
        let success = ArtifactReadResult::success(super::ArtifactReadSuccess {
            reference: reference.clone(),
            offset: 2,
            requested_bytes: 4,
            returned_bytes: 4,
            total_bytes: 10,
            eof: false,
            data: "AP+A/w==".into(),
        });
        let encoded = serde_json::to_value(&success).expect("serialize success");
        assert_eq!(encoded["is_error"], false);
        assert_eq!(encoded["data"], "AP+A/w==");
        let decoded: ArtifactReadResult = serde_json::from_value(encoded).expect("decode success");
        assert_eq!(decoded, success);

        let error = ArtifactReadResult::error(super::ArtifactReadError::range_out_of_bounds(
            reference.clone(),
        ));
        let encoded = serde_json::to_value(&error).expect("serialize error");
        assert_eq!(encoded["is_error"], true);
        assert_eq!(encoded["error"]["code"], "range_out_of_bounds");
        assert_eq!(encoded["error"]["reference"], reference.to_string());
        let decoded: ArtifactReadResult = serde_json::from_value(encoded).expect("decode error");
        assert_eq!(decoded, error);
        assert_eq!(
            error.error_projection().and_then(|value| value.code_kind()),
            Some(ArtifactReadErrorCode::RangeOutOfBounds)
        );
    }

    #[test]
    fn store_error_projection_never_exposes_path_or_calculated_hash() {
        let (_directory, authority) = authority();
        let reference = format!("artifact:sha256:{}", "b".repeat(64));
        let result = invoke(
            &authority,
            serde_json::json!({
                "reference": reference,
                "offset": 0,
                "length": 1,
            }),
        );
        let encoded = serde_json::to_string(&result).expect("serialize unavailable result");
        assert!(result.is_error());
        assert!(encoded.contains("artifact_unavailable"));
        assert!(!encoded.contains("sha256/"));
    }
}

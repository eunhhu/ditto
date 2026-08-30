//! The builtin, read-only `artifact.read` capability.
//!
//! This crate deliberately owns only the capability contract and its bounded
//! local executor.  It accepts canonical content-addressed artifact
//! references, never paths, and turns store failures into a small stable error
//! projection.  The semantic kernel remains responsible for scope checks
//! against the event spine and for durable tool-call ordering.

use std::{borrow::Borrow, fmt};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use ditto_artifact_store::{ArtifactRef, ArtifactStore, ArtifactStoreError};
use ditto_capability::{CapabilitySchema, JSON_SCHEMA_DRAFT_2020_12_URI};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use serde_json::{Value, json};

/// Stable identifier of the builtin capability.
pub const ARTIFACT_READ_ID: &str = "artifact.read";
/// Version of the builtin capability contract.
pub const ARTIFACT_READ_VERSION: &str = "0.1.0";
/// Maximum number of bytes requested by one invocation.
pub const MAX_READ_BYTES: usize = 16 * 1024;

const REFERENCE_PATTERN: &str = r"^artifact:sha256:[0-9a-f]{64}$";
const INVALID_ARGUMENTS_MESSAGE: &str = "artifact.read arguments are invalid";
const INVALID_REFERENCE_MESSAGE: &str = "artifact reference is invalid";
const RANGE_MESSAGE: &str = "artifact offset is beyond the end of the artifact";
const UNAVAILABLE_MESSAGE: &str = "artifact is unavailable";
const INTEGRITY_MESSAGE: &str = "artifact integrity verification failed";

/// Wire arguments for `artifact.read`.
///
/// Deserialization is intentionally strict: all three fields are required,
/// unknown fields are rejected, references are canonicalized by
/// [`ArtifactRef`], and the length bound is enforced before an executor can
/// touch the store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArtifactReadArguments {
    pub reference: ArtifactRef,
    pub offset: u64,
    pub length: u64,
}

impl ArtifactReadArguments {
    pub fn new(
        reference: ArtifactRef,
        offset: u64,
        length: u64,
    ) -> Result<Self, ArtifactReadError> {
        if length == 0 || length > MAX_READ_BYTES as u64 {
            return Err(ArtifactReadError::invalid_arguments());
        }
        Ok(Self {
            reference,
            offset,
            length,
        })
    }

    pub fn normalize(self) -> ArtifactReadResource {
        ArtifactReadResource {
            reference: self.reference,
            offset: self.offset,
            length: self.length,
        }
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
        if wire.length == 0 || wire.length > MAX_READ_BYTES as u64 {
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArtifactReadResource {
    pub reference: ArtifactRef,
    pub offset: u64,
    pub length: u64,
}

impl ArtifactReadResource {
    pub fn new(
        reference: ArtifactRef,
        offset: u64,
        length: u64,
    ) -> Result<Self, ArtifactReadError> {
        ArtifactReadArguments::new(reference, offset, length).map(ArtifactReadArguments::normalize)
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
    pub code: String,
    pub message: String,
    pub reference: Option<ArtifactRef>,
}

impl ArtifactReadError {
    pub fn new(
        code: ArtifactReadErrorCode,
        _message: &'static str,
        reference: Option<ArtifactRef>,
    ) -> Self {
        Self {
            code: code.as_str().to_owned(),
            message: Self::expected_message(code).to_owned(),
            reference,
        }
    }

    pub fn invalid_arguments() -> Self {
        Self::new(
            ArtifactReadErrorCode::InvalidArguments,
            INVALID_ARGUMENTS_MESSAGE,
            None,
        )
    }

    pub fn invalid_reference() -> Self {
        Self::new(
            ArtifactReadErrorCode::InvalidReference,
            INVALID_REFERENCE_MESSAGE,
            None,
        )
    }

    pub fn range_out_of_bounds(reference: ArtifactRef) -> Self {
        Self::new(
            ArtifactReadErrorCode::RangeOutOfBounds,
            RANGE_MESSAGE,
            Some(reference),
        )
    }

    /// Creates the stable scope-denial projection used when the kernel has no
    /// task/session root for the canonical reference.
    pub fn not_authorized(reference: ArtifactRef) -> Self {
        Self::new(
            ArtifactReadErrorCode::UnauthorizedReference,
            "artifact reference is not authorized for this turn",
            Some(reference),
        )
    }

    /// Alias for [`not_authorized`](Self::not_authorized).
    pub fn unauthorized_reference(reference: ArtifactRef) -> Self {
        Self::not_authorized(reference)
    }

    fn unavailable(reference: ArtifactRef) -> Self {
        Self::new(
            ArtifactReadErrorCode::ArtifactUnavailable,
            UNAVAILABLE_MESSAGE,
            Some(reference),
        )
    }

    fn integrity(reference: ArtifactRef) -> Self {
        Self::new(
            ArtifactReadErrorCode::IntegrityFailure,
            INTEGRITY_MESSAGE,
            Some(reference),
        )
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
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            code: String,
            message: String,
            #[serde(default)]
            reference: Option<String>,
        }

        let wire = Wire::deserialize(deserializer)?;
        let Some(code) = (match wire.code.as_str() {
            "invalid_arguments" => Some(ArtifactReadErrorCode::InvalidArguments),
            "invalid_reference" => Some(ArtifactReadErrorCode::InvalidReference),
            "range_out_of_bounds" => Some(ArtifactReadErrorCode::RangeOutOfBounds),
            "artifact_unavailable" => Some(ArtifactReadErrorCode::ArtifactUnavailable),
            "integrity_failure" => Some(ArtifactReadErrorCode::IntegrityFailure),
            "unauthorized_reference" => Some(ArtifactReadErrorCode::UnauthorizedReference),
            _ => None,
        }) else {
            return Err(D::Error::custom(
                "artifact.read error projection is invalid",
            ));
        };
        let reference = wire.reference.map(|value| {
            ArtifactRef::new(value)
                .map_err(|_| D::Error::custom("artifact.read error projection is invalid"))
        });
        let reference = match reference {
            Some(Ok(reference)) => Some(reference),
            Some(Err(error)) => return Err(error),
            None => None,
        };
        let error = Self::new(code, Self::expected_message(code), reference);
        if wire.message != error.message
            || Self::requires_reference(code) != error.reference.is_some()
        {
            return Err(D::Error::custom(
                "artifact.read error projection is invalid",
            ));
        }
        Ok(error)
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
    pub reference: ArtifactRef,
    pub offset: u64,
    pub requested_bytes: u64,
    pub returned_bytes: u64,
    pub total_bytes: u64,
    pub eof: bool,
    pub data: String,
}

impl ArtifactReadSuccess {
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
pub struct ArtifactReadResult {
    /// `false` for a successful projection and `true` for an error result.
    pub is_error: bool,
    success: Option<ArtifactReadSuccess>,
    error: Option<ArtifactReadError>,
}

impl ArtifactReadResult {
    pub fn success(value: ArtifactReadSuccess) -> Self {
        Self {
            is_error: false,
            success: Some(value),
            error: None,
        }
    }

    pub fn error(value: ArtifactReadError) -> Self {
        Self {
            is_error: true,
            success: None,
            error: Some(value),
        }
    }

    pub fn is_error(&self) -> bool {
        self.is_error
    }

    pub fn success_projection(&self) -> Option<&ArtifactReadSuccess> {
        self.success.as_ref()
    }

    pub fn error_projection(&self) -> Option<&ArtifactReadError> {
        self.error.as_ref()
    }

    pub fn into_success(self) -> Option<ArtifactReadSuccess> {
        self.success
    }

    pub fn into_error(self) -> Option<ArtifactReadError> {
        self.error
    }
}

impl Serialize for ArtifactReadResult {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if self.is_error {
            let Some(error) = &self.error else {
                return Err(serde::ser::Error::custom(
                    "artifact.read error result has no error projection",
                ));
            };
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
        } else {
            let Some(success) = &self.success else {
                return Err(serde::ser::Error::custom(
                    "artifact.read success result has no success projection",
                ));
            };
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

    pub fn parse_json(&self, input: &str) -> Result<ArtifactReadResource, ArtifactReadError> {
        let value: Value =
            serde_json::from_str(input).map_err(|_| ArtifactReadError::invalid_arguments())?;
        self.normalize(value)
    }

    pub fn parse_bytes(&self, input: &[u8]) -> Result<ArtifactReadResource, ArtifactReadError> {
        let value: Value =
            serde_json::from_slice(input).map_err(|_| ArtifactReadError::invalid_arguments())?;
        self.normalize(value)
    }
}

/// Normalize one JSON argument object without constructing an executor.
pub fn normalize_arguments<T>(arguments: T) -> Result<ArtifactReadResource, ArtifactReadError>
where
    T: Borrow<Value>,
{
    ArtifactReadNormalizer.normalize(arguments)
}

/// Short spelling for [`normalize_arguments`].
pub fn normalize<T>(arguments: T) -> Result<ArtifactReadResource, ArtifactReadError>
where
    T: Borrow<Value>,
{
    normalize_arguments(arguments)
}

/// Returns the complete provider-neutral level-2 capability schema.
pub fn capability_schema() -> CapabilitySchema {
    CapabilitySchema {
        id: ARTIFACT_READ_ID.to_owned(),
        version: ARTIFACT_READ_VERSION.to_owned(),
        summary: "Read a bounded range from a content-addressed artifact.".to_owned(),
        input_schema: input_schema(),
        output_schema: output_schema(),
    }
}

/// Alias for [`capability_schema`].
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
                    "offset": {"type": "integer", "minimum": 0},
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
                    "total_bytes": {"type": "integer", "minimum": 0},
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
                    "error": {
                        "type": "object",
                        "properties": {
                            "code": {
                                "type": "string",
                                "enum": [
                                    "invalid_arguments",
                                    "invalid_reference",
                                    "range_out_of_bounds",
                                    "artifact_unavailable",
                                    "integrity_failure",
                                    "unauthorized_reference",
                                ],
                            },
                            "message": {"type": "string"},
                            "reference": {
                                "type": "string",
                                "pattern": REFERENCE_PATTERN,
                            },
                        },
                        "required": ["code", "message"],
                        "additionalProperties": false,
                    },
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
/// is the verified `ArtifactStore::read_range` call after metadata and range
/// checks have passed.
#[derive(Clone)]
pub struct ArtifactReadAuthority {
    store: ArtifactStore,
    normalizer: ArtifactReadNormalizer,
}

/// Executor spelling retained for callers that use an execution-oriented
/// name.  Both names refer to the same narrow authority.
pub type ArtifactReadExecutor = ArtifactReadAuthority;

impl ArtifactReadAuthority {
    pub fn new(store: ArtifactStore) -> Self {
        Self {
            store,
            normalizer: ArtifactReadNormalizer,
        }
    }

    pub fn from_store(store: ArtifactStore) -> Self {
        Self::new(store)
    }

    pub fn schema(&self) -> CapabilitySchema {
        capability_schema()
    }

    pub fn normalizer(&self) -> ArtifactReadNormalizer {
        self.normalizer
    }

    pub fn normalize<T>(&self, arguments: T) -> Result<ArtifactReadResource, ArtifactReadError>
    where
        T: Borrow<Value>,
    {
        self.normalizer.normalize(arguments)
    }

    pub fn normalize_arguments<T>(
        &self,
        arguments: T,
    ) -> Result<ArtifactReadResource, ArtifactReadError>
    where
        T: Borrow<Value>,
    {
        self.normalize(arguments)
    }

    pub fn invoke<T>(&self, arguments: T) -> ArtifactReadResult
    where
        T: Borrow<Value>,
    {
        match self.normalize(arguments) {
            Ok(resource) => self.execute(&resource),
            Err(error) => ArtifactReadResult::error(error),
        }
    }

    pub fn execute(&self, resource: &ArtifactReadResource) -> ArtifactReadResult {
        if resource.length == 0 || resource.length > MAX_READ_BYTES as u64 {
            return ArtifactReadResult::error(ArtifactReadError::invalid_arguments());
        }
        let reference = resource.reference.clone();
        let metadata = match self.store.metadata(&reference) {
            Ok(metadata) => metadata,
            Err(error) => return ArtifactReadResult::error(map_store_error(error, reference)),
        };

        if resource.offset > metadata.bytes {
            // Verify the immutable object even for a rejected range.  This
            // keeps every authority invocation tamper-detecting, including
            // callers probing beyond the recorded EOF.
            let length = usize::try_from(resource.length)
                .expect("artifact.read length is bounded by MAX_READ_BYTES");
            if let Err(error) = self.store.read_range(&reference, resource.offset, length) {
                return ArtifactReadResult::error(map_store_error(error, reference));
            }
            return ArtifactReadResult::error(ArtifactReadError::range_out_of_bounds(reference));
        }

        // `read_range` verifies the complete object through the same open file
        // descriptor even for an empty range, preserving tamper detection at
        // EOF while keeping the returned projection bounded.
        let length = usize::try_from(resource.length)
            .expect("artifact.read length is bounded by MAX_READ_BYTES");
        let bytes = match self.store.read_range(&reference, resource.offset, length) {
            Ok(bytes) => bytes,
            Err(error) => return ArtifactReadResult::error(map_store_error(error, reference)),
        };
        let returned_bytes = bytes.len() as u64;
        let eof = resource.offset == metadata.bytes
            || returned_bytes == metadata.bytes.saturating_sub(resource.offset);

        ArtifactReadResult::success(ArtifactReadSuccess {
            reference,
            offset: resource.offset,
            requested_bytes: resource.length,
            returned_bytes,
            total_bytes: metadata.bytes,
            eof,
            data: BASE64.encode(bytes),
        })
    }

    pub fn execute_arguments<T>(&self, arguments: T) -> ArtifactReadResult
    where
        T: Borrow<Value>,
    {
        self.invoke(arguments)
    }

    pub fn read(&self, resource: &ArtifactReadResource) -> ArtifactReadResult {
        self.execute(resource)
    }
}

fn map_store_error(error: ArtifactStoreError, reference: ArtifactRef) -> ArtifactReadError {
    match error {
        ArtifactStoreError::Integrity { .. } => ArtifactReadError::integrity(reference),
        ArtifactStoreError::InvalidReference(_) => ArtifactReadError::invalid_reference(),
        ArtifactStoreError::MetadataMismatch(_) => ArtifactReadError::integrity(reference),
        ArtifactStoreError::Io(_)
        | ArtifactStoreError::Metadata(_)
        | ArtifactStoreError::TooLarge { .. }
        | ArtifactStoreError::NotRegularFile(_) => ArtifactReadError::unavailable(reference),
    }
}

#[cfg(test)]
mod tests {
    use ditto_capability::validate_json_schema;
    use tempfile::tempdir;

    use super::{
        ARTIFACT_READ_ID, ARTIFACT_READ_VERSION, ArtifactReadAuthority, ArtifactReadErrorCode,
        ArtifactReadNormalizer, ArtifactReadResult, MAX_READ_BYTES, capability_schema,
        input_schema, output_schema,
    };

    fn authority() -> (tempfile::TempDir, ArtifactReadAuthority) {
        let directory = tempdir().expect("temporary directory");
        let store = ditto_artifact_store::ArtifactStore::open(directory.path())
            .expect("open artifact store");
        (directory, ArtifactReadAuthority::new(store))
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
        assert_eq!(input["properties"]["length"]["maximum"], MAX_READ_BYTES);
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
        let result = authority.invoke(serde_json::json!({
            "reference": reference,
            "offset": 0,
            "length": 1,
        }));
        let encoded = serde_json::to_string(&result).expect("serialize unavailable result");
        assert!(result.is_error());
        assert!(encoded.contains("artifact_unavailable"));
        assert!(!encoded.contains("sha256/"));
    }
}

use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

use ditto_retrieval::{
    CapabilityRootLimit, EmbeddingProvider, ExecutionEpochLimit, MAX_CANDIDATE_COUNT,
    MAX_RETRIEVAL_DOCUMENT_BYTES, MAX_RETRIEVAL_IDENTIFIER_BYTES, RetrievalDocument,
    RetrievalError, RetrievalMode, RetrievalWorkBudget, TaskQuery, canonical_exact_identity,
    cosine_similarity,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use ulid::Ulid;

mod invocation;
mod schema_instance;

pub use invocation::{
    ArgumentStage, ArtifactResourceId, CanonicalInvocation, CanonicalPathResource,
    CanonicalPathRoot, CanonicalResource, CanonicalResourceError, CapabilityDeriver,
    CapabilityRevision, CapabilityRevisionError, DerivationBudget, DeriverError, DeriverRevision,
    IdempotencyKey, InvocationCompiler, InvocationDigest, InvocationError, InvocationId,
    ManifestDigest, ResolvedPlacement, SchemaDigest, ToolCallId, UntrustedToolCall,
    UntrustedToolCallError, canonical_manifest_digest, canonical_schema_digest,
};
pub use schema_instance::{
    JsonSchemaInstanceError, MAX_INVOCATION_ARGUMENT_BYTES, MAX_INVOCATION_SCHEMA_BYTES,
    MAX_SCHEMA_INSTANCE_DEPTH, MAX_SCHEMA_INSTANCE_WORK, validate_json_schema_instance,
};

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum DataAccess {
    #[default]
    None,
    Metadata,
    Content,
    Credentials,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum Mutation {
    #[default]
    None,
    Reversible,
    Irreversible,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum Externality {
    #[default]
    Local,
    Network,
    HumanCommunication,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum Privilege {
    #[default]
    User,
    Elevated,
}

/// Orthogonal effect dimensions. Privilege never implies mutation, communication,
/// or credential access.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EffectProfile {
    #[serde(default)]
    pub access: DataAccess,
    #[serde(default)]
    pub mutation: Mutation,
    #[serde(default)]
    pub externality: Externality,
    #[serde(default)]
    pub privilege: Privilege,
}

impl EffectProfile {
    pub const fn permits(self, claimed: Self) -> bool {
        self.access as u8 >= claimed.access as u8
            && self.mutation as u8 >= claimed.mutation as u8
            && self.externality as u8 >= claimed.externality as u8
            && self.privilege as u8 >= claimed.privilege as u8
    }

    pub const fn read_content() -> Self {
        Self {
            access: DataAccess::Content,
            mutation: Mutation::None,
            externality: Externality::Local,
            privilege: Privilege::User,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CapabilityKind {
    Tool,
    Skill,
    Resource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeType {
    Builtin,
    Process,
    Wasi,
    Mcp,
    Remote,
}

/// Administrative lifecycle controlling whether an installed capability may
/// enter a model-visible catalogue or runtime working set.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CapabilityLifecycle {
    #[default]
    Active,
    Retired,
    Quarantined,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityManifest {
    pub id: String,
    pub version: String,
    pub namespace: String,
    pub kind: CapabilityKind,
    #[serde(default)]
    pub lifecycle: CapabilityLifecycle,
    pub summary: String,
    pub runtime: RuntimeSpec,
    #[serde(default)]
    pub placement: PlacementSpec,
    #[serde(default)]
    pub retrieval: RetrievalSpec,
    pub effects: EffectSpec,
    #[serde(default)]
    pub policy: PolicySpec,
    #[serde(default)]
    pub verification: VerificationSpec,
}

/// Build the canonical bounded V2 capability retrieval document.
pub fn capability_retrieval_document(
    manifest: &CapabilityManifest,
) -> Result<RetrievalDocument, RetrievalError> {
    let document_len = capability_retrieval_document_len(manifest)?;
    let mut aliases = manifest
        .retrieval
        .aliases
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    aliases.sort_unstable_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
    let mut intents = manifest
        .retrieval
        .intents
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    intents.sort_unstable_by(|left, right| left.as_bytes().cmp(right.as_bytes()));

    let mut document = String::with_capacity(document_len);
    document.push_str("id=");
    document.push_str(&manifest.id);
    document.push_str("\nnamespace=");
    document.push_str(&manifest.namespace);
    document.push_str("\nsummary=");
    document.push_str(&manifest.summary);
    for alias in aliases {
        document.push_str("\nalias=");
        document.push_str(alias);
    }
    for intent in intents {
        document.push_str("\nintent=");
        document.push_str(intent);
    }
    RetrievalDocument::new(document)
}

fn capability_retrieval_document_len(
    manifest: &CapabilityManifest,
) -> Result<usize, RetrievalError> {
    let mut actual = "id="
        .len()
        .saturating_add(manifest.id.len())
        .saturating_add("\nnamespace=".len())
        .saturating_add(manifest.namespace.len())
        .saturating_add("\nsummary=".len())
        .saturating_add(manifest.summary.len());
    for alias in &manifest.retrieval.aliases {
        actual = actual
            .saturating_add("\nalias=".len())
            .saturating_add(alias.len());
    }
    for intent in &manifest.retrieval.intents {
        actual = actual
            .saturating_add("\nintent=".len())
            .saturating_add(intent.len());
    }
    if actual > MAX_RETRIEVAL_DOCUMENT_BYTES {
        return Err(RetrievalError::RetrievalDocumentTooLong {
            actual,
            maximum: MAX_RETRIEVAL_DOCUMENT_BYTES,
        });
    }
    Ok(actual)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeSpec {
    #[serde(rename = "type")]
    pub runtime_type: RuntimeType,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default = "default_true")]
    pub lazy: bool,
    #[serde(default = "default_idle_ttl_ms")]
    pub idle_ttl_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlacementSpec {
    #[serde(default)]
    pub modes: Vec<String>,
    #[serde(default)]
    pub requires: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RetrievalSpec {
    #[serde(default)]
    pub intents: Vec<String>,
    #[serde(default)]
    pub negative_examples: Vec<String>,
    #[serde(default)]
    pub complements: Vec<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EffectSpec {
    #[serde(default)]
    pub minimum: EffectProfile,
    pub maximum: EffectProfile,
    #[serde(default)]
    pub resources: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PolicySpec {
    #[serde(default)]
    pub approval: Option<String>,
    #[serde(default)]
    pub secret_handles: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VerificationSpec {
    #[serde(default)]
    pub default: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityCard {
    pub id: String,
    pub namespace: String,
    pub kind: CapabilityKind,
    pub summary: String,
    pub minimum_effect: EffectProfile,
    pub maximum_effect: EffectProfile,
    pub placement_modes: Vec<String>,
}

impl From<&CapabilityManifest> for CapabilityCard {
    fn from(manifest: &CapabilityManifest) -> Self {
        Self {
            id: manifest.id.clone(),
            namespace: manifest.namespace.clone(),
            kind: manifest.kind,
            summary: manifest.summary.clone(),
            minimum_effect: manifest.effects.minimum,
            maximum_effect: manifest.effects.maximum,
            placement_modes: manifest.placement.modes.clone(),
        }
    }
}

/// Provider-neutral level-2 capability schema exposed to a model driver.
///
/// Validation is structural: boolean schemas are valid, recognized JSON
/// Schema keywords are checked recursively, and unknown keywords are retained
/// as opaque extensions. This does not evaluate instances or claim that a
/// provider-specific tool API accepts a schema (including a boolean schema).
/// In the supported dialect, `required` may be empty, while schema arrays such
/// as `prefixItems` must contain at least one valid schema.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapabilitySchema {
    pub id: String,
    pub version: String,
    pub summary: String,
    pub input_schema: Value,
    pub output_schema: Value,
}

impl CapabilitySchema {
    pub fn validate(&self) -> Result<(), CapabilitySchemaError> {
        if !valid_capability_id(&self.id) {
            return Err(CapabilitySchemaError::InvalidId {
                id: self.id.clone(),
            });
        }
        if !valid_semver(&self.version) {
            return Err(CapabilitySchemaError::InvalidVersion {
                version: self.version.clone(),
            });
        }
        if self.summary.trim().is_empty() {
            return Err(CapabilitySchemaError::EmptySummary);
        }
        validate_schema_field("input_schema", &self.input_schema)?;
        validate_schema_field("output_schema", &self.output_schema)?;
        Ok(())
    }
}

/// Canonical dialect for provider-neutral capability schemas.
pub const JSON_SCHEMA_DRAFT_2020_12_URI: &str = "https://json-schema.org/draft/2020-12/schema";

/// Structural validation failures for a provider-neutral JSON Schema.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum JsonSchemaValidationError {
    #[error("JSON Schema must be a boolean or a Draft 2020-12 schema object")]
    Invalid,
    #[error("JSON Schema declares unsupported dialect: {dialect}")]
    UnsupportedDialect { dialect: String },
}

/// Validate a provider-neutral JSON Schema using the Draft 2020-12 structure.
///
/// An omitted `$schema` is interpreted as Draft 2020-12. Unknown keywords are
/// intentionally opaque extension data; this function does not evaluate
/// instances or certify compatibility with a provider's tool API.
pub fn validate_json_schema(value: &Value) -> Result<(), JsonSchemaValidationError> {
    validate_schema(value)
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CapabilitySchemaError {
    #[error("capability schema id is invalid: {id}")]
    InvalidId { id: String },
    #[error("capability schema version is invalid: {version}")]
    InvalidVersion { version: String },
    #[error("capability schema summary is empty")]
    EmptySummary,
    #[error("capability schema {field} must be a valid provider-neutral JSON Schema")]
    InvalidSchema { field: &'static str },
    #[error("capability schema {field} declares unsupported JSON Schema dialect: {dialect}")]
    UnsupportedDialect {
        field: &'static str,
        dialect: String,
    },
}

fn validate_schema_field(field: &'static str, value: &Value) -> Result<(), CapabilitySchemaError> {
    match validate_json_schema(value) {
        Ok(()) => Ok(()),
        Err(JsonSchemaValidationError::Invalid) => {
            Err(CapabilitySchemaError::InvalidSchema { field })
        }
        Err(JsonSchemaValidationError::UnsupportedDialect { dialect }) => {
            Err(CapabilitySchemaError::UnsupportedDialect { field, dialect })
        }
    }
}

fn validate_schema(value: &Value) -> Result<(), JsonSchemaValidationError> {
    match value {
        Value::Bool(_) => Ok(()),
        Value::Object(object) => validate_schema_object(object),
        Value::Array(_) | Value::Null | Value::Number(_) | Value::String(_) => {
            Err(JsonSchemaValidationError::Invalid)
        }
    }
}

fn validate_schema_object(
    object: &serde_json::Map<String, Value>,
) -> Result<(), JsonSchemaValidationError> {
    if let Some(value) = object.get("$schema") {
        match value {
            Value::String(dialect) if dialect == JSON_SCHEMA_DRAFT_2020_12_URI => {}
            Value::String(dialect) => {
                return Err(JsonSchemaValidationError::UnsupportedDialect {
                    dialect: dialect.clone(),
                });
            }
            _ => return Err(JsonSchemaValidationError::Invalid),
        }
    }
    for keyword in [
        "$id",
        "$ref",
        "$anchor",
        "$dynamicRef",
        "$dynamicAnchor",
        "$comment",
    ] {
        if let Some(value) = object.get(keyword) {
            if !value.is_string() {
                return Err(JsonSchemaValidationError::Invalid);
            }
        }
    }
    if let Some(value) = object.get("$vocabulary") {
        validate_boolean_map(value)?;
    }

    if let Some(value) = object.get("type") {
        if !is_valid_type(value) {
            return Err(JsonSchemaValidationError::Invalid);
        }
    }
    if let Some(value) = object.get("enum") {
        if !value.is_array() {
            return Err(JsonSchemaValidationError::Invalid);
        }
    }
    if let Some(value) = object.get("multipleOf") {
        if !is_positive_number(value) {
            return Err(JsonSchemaValidationError::Invalid);
        }
    }
    for keyword in ["maximum", "exclusiveMaximum", "minimum", "exclusiveMinimum"] {
        if let Some(value) = object.get(keyword) {
            if !is_number(value) {
                return Err(JsonSchemaValidationError::Invalid);
            }
        }
    }
    for keyword in [
        "maxLength",
        "minLength",
        "maxItems",
        "minItems",
        "maxProperties",
        "minProperties",
        "maxContains",
        "minContains",
    ] {
        if let Some(value) = object.get(keyword) {
            if !is_non_negative_integer(value) {
                return Err(JsonSchemaValidationError::Invalid);
            }
        }
    }
    for keyword in ["pattern", "format", "contentEncoding", "contentMediaType"] {
        if let Some(value) = object.get(keyword) {
            if !value.is_string() {
                return Err(JsonSchemaValidationError::Invalid);
            }
        }
    }
    for keyword in ["uniqueItems", "deprecated", "readOnly", "writeOnly"] {
        if let Some(value) = object.get(keyword) {
            if !value.is_boolean() {
                return Err(JsonSchemaValidationError::Invalid);
            }
        }
    }
    for keyword in ["title", "description"] {
        if let Some(value) = object.get(keyword) {
            if !value.is_string() {
                return Err(JsonSchemaValidationError::Invalid);
            }
        }
    }
    if let Some(value) = object.get("examples") {
        if !value.is_array() {
            return Err(JsonSchemaValidationError::Invalid);
        }
    }

    for keyword in ["allOf", "anyOf", "oneOf", "prefixItems"] {
        if let Some(value) = object.get(keyword) {
            validate_schema_array(value, true)?;
        }
    }
    for keyword in [
        "not",
        "if",
        "then",
        "else",
        "contains",
        "propertyNames",
        "contentSchema",
        "unevaluatedItems",
        "unevaluatedProperties",
        "items",
        "additionalProperties",
    ] {
        if let Some(value) = object.get(keyword) {
            validate_schema(value)?;
        }
    }

    if let Some(value) = object.get("required") {
        validate_string_array(value, false)?;
    }
    if let Some(value) = object.get("dependentRequired") {
        validate_dependent_required(value)?;
    }

    for keyword in [
        "$defs",
        "properties",
        "patternProperties",
        "dependentSchemas",
    ] {
        if let Some(value) = object.get(keyword) {
            validate_schema_map(value)?;
        }
    }

    Ok(())
}

fn is_valid_type(value: &Value) -> bool {
    const TYPE_NAMES: [&str; 7] = [
        "null", "boolean", "object", "array", "number", "string", "integer",
    ];

    match value {
        Value::String(name) => TYPE_NAMES.contains(&name.as_str()),
        Value::Array(values) => {
            !values.is_empty()
                && values.iter().all(|value| {
                    value
                        .as_str()
                        .is_some_and(|name| TYPE_NAMES.contains(&name))
                })
                && values
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<HashSet<_>>()
                    .len()
                    == values.len()
        }
        Value::Bool(_) | Value::Null | Value::Number(_) | Value::Object(_) => false,
    }
}

fn is_number(value: &Value) -> bool {
    value.is_number()
}

fn is_positive_number(value: &Value) -> bool {
    value
        .as_f64()
        .is_some_and(|number| number.is_finite() && number > 0.0)
}

fn is_non_negative_integer(value: &Value) -> bool {
    let Value::Number(number) = value else {
        return false;
    };

    number.as_u64().is_some()
        || number
            .as_f64()
            .is_some_and(|number| number.is_finite() && number >= 0.0 && number.fract() == 0.0)
}

fn validate_schema_array(
    value: &Value,
    require_non_empty: bool,
) -> Result<(), JsonSchemaValidationError> {
    let Value::Array(values) = value else {
        return Err(JsonSchemaValidationError::Invalid);
    };
    if require_non_empty && values.is_empty() {
        return Err(JsonSchemaValidationError::Invalid);
    }
    for schema in values {
        validate_schema(schema)?;
    }
    Ok(())
}

fn validate_schema_map(value: &Value) -> Result<(), JsonSchemaValidationError> {
    let Value::Object(object) = value else {
        return Err(JsonSchemaValidationError::Invalid);
    };
    for schema in object.values() {
        validate_schema(schema)?;
    }
    Ok(())
}

fn validate_boolean_map(value: &Value) -> Result<(), JsonSchemaValidationError> {
    let Value::Object(object) = value else {
        return Err(JsonSchemaValidationError::Invalid);
    };
    if object.values().all(Value::is_boolean) {
        Ok(())
    } else {
        Err(JsonSchemaValidationError::Invalid)
    }
}

fn validate_string_array(
    value: &Value,
    require_non_empty: bool,
) -> Result<(), JsonSchemaValidationError> {
    let Value::Array(values) = value else {
        return Err(JsonSchemaValidationError::Invalid);
    };
    if require_non_empty && values.is_empty() {
        return Err(JsonSchemaValidationError::Invalid);
    }

    let mut entries = HashSet::with_capacity(values.len());
    for value in values {
        let Some(entry) = value.as_str() else {
            return Err(JsonSchemaValidationError::Invalid);
        };
        if !entries.insert(entry) {
            return Err(JsonSchemaValidationError::Invalid);
        }
    }
    Ok(())
}

fn validate_dependent_required(value: &Value) -> Result<(), JsonSchemaValidationError> {
    let Value::Object(object) = value else {
        return Err(JsonSchemaValidationError::Invalid);
    };
    for required in object.values() {
        validate_string_array(required, false)?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    #[default]
    Catalogue,
    Runtime,
}

/// Maximum entries accepted by any list-valued runtime search-context field.
pub const MAX_SEARCH_CONTEXT_ENTRIES: usize = 64;
/// Maximum UTF-8 byte length of one runtime search-context value.
pub const MAX_SEARCH_CONTEXT_VALUE_BYTES: usize = MAX_RETRIEVAL_IDENTIFIER_BYTES;

/// Validation failures for model-independent capability placement input.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SearchContextError {
    #[error("runtime search context is missing {field}")]
    MissingRuntimeField { field: &'static str },
    #[error("search context {field} has {actual} entries, exceeding the maximum of {maximum}")]
    TooManyEntries {
        field: &'static str,
        actual: usize,
        maximum: usize,
    },
    #[error("search context {field} contains an empty value")]
    EmptyValue { field: &'static str },
    #[error("search context {field} value is {actual} bytes, exceeding the maximum of {maximum}")]
    ValueTooLong {
        field: &'static str,
        actual: usize,
        maximum: usize,
    },
    #[error("search context {field} contains a non-canonical exact-match value")]
    NonCanonicalValue { field: &'static str },
    #[error("search context {field} contains a duplicate value: {value}")]
    DuplicateValue { field: &'static str, value: String },
    #[error("preferred placement {preferred} is not in available_placements")]
    PreferredPlacementUnavailable { preferred: String },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchContext {
    #[serde(default)]
    pub mode: SearchMode,
    #[serde(default)]
    pub available_placements: Option<Vec<String>>,
    #[serde(default)]
    pub preferred_placement: Option<String>,
    #[serde(default)]
    pub available_requirements: Option<Vec<String>>,
    #[serde(default)]
    pub effect_ceiling: Option<EffectProfile>,
    #[serde(default)]
    pub allowed_capability_ids: Option<Vec<String>>,
}

impl SearchContext {
    pub fn catalogue() -> Self {
        Self::default()
    }

    pub fn runtime(
        available_placements: Vec<String>,
        available_requirements: Vec<String>,
        effect_ceiling: EffectProfile,
    ) -> Self {
        Self {
            mode: SearchMode::Runtime,
            available_placements: Some(available_placements),
            preferred_placement: None,
            available_requirements: Some(available_requirements),
            effect_ceiling: Some(effect_ceiling),
            allowed_capability_ids: None,
        }
    }

    /// Validate bounded placement input before any embedding provider call.
    pub fn validate(&self) -> Result<(), SearchContextError> {
        validate_search_values(
            "available_placements",
            self.available_placements.as_deref(),
            false,
        )?;
        validate_search_values(
            "available_requirements",
            self.available_requirements.as_deref(),
            false,
        )?;
        validate_search_values(
            "allowed_capability_ids",
            self.allowed_capability_ids.as_deref(),
            true,
        )?;
        if let Some(preferred) = self.preferred_placement.as_deref() {
            validate_search_value("preferred_placement", preferred, false)?;
            if self
                .available_placements
                .as_ref()
                .is_none_or(|available| !available.iter().any(|value| value == preferred))
            {
                return Err(SearchContextError::PreferredPlacementUnavailable {
                    preferred: preferred.to_owned(),
                });
            }
        }
        if self.mode == SearchMode::Runtime {
            for (field, present) in [
                ("available_placements", self.available_placements.is_some()),
                (
                    "available_requirements",
                    self.available_requirements.is_some(),
                ),
                ("effect_ceiling", self.effect_ceiling.is_some()),
            ] {
                if !present {
                    return Err(SearchContextError::MissingRuntimeField { field });
                }
            }
        }
        Ok(())
    }
}

fn validate_search_values(
    field: &'static str,
    values: Option<&[String]>,
    allow_wildcard: bool,
) -> Result<(), SearchContextError> {
    let Some(values) = values else {
        return Ok(());
    };
    if values.len() > MAX_SEARCH_CONTEXT_ENTRIES {
        return Err(SearchContextError::TooManyEntries {
            field,
            actual: values.len(),
            maximum: MAX_SEARCH_CONTEXT_ENTRIES,
        });
    }
    let mut seen = HashSet::with_capacity(values.len());
    for value in values {
        validate_search_value(field, value, allow_wildcard)?;
        if !seen.insert(value.as_str()) {
            return Err(SearchContextError::DuplicateValue {
                field,
                value: value.clone(),
            });
        }
    }
    Ok(())
}

fn validate_search_value(
    field: &'static str,
    value: &str,
    allow_wildcard: bool,
) -> Result<(), SearchContextError> {
    if value.is_empty() {
        return Err(SearchContextError::EmptyValue { field });
    }
    if value.len() > MAX_SEARCH_CONTEXT_VALUE_BYTES {
        return Err(SearchContextError::ValueTooLong {
            field,
            actual: value.len(),
            maximum: MAX_SEARCH_CONTEXT_VALUE_BYTES,
        });
    }
    if allow_wildcard && value == "*" {
        return Ok(());
    }
    match canonical_exact_identity(value) {
        Ok(canonical) if canonical == value => Ok(()),
        _ => Err(SearchContextError::NonCanonicalValue { field }),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ExecutionEpoch {
    pub id: String,
    max_working_set: usize,
    capabilities: Vec<CapabilityCard>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    invocation_revisions: Vec<CapabilityRevision>,
}

impl ExecutionEpoch {
    pub fn new(max_working_set: usize) -> Self {
        Self {
            id: Ulid::new().to_string(),
            max_working_set,
            capabilities: Vec::new(),
            invocation_revisions: Vec::new(),
        }
    }

    pub fn page_in(&mut self, cards: impl IntoIterator<Item = CapabilityCard>) -> usize {
        let initial_len = self.capabilities.len();
        let mut existing = self
            .capabilities
            .iter()
            .map(|card| card.id.clone())
            .collect::<HashSet<_>>();
        for card in cards {
            if self.capabilities.len() >= self.max_working_set {
                break;
            }
            if existing.insert(card.id.clone()) {
                self.capabilities.push(card);
            }
        }
        self.capabilities.len() - initial_len
    }

    pub fn capabilities(&self) -> &[CapabilityCard] {
        &self.capabilities
    }

    /// Page in one capability with the exact contract required for live
    /// invocation. Discovery-only cards added through [`Self::page_in`] remain
    /// non-invocable.
    pub fn page_in_invocable(
        &mut self,
        manifest: &CapabilityManifest,
        schema: &CapabilitySchema,
        deriver_revision: DeriverRevision,
    ) -> Result<usize, CapabilityRevisionError> {
        let revision = CapabilityRevision::from_contract(manifest, schema, deriver_revision)?;
        if self.capabilities.iter().any(|card| card.id == manifest.id) {
            return match self.invocation_revision(&manifest.id) {
                Some(existing) if existing == &revision => Ok(0),
                Some(_) | None => Err(CapabilityRevisionError::EpochRevisionConflict {
                    capability_id: manifest.id.clone(),
                }),
            };
        }
        if self.capabilities.len() >= self.max_working_set {
            return Ok(0);
        }
        self.capabilities.push(CapabilityCard::from(manifest));
        self.invocation_revisions.push(revision);
        Ok(1)
    }

    pub fn invocation_revisions(&self) -> &[CapabilityRevision] {
        &self.invocation_revisions
    }

    pub fn invocation_revision(&self, capability_id: &str) -> Option<&CapabilityRevision> {
        self.invocation_revisions
            .iter()
            .find(|revision| revision.capability_id() == capability_id)
    }

    pub fn max_working_set(&self) -> usize {
        self.max_working_set
    }

    pub fn remaining_capacity(&self) -> usize {
        self.max_working_set.saturating_sub(self.capabilities.len())
    }
}

impl<'de> Deserialize<'de> for ExecutionEpoch {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct EpochWire {
            id: String,
            max_working_set: usize,
            capabilities: Vec<CapabilityCard>,
            #[serde(default)]
            invocation_revisions: Vec<CapabilityRevision>,
        }

        let wire = EpochWire::deserialize(deserializer)?;
        if wire.id.trim().is_empty() {
            return Err(serde::de::Error::custom("execution epoch id is empty"));
        }
        if wire.capabilities.len() > wire.max_working_set {
            return Err(serde::de::Error::custom(
                "execution epoch exceeds its working-set limit",
            ));
        }
        let mut ids = HashSet::new();
        if wire
            .capabilities
            .iter()
            .any(|card| !ids.insert(card.id.as_str()))
        {
            return Err(serde::de::Error::custom(
                "execution epoch contains duplicate capabilities",
            ));
        }
        if wire.invocation_revisions.len() > wire.capabilities.len() {
            return Err(serde::de::Error::custom(
                "execution epoch contains more revisions than capabilities",
            ));
        }
        let mut revision_ids = HashSet::new();
        for revision in &wire.invocation_revisions {
            if !revision_ids.insert(revision.capability_id()) {
                return Err(serde::de::Error::custom(
                    "execution epoch contains duplicate capability revisions",
                ));
            }
            if !ids.contains(revision.capability_id()) {
                return Err(serde::de::Error::custom(
                    "execution epoch revision has no matching capability card",
                ));
            }
        }
        Ok(Self {
            id: wire.id,
            max_working_set: wire.max_working_set,
            capabilities: wire.capabilities,
            invocation_revisions: wire.invocation_revisions,
        })
    }
}

#[derive(Debug, Error)]
pub enum CapabilityError {
    #[error("capability manifest I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse capability manifest {path}: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("duplicate capability id: {0}")]
    DuplicateId(String),
    #[error("invalid capability {id}: {reason}")]
    InvalidManifest { id: String, reason: String },
    #[error("capability {id} references unknown complement {complement}")]
    UnknownComplement { id: String, complement: String },
}

/// Failures from the bounded V2 capability retrieval path.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum CapabilitySearchError {
    #[error("capability search context is invalid: {0}")]
    InvalidSearchContext(#[from] SearchContextError),
    #[error("shared retrieval query or capability document is invalid: {0}")]
    Retrieval(#[from] RetrievalError),
    #[error(
        "task query mode {mode} does not match the embedding provider presence ({provider_present})"
    )]
    ProviderModeMismatch {
        mode: RetrievalMode,
        provider_present: bool,
    },
    #[error("capability {id} references unknown complement {complement}")]
    UnknownComplement { id: String, complement: String },
}

#[derive(Debug, Clone, Default)]
pub struct CapabilityCatalog {
    manifests: Vec<CapabilityManifest>,
    positions: HashMap<String, usize>,
}

impl CapabilityCatalog {
    pub fn load(root: impl AsRef<Path>) -> Result<Self, CapabilityError> {
        let root = root.as_ref();
        if !root.exists() {
            return Ok(Self::default());
        }

        let mut paths = Vec::new();
        collect_manifests(root, &mut paths)?;
        paths.sort();

        let mut catalog = Self::default();
        for path in paths {
            let content = fs::read_to_string(&path)?;
            let manifest = toml::from_str::<CapabilityManifest>(&content).map_err(|source| {
                CapabilityError::Parse {
                    path: path.clone(),
                    source,
                }
            })?;
            catalog.insert(manifest)?;
        }
        catalog.validate()?;
        Ok(catalog)
    }

    pub fn insert(&mut self, manifest: CapabilityManifest) -> Result<(), CapabilityError> {
        validate_manifest(&manifest)?;
        if self.positions.contains_key(&manifest.id) {
            return Err(CapabilityError::DuplicateId(manifest.id));
        }
        let index = self.manifests.len();
        self.positions.insert(manifest.id.clone(), index);
        self.manifests.push(manifest);
        Ok(())
    }

    pub fn validate(&self) -> Result<(), CapabilityError> {
        for manifest in &self.manifests {
            for complement in &manifest.retrieval.complements {
                if self.get(complement).is_none() {
                    return Err(CapabilityError::UnknownComplement {
                        id: manifest.id.clone(),
                        complement: complement.clone(),
                    });
                }
            }
        }
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.manifests.len()
    }

    pub fn is_empty(&self) -> bool {
        self.manifests.is_empty()
    }

    pub fn get(&self, id: &str) -> Option<&CapabilityManifest> {
        self.positions
            .get(id)
            .and_then(|index| self.manifests.get(*index))
    }

    pub fn cards(&self) -> Vec<CapabilityCard> {
        self.manifests
            .iter()
            .filter(|manifest| manifest.lifecycle == CapabilityLifecycle::Active)
            .map(CapabilityCard::from)
            .collect()
    }

    pub fn search(&self, query: &str, limit: usize) -> Vec<CapabilityCard> {
        self.search_with_context(query, &SearchContext::catalogue(), limit)
    }

    pub fn search_with_context(
        &self,
        query: &str,
        context: &SearchContext,
        limit: usize,
    ) -> Vec<CapabilityCard> {
        if limit == 0 {
            return Vec::new();
        }
        let query = normalize(query);
        let query_tokens = tokenize(&query);
        let mut ranked = self
            .manifests
            .iter()
            .filter(|manifest| matches_context(manifest, context))
            .filter_map(|manifest| {
                let score = lexical_score(manifest, &query, &query_tokens, context);
                (query.is_empty() || score > 0.0).then_some((score, manifest))
            })
            .collect::<Vec<_>>();

        ranked.sort_by(|(left_score, left), (right_score, right)| {
            right_score
                .total_cmp(left_score)
                .then_with(|| left.id.cmp(&right.id))
        });

        let mut selected = Vec::new();
        let mut seen_ids = HashSet::new();
        for (_, manifest) in ranked {
            if selected.len() >= limit {
                break;
            }
            if seen_ids.insert(manifest.id.as_str()) {
                selected.push(manifest);
            }
            for complement_id in &manifest.retrieval.complements {
                if selected.len() >= limit {
                    break;
                }
                let Some(complement) = self.get(complement_id) else {
                    continue;
                };
                if matches_context(complement, context) && seen_ids.insert(complement.id.as_str()) {
                    selected.push(complement);
                }
            }
        }

        selected.into_iter().map(CapabilityCard::from).collect()
    }

    /// Search the installed catalogue with a validated shared V2 task query.
    ///
    /// This path deliberately does not construct or embed a query.  An
    /// embedded [`TaskQuery`] already owns its one validated query vector;
    /// this method only performs bounded document calls for roots that first
    /// pass runtime and negative-example filters plus exact/lexical
    /// eligibility.
    pub fn search_task_query(
        &self,
        query: &TaskQuery,
        context: &SearchContext,
        root_limit: CapabilityRootLimit,
        epoch_limit: ExecutionEpochLimit,
        provider: Option<&dyn EmbeddingProvider>,
    ) -> Result<Vec<CapabilityCard>, CapabilitySearchError> {
        let mut budget = RetrievalWorkBudget::new();
        self.search_task_query_with_budget(
            query,
            context,
            root_limit,
            epoch_limit,
            provider,
            &mut budget,
        )
    }

    /// Search while sharing the caller's request-local work envelope with
    /// query construction and context retrieval.
    pub fn search_task_query_with_budget(
        &self,
        query: &TaskQuery,
        context: &SearchContext,
        root_limit: CapabilityRootLimit,
        epoch_limit: ExecutionEpochLimit,
        provider: Option<&dyn EmbeddingProvider>,
        budget: &mut RetrievalWorkBudget,
    ) -> Result<Vec<CapabilityCard>, CapabilitySearchError> {
        context.validate()?;
        validate_query_provider(query, provider)?;

        let mut active_count = 0;
        for manifest in &self.manifests {
            if manifest.lifecycle == CapabilityLifecycle::Active {
                active_count += 1;
                if active_count > MAX_CANDIDATE_COUNT {
                    return Err(RetrievalError::CandidateCountExceeded {
                        actual: active_count,
                        maximum: MAX_CANDIDATE_COUNT,
                    }
                    .into());
                }
            }
        }

        // This is the sole V2 catalogue ordering pass.  Complement expansion
        // below uses the existing ID index and never scans or sorts again.
        let mut manifests = self
            .manifests
            .iter()
            .filter(|manifest| manifest.lifecycle == CapabilityLifecycle::Active)
            .collect::<Vec<_>>();
        manifests.sort_unstable_by(|left, right| left.id.cmp(&right.id));

        // Resolve references for every active root before any provider call.
        for manifest in &manifests {
            for complement in &manifest.retrieval.complements {
                if self.get(complement).is_none() {
                    return Err(CapabilitySearchError::UnknownComplement {
                        id: manifest.id.clone(),
                        complement: complement.clone(),
                    });
                }
            }
        }

        let mut roots = Vec::with_capacity(root_limit.get());
        for manifest in manifests {
            budget.charge_candidate_bytes(capability_candidate_bytes(manifest))?;
            if !matches_context(manifest, context) {
                continue;
            }
            if denied_by_negative_examples(manifest, query, budget)? {
                continue;
            }

            let document_len = capability_retrieval_document_len(manifest)?;
            budget.charge_document_bytes(document_len)?;
            let document = capability_retrieval_document(manifest)?;
            let exact = exact_capability_match(manifest, query, budget)?;
            let lexical_overlap = query.lexical_overlap_with_budget(&document, budget)?;
            if !exact && lexical_overlap == 0.0 {
                continue;
            }
            let embedding_similarity = if let Some(provider) = provider {
                let vector = query.embed_document_with_budget(provider, &document, budget)?;
                let query_vector = query
                    .query_embedding()
                    .ok_or(RetrievalError::EmbeddingNotConfigured)?;
                cosine_similarity(query_vector, &vector)?
            } else {
                0.0
            };
            roots.push(V2Root {
                manifest,
                exact,
                lexical_overlap,
                preferred: preferred_placement_match(manifest, context),
                embedding_similarity,
            });
            roots.sort_unstable_by(v2_root_order);
            if roots.len() > root_limit.get() {
                roots.pop();
            }
        }

        let mut selected = Vec::new();
        let mut seen_ids = HashSet::new();
        let epoch_capacity = epoch_limit.get();
        for root in roots.into_iter().take(root_limit.get()) {
            if selected.len() >= epoch_capacity {
                break;
            }
            if seen_ids.insert(root.manifest.id.as_str()) {
                selected.push(CapabilityCard::from(root.manifest));
            }
            if selected.len() >= epoch_capacity {
                break;
            }

            for complement_id in &root.manifest.retrieval.complements {
                if selected.len() >= epoch_capacity {
                    break;
                }
                let complement = self.get(complement_id).ok_or_else(|| {
                    CapabilitySearchError::UnknownComplement {
                        id: root.manifest.id.clone(),
                        complement: complement_id.clone(),
                    }
                })?;
                if !matches_context(complement, context)
                    || denied_by_negative_examples(complement, query, budget)?
                {
                    continue;
                }
                if seen_ids.insert(complement.id.as_str()) {
                    selected.push(CapabilityCard::from(complement));
                }
            }
        }

        Ok(selected)
    }
}

fn validate_query_provider(
    query: &TaskQuery,
    provider: Option<&dyn EmbeddingProvider>,
) -> Result<(), CapabilitySearchError> {
    let provider_present = provider.is_some();
    let pairing_valid = match query.mode() {
        RetrievalMode::LexicalOnly => !provider_present,
        RetrievalMode::Embedded => provider_present,
    };
    if pairing_valid {
        Ok(())
    } else {
        Err(CapabilitySearchError::ProviderModeMismatch {
            mode: query.mode(),
            provider_present,
        })
    }
}

fn denied_by_negative_examples(
    manifest: &CapabilityManifest,
    query: &TaskQuery,
    budget: &mut RetrievalWorkBudget,
) -> Result<bool, CapabilitySearchError> {
    let mut denied = false;
    for example in &manifest.retrieval.negative_examples {
        budget.charge_lexical_bytes(example.len())?;
        if query.contains_normalized_phrase(example)? {
            denied = true;
        }
    }
    Ok(denied)
}

fn exact_capability_match(
    manifest: &CapabilityManifest,
    query: &TaskQuery,
    budget: &mut RetrievalWorkBudget,
) -> Result<bool, RetrievalError> {
    budget.charge_lexical_bytes(manifest.id.len())?;
    if query.matches_exact_term(&manifest.id)? {
        return Ok(true);
    }
    for alias in &manifest.retrieval.aliases {
        budget.charge_lexical_bytes(alias.len())?;
        if query.matches_exact_term(alias)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn capability_candidate_bytes(manifest: &CapabilityManifest) -> usize {
    let mut bytes = manifest
        .id
        .len()
        .saturating_add(manifest.version.len())
        .saturating_add(manifest.namespace.len())
        .saturating_add(manifest.summary.len());
    for value in manifest
        .placement
        .modes
        .iter()
        .chain(&manifest.placement.requires)
        .chain(&manifest.retrieval.intents)
        .chain(&manifest.retrieval.negative_examples)
        .chain(&manifest.retrieval.complements)
        .chain(&manifest.retrieval.aliases)
        .chain(&manifest.effects.resources)
        .chain(&manifest.policy.secret_handles)
    {
        bytes = bytes.saturating_add(value.len());
    }
    if let Some(value) = manifest.runtime.command.as_deref() {
        bytes = bytes.saturating_add(value.len());
    }
    if let Some(value) = manifest.policy.approval.as_deref() {
        bytes = bytes.saturating_add(value.len());
    }
    if let Some(value) = manifest.verification.default.as_deref() {
        bytes = bytes.saturating_add(value.len());
    }
    bytes
}

fn preferred_placement_match(manifest: &CapabilityManifest, context: &SearchContext) -> bool {
    context
        .preferred_placement
        .as_ref()
        .is_some_and(|preferred| {
            context
                .available_placements
                .as_ref()
                .is_some_and(|available| available.contains(preferred))
                && manifest.placement.modes.contains(preferred)
        })
}

struct V2Root<'a> {
    manifest: &'a CapabilityManifest,
    exact: bool,
    lexical_overlap: f32,
    preferred: bool,
    embedding_similarity: f32,
}

fn v2_root_order(left: &V2Root<'_>, right: &V2Root<'_>) -> Ordering {
    right
        .exact
        .cmp(&left.exact)
        .then_with(|| {
            right
                .embedding_similarity
                .total_cmp(&left.embedding_similarity)
        })
        .then_with(|| right.lexical_overlap.total_cmp(&left.lexical_overlap))
        .then_with(|| right.preferred.cmp(&left.preferred))
        .then_with(|| left.manifest.id.cmp(&right.manifest.id))
}

fn matches_context(manifest: &CapabilityManifest, context: &SearchContext) -> bool {
    if manifest.lifecycle != CapabilityLifecycle::Active {
        return false;
    }
    if let Some(allowed) = &context.allowed_capability_ids
        && !allowed.iter().any(|id| id == &manifest.id || id == "*")
    {
        return false;
    }

    if context.mode == SearchMode::Catalogue {
        return true;
    }

    let Some(placements) = &context.available_placements else {
        return false;
    };
    if manifest.placement.modes.is_empty()
        || !manifest
            .placement
            .modes
            .iter()
            .any(|mode| placements.contains(mode))
    {
        return false;
    }

    match &context.available_requirements {
        Some(available) => {
            if !manifest
                .placement
                .requires
                .iter()
                .all(|requirement| available.contains(requirement))
            {
                return false;
            }
        }
        None if !manifest.placement.requires.is_empty() => return false,
        None => {}
    }

    let Some(effect_ceiling) = context.effect_ceiling else {
        return false;
    };
    effect_ceiling.permits(manifest.effects.minimum)
}

fn lexical_score(
    manifest: &CapabilityManifest,
    normalized_query: &str,
    query_tokens: &HashSet<String>,
    context: &SearchContext,
) -> f32 {
    if manifest.retrieval.negative_examples.iter().any(|example| {
        let normalized = normalize(example);
        !normalized.is_empty() && normalized_query.contains(&normalized)
    }) {
        return 0.0;
    }

    let id = normalize(&manifest.id);
    let aliases = normalize(&manifest.retrieval.aliases.join(" "));
    let positive = normalize(&format!(
        "{} {} {} {} {}",
        manifest.id,
        manifest.namespace,
        manifest.summary,
        aliases,
        manifest.retrieval.intents.join(" ")
    ));
    let positive_tokens = tokenize(&positive);

    let mut score = 0.0;
    if id == normalized_query {
        score += 10.0;
    } else if id.contains(normalized_query) || aliases.contains(normalized_query) {
        score += 5.0;
    } else if positive.contains(normalized_query) {
        score += 2.5;
    }

    if !query_tokens.is_empty() {
        let overlap =
            query_tokens.intersection(&positive_tokens).count() as f32 / query_tokens.len() as f32;
        score += overlap * 4.0;
    }

    let negative_penalty = manifest
        .retrieval
        .negative_examples
        .iter()
        .map(|example| {
            let tokens = tokenize(&normalize(example));
            if query_tokens.is_empty() {
                0.0
            } else {
                query_tokens.intersection(&tokens).count() as f32 / query_tokens.len() as f32
            }
        })
        .fold(0.0_f32, f32::max);
    score -= negative_penalty * 3.0;

    if preferred_placement_match(manifest, context) {
        score += 0.25;
    }

    score
}

fn validate_manifest(manifest: &CapabilityManifest) -> Result<(), CapabilityError> {
    macro_rules! invalid {
        ($reason:expr) => {
            CapabilityError::InvalidManifest {
                id: manifest.id.clone(),
                reason: $reason.into(),
            }
        };
    }
    if !valid_capability_id(&manifest.id) {
        return Err(invalid!("id must contain lowercase dotted segments"));
    }
    if manifest.namespace != manifest.id.split('.').next().unwrap_or_default() {
        return Err(invalid!("namespace must match the first id segment"));
    }
    if !valid_semver(&manifest.version) {
        return Err(invalid!("version must be a semantic x.y.z version"));
    }
    if manifest.summary.trim().is_empty() {
        return Err(invalid!("summary is empty"));
    }
    if manifest.placement.modes.is_empty() {
        return Err(invalid!("at least one placement mode is required"));
    }
    if manifest.runtime.runtime_type == RuntimeType::Process
        && manifest
            .runtime
            .command
            .as_ref()
            .is_none_or(|command| command.trim().is_empty())
    {
        return Err(invalid!("process runtime requires a command"));
    }
    if !manifest.effects.maximum.permits(manifest.effects.minimum) {
        return Err(invalid!("minimum effect exceeds maximum effect"));
    }
    for (label, values) in [
        ("alias", &manifest.retrieval.aliases),
        ("intent", &manifest.retrieval.intents),
        ("complement", &manifest.retrieval.complements),
    ] {
        let mut seen = HashSet::new();
        if values.iter().any(|value| !seen.insert(value)) {
            return Err(invalid!(format!("duplicate {label}")));
        }
    }
    if manifest
        .retrieval
        .complements
        .iter()
        .any(|complement| complement == &manifest.id)
    {
        return Err(invalid!("capability cannot complement itself"));
    }
    Ok(())
}

fn collect_manifests(directory: &Path, output: &mut Vec<PathBuf>) -> Result<(), std::io::Error> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_manifests(&path, output)?;
        } else if path
            .file_name()
            .is_some_and(|name| name == "capability.toml")
        {
            output.push(path);
        }
    }
    Ok(())
}

fn valid_capability_id(id: &str) -> bool {
    let mut segments = id.split('.');
    let mut count = 0;
    for segment in &mut segments {
        count += 1;
        let mut chars = segment.chars();
        let Some(first) = chars.next() else {
            return false;
        };
        if !first.is_ascii_lowercase() {
            return false;
        }
        if !chars.all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
        }) {
            return false;
        }
    }
    count >= 2
}

fn valid_semver(version: &str) -> bool {
    let core = version.split_once('-').map_or(version, |(core, _)| core);
    let parts = core.split('.').collect::<Vec<_>>();
    parts.len() == 3
        && parts.iter().all(|part| {
            !part.is_empty() && part.chars().all(|character| character.is_ascii_digit())
        })
}

fn normalize(input: &str) -> String {
    input
        .chars()
        .map(|character| {
            if character.is_alphanumeric() || character == '.' {
                character.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn tokenize(input: &str) -> HashSet<String> {
    input
        .split_whitespace()
        .filter(|token| token.chars().count() > 1)
        .map(str::to_owned)
        .collect()
}

const fn default_true() -> bool {
    true
}

const fn default_idle_ttl_ms() -> u64 {
    30_000
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        sync::{Arc, Mutex},
    };

    use tempfile::tempdir;

    use ditto_retrieval::{
        Embedding, EmbeddingProviderError, EmbeddingPurpose, MAX_PROVIDER_CALLS, MAX_REQUEST_BYTES,
        MAX_RETRIEVAL_DOCUMENT_BYTES, RetrievalError, RetrievalWorkBudget, RetrievalWorkKind,
        TaskSignatureV2,
    };

    use super::{
        CapabilityCard, CapabilityCatalog, CapabilityError, CapabilityKind, CapabilityLifecycle,
        CapabilityManifest, CapabilityRootLimit, CapabilitySchema, CapabilitySchemaError,
        DataAccess, EffectProfile, EffectSpec, EmbeddingProvider, ExecutionEpoch,
        ExecutionEpochLimit, Externality, JSON_SCHEMA_DRAFT_2020_12_URI, JsonSchemaValidationError,
        Mutation, PlacementSpec, PolicySpec, Privilege, RetrievalSpec, RuntimeSpec, RuntimeType,
        SearchContext, SearchContextError, SearchMode, TaskQuery, VerificationSpec,
        capability_retrieval_document, validate_json_schema,
    };

    const MANIFEST: &str = r#"
id = "device.process.run"
version = "0.1.0"
namespace = "device"
kind = "tool"
summary = "Run a structured process on a registered device."

[runtime]
type = "process"
command = "ditto-device-runner"

[placement]
modes = ["local", "ssh"]
requires = ["process"]

[retrieval]
intents = ["run a command on another computer", "restart a service"]
negative_examples = ["send a message to another person"]
aliases = ["remote command"]

[effects.minimum]
access = "metadata"
mutation = "none"
externality = "local"
privilege = "user"

[effects.maximum]
access = "credentials"
mutation = "irreversible"
externality = "network"
privilege = "elevated"

[policy]
approval = "risk-based"

    [verification]
default = "exit-code-and-expected-output"
"#;

    #[derive(Clone, Default)]
    struct RecordingProvider {
        calls: Arc<Mutex<Vec<(EmbeddingPurpose, String)>>>,
        descriptor: String,
        query_vector: Vec<f32>,
        document_vector: Vec<f32>,
        failing_documents: bool,
        mismatched_documents: bool,
        mismatched_dimensions: bool,
    }

    impl RecordingProvider {
        fn new() -> Self {
            Self {
                descriptor: "fixture-v1".into(),
                query_vector: vec![1.0, 0.0],
                document_vector: vec![0.0, 1.0],
                ..Self::default()
            }
        }

        fn calls(&self) -> Vec<(EmbeddingPurpose, String)> {
            self.calls.lock().expect("provider calls lock").clone()
        }

        fn document_call_count(&self) -> usize {
            self.calls()
                .iter()
                .filter(|(purpose, _)| *purpose == EmbeddingPurpose::Document)
                .count()
        }
    }

    impl EmbeddingProvider for RecordingProvider {
        fn embed(
            &self,
            purpose: EmbeddingPurpose,
            text: &str,
        ) -> Result<Embedding, EmbeddingProviderError> {
            self.calls
                .lock()
                .expect("provider calls lock")
                .push((purpose, text.to_owned()));
            if purpose == EmbeddingPurpose::Document && self.failing_documents {
                return Err(EmbeddingProviderError::failure("document unavailable"));
            }
            let descriptor = if purpose == EmbeddingPurpose::Document && self.mismatched_documents {
                "other-descriptor"
            } else {
                &self.descriptor
            };
            let vector = if purpose == EmbeddingPurpose::Document && self.mismatched_dimensions {
                vec![1.0, 0.0, 0.0]
            } else {
                match purpose {
                    EmbeddingPurpose::Query => self.query_vector.clone(),
                    EmbeddingPurpose::Document
                        if text.contains("gamma.lexical") || text.contains("device.high") =>
                    {
                        vec![1.0, 0.0]
                    }
                    EmbeddingPurpose::Document
                        if text.contains("beta.lexical") || text.contains("device.low") =>
                    {
                        vec![0.0, 1.0]
                    }
                    EmbeddingPurpose::Document => self.document_vector.clone(),
                }
            };
            Ok(Embedding::new(descriptor, vector))
        }
    }

    #[test]
    fn loads_and_searches_manifests() {
        let directory = tempdir().expect("temporary directory");
        let capability_dir = directory.path().join("device-process-run");
        fs::create_dir_all(&capability_dir).expect("create capability directory");
        fs::write(capability_dir.join("capability.toml"), MANIFEST).expect("write manifest");

        let catalog = CapabilityCatalog::load(directory.path()).expect("load catalog");
        assert_eq!(catalog.len(), 1);

        let result = catalog.search("remote command", 3);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "device.process.run");
    }

    #[test]
    fn runtime_search_is_fail_closed_and_uses_minimum_effect() {
        let mut catalog = CapabilityCatalog::default();
        catalog
            .insert(manifest(
                "device.process.run",
                "restart service",
                &["local", "ssh"],
                &["process"],
                &[],
                EffectProfile {
                    access: DataAccess::Metadata,
                    ..EffectProfile::default()
                },
                EffectProfile {
                    access: DataAccess::Credentials,
                    mutation: Mutation::Irreversible,
                    externality: Externality::Network,
                    privilege: Privilege::Elevated,
                },
            ))
            .expect("insert capability");

        let missing_runtime_state = SearchContext {
            mode: SearchMode::Runtime,
            ..SearchContext::default()
        };
        assert!(
            catalog
                .search_with_context("restart service", &missing_runtime_state, 3)
                .is_empty()
        );

        let context = SearchContext::runtime(
            vec!["local".into()],
            vec!["process".into()],
            EffectProfile {
                access: DataAccess::Metadata,
                ..EffectProfile::default()
            },
        );
        let result = catalog.search_with_context("restart service", &context, 3);
        assert_eq!(result[0].id, "device.process.run");
    }

    #[test]
    fn independently_places_and_deduplicates_complements() {
        let mut catalog = CapabilityCatalog::default();
        catalog
            .insert(manifest(
                "device.process.run",
                "inspect remote logs",
                &["ssh"],
                &["process"],
                &["artifact.read"],
                EffectProfile {
                    access: DataAccess::Metadata,
                    externality: Externality::Network,
                    ..EffectProfile::default()
                },
                EffectProfile {
                    access: DataAccess::Content,
                    externality: Externality::Network,
                    ..EffectProfile::default()
                },
            ))
            .expect("insert process capability");
        catalog
            .insert(manifest(
                "artifact.read",
                "read remote process output",
                &["local"],
                &[],
                &[],
                EffectProfile::read_content(),
                EffectProfile::read_content(),
            ))
            .expect("insert artifact capability");
        catalog.validate().expect("validate references");

        let mut context = SearchContext::runtime(
            vec!["ssh".into(), "local".into()],
            vec!["process".into()],
            EffectProfile {
                access: DataAccess::Content,
                externality: Externality::Network,
                ..EffectProfile::default()
            },
        );
        context.preferred_placement = Some("ssh".into());
        let results = catalog.search_with_context("inspect remote logs", &context, 3);
        let ids = results
            .iter()
            .map(|card| card.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, ["device.process.run", "artifact.read"]);
    }

    #[test]
    fn rejects_unknown_complements() {
        let mut catalog = CapabilityCatalog::default();
        catalog
            .insert(manifest(
                "device.process.run",
                "run process",
                &["local"],
                &["process"],
                &["device.process.wait"],
                EffectProfile::default(),
                EffectProfile::default(),
            ))
            .expect("insert capability");
        assert!(matches!(
            catalog.validate(),
            Err(CapabilityError::UnknownComplement { .. })
        ));
    }

    #[test]
    fn bounds_and_stabilizes_a_large_capability_working_set() {
        let mut catalog = CapabilityCatalog::default();
        for index in 0..1_000 {
            catalog
                .insert(manifest(
                    &format!("synthetic.noise{index:04}.run"),
                    "unrelated synthetic operation",
                    &["local"],
                    &[],
                    &[],
                    EffectProfile::default(),
                    EffectProfile::default(),
                ))
                .expect("insert synthetic capability");
        }
        catalog
            .insert(manifest(
                "database.backup",
                "create database backup snapshot",
                &["local"],
                &[],
                &[],
                EffectProfile::default(),
                EffectProfile {
                    mutation: Mutation::Reversible,
                    ..EffectProfile::default()
                },
            ))
            .expect("insert relevant capability");

        let mut epoch = ExecutionEpoch::new(6);
        let first_page = catalog.search("database backup", 6);
        assert_eq!(first_page.len(), 1);
        assert_eq!(epoch.page_in(first_page), 1);
        let stable_id = epoch.capabilities()[0].id.clone();

        let noise = catalog.search("synthetic operation", 100);
        assert_eq!(epoch.page_in(noise), 5);
        assert_eq!(epoch.capabilities().len(), 6);
        assert_eq!(epoch.capabilities()[0].id, stable_id);
        assert_eq!(epoch.page_in(catalog.search("database backup", 6)), 0);
    }

    #[test]
    fn orthogonal_effects_do_not_inherit_from_privilege() {
        let elevated_read = EffectProfile {
            access: DataAccess::Content,
            privilege: Privilege::Elevated,
            ..EffectProfile::default()
        };
        let irreversible_write = EffectProfile {
            mutation: Mutation::Irreversible,
            ..EffectProfile::default()
        };
        assert!(!elevated_read.permits(irreversible_write));
    }

    #[test]
    fn capability_schema_round_trips_full_json_schemas() {
        let schema = CapabilitySchema {
            id: "artifact.read".into(),
            version: "1.2.3-beta".into(),
            summary: "Read an artifact range".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "artifact_id": { "type": "string" }
                },
                "required": ["artifact_id"]
            }),
            output_schema: serde_json::json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "type": "object",
                "additionalProperties": false
            }),
        };

        schema.validate().expect("valid capability schema");
        let encoded = serde_json::to_string(&schema).expect("serialize capability schema");
        let decoded: CapabilitySchema =
            serde_json::from_str(&encoded).expect("deserialize capability schema");
        assert_eq!(decoded.id, schema.id);
        assert_eq!(decoded.version, schema.version);
        assert_eq!(decoded.summary, schema.summary);
        assert_eq!(decoded.input_schema, schema.input_schema);
        assert_eq!(decoded.output_schema, schema.output_schema);
    }

    #[test]
    fn capability_schema_rejects_invalid_id_and_schema_root() {
        let mut schema = CapabilitySchema {
            id: "Artifact.Read".into(),
            version: "1.0.0".into(),
            summary: "Read an artifact".into(),
            input_schema: serde_json::json!({"type": "object"}),
            output_schema: serde_json::json!(true),
        };
        assert!(matches!(
            schema.validate(),
            Err(CapabilitySchemaError::InvalidId { .. })
        ));

        schema.id = "artifact.read".into();
        schema.input_schema = serde_json::json!(["an", "array"]);
        assert!(matches!(
            schema.validate(),
            Err(CapabilitySchemaError::InvalidSchema {
                field: "input_schema"
            })
        ));
    }

    #[test]
    fn capability_schema_rejects_malformed_known_keywords_recursively() {
        let invalid_input_schemas = [
            serde_json::json!({"type": 42}),
            serde_json::json!({"type": ["object", 42]}),
            serde_json::json!({"type": []}),
            serde_json::json!({"properties": {"artifact_id": 42}}),
            serde_json::json!({"items": 42}),
            serde_json::json!({"items": []}),
            serde_json::json!({"additionalProperties": 42}),
            serde_json::json!({"anyOf": [{"type": "string"}, 42]}),
            serde_json::json!({"prefixItems": []}),
            serde_json::json!({"required": ["artifact_id", 42]}),
            serde_json::json!({
                "properties": {
                    "nested": {
                        "items": {
                            "oneOf": [
                                {"type": "string"},
                                {"properties": {"child": {"type": 42}}}
                            ]
                        }
                    }
                }
            }),
        ];

        for input_schema in invalid_input_schemas {
            let schema = CapabilitySchema {
                id: "artifact.read".into(),
                version: "1.0.0".into(),
                summary: "Read an artifact".into(),
                input_schema,
                output_schema: serde_json::json!(true),
            };
            assert!(matches!(
                schema.validate(),
                Err(CapabilitySchemaError::InvalidSchema {
                    field: "input_schema"
                })
            ));
        }
    }

    #[test]
    fn capability_schema_allows_boolean_schemas_and_opaque_extensions() {
        let mut schema = CapabilitySchema {
            id: "artifact.read".into(),
            version: "1.0.0".into(),
            summary: "Read an artifact".into(),
            input_schema: serde_json::json!(false),
            output_schema: serde_json::json!(true),
        };
        schema
            .validate()
            .expect("boolean schemas are provider-neutral schemas");

        schema.input_schema = serde_json::json!({
            "type": "object",
            "enum": [],
            "dependentRequired": {"artifact_id": []}
        });
        schema
            .validate()
            .expect("empty enum and dependentRequired arrays are valid");

        schema.input_schema = serde_json::json!({
            "type": "array",
            "prefixItems": [{"type": "string"}],
            "required": []
        });
        schema
            .validate()
            .expect("empty required arrays are structurally valid");

        schema.input_schema = serde_json::json!({
            "type": "object",
            "properties": {
                "artifact_id": {
                    "type": "string",
                    "x-extension": {"items": 42}
                }
            },
            "x-provider-metadata": {"required": [42]}
        });
        schema
            .validate()
            .expect("unknown extension keywords remain opaque");
    }

    #[test]
    fn capability_schema_enforces_canonical_draft_2020_12_dialect() {
        validate_json_schema(&serde_json::json!({"type": "object"}))
            .expect("omitted $schema uses Draft 2020-12");
        validate_json_schema(&serde_json::json!({
            "$schema": JSON_SCHEMA_DRAFT_2020_12_URI,
            "type": "object"
        }))
        .expect("canonical Draft 2020-12 declaration is accepted");

        let mut schema = CapabilitySchema {
            id: "artifact.read".into(),
            version: "1.0.0".into(),
            summary: "Read an artifact".into(),
            input_schema: serde_json::json!({
                "$schema": "http://json-schema.org/draft-07/schema",
                "type": "object"
            }),
            output_schema: serde_json::json!(true),
        };
        assert!(matches!(
            schema.validate(),
            Err(CapabilitySchemaError::UnsupportedDialect {
                field: "input_schema",
                dialect
            }) if dialect == "http://json-schema.org/draft-07/schema"
        ));

        schema.input_schema = serde_json::json!({
            "properties": {
                "nested": {
                    "$schema": "http://json-schema.org/draft-07/schema"
                }
            }
        });
        assert!(matches!(
            validate_json_schema(&schema.input_schema),
            Err(JsonSchemaValidationError::UnsupportedDialect { dialect })
                if dialect == "http://json-schema.org/draft-07/schema"
        ));
    }

    #[test]
    fn capability_schema_keeps_obsolete_keywords_opaque() {
        let schema = serde_json::json!({
            "type": "object",
            "id": 42,
            "$recursiveRef": 42,
            "$recursiveAnchor": "not a boolean",
            "additionalItems": 42,
            "dependencies": {"artifact_id": 42},
            "definitions": {"nested": 42}
        });
        validate_json_schema(&schema)
            .expect("obsolete keyword values are opaque extensions in Draft 2020-12");
    }

    #[test]
    fn shared_task_query_preserves_in_contract_lexical_order_and_embedding_only_reranks_eligible_roots()
     {
        let mut catalog = CapabilityCatalog::default();
        let mut exact = manifest(
            "alpha.exact",
            "operate an exact task",
            &["local"],
            &[],
            &[],
            EffectProfile::default(),
            EffectProfile::default(),
        );
        exact.retrieval.aliases = vec!["Special Alias".into()];
        catalog.insert(exact).expect("exact capability");
        catalog
            .insert(manifest(
                "beta.lexical",
                "operate beta task",
                &["local"],
                &[],
                &[],
                EffectProfile::default(),
                EffectProfile::default(),
            ))
            .expect("lexical capability");
        catalog
            .insert(manifest(
                "gamma.lexical",
                "operate gamma task",
                &["local"],
                &[],
                &[],
                EffectProfile::default(),
                EffectProfile::default(),
            ))
            .expect("second lexical capability");
        catalog
            .insert(manifest(
                "zeta.noise",
                "unrelated operation",
                &["local"],
                &[],
                &[],
                EffectProfile::default(),
                EffectProfile::default(),
            ))
            .expect("noise capability");

        let signature = TaskSignatureV2 {
            request: "operate".into(),
            entities: vec!["special alias".into()],
            ..TaskSignatureV2::default()
        };
        let lexical_query = TaskQuery::new(signature.clone()).expect("lexical query");
        let lexical = catalog
            .search_task_query(
                &lexical_query,
                &SearchContext::catalogue(),
                CapabilityRootLimit::new(3).expect("root limit"),
                ExecutionEpochLimit::new(8).expect("epoch limit"),
                None,
            )
            .expect("lexical search");
        assert_eq!(
            lexical
                .iter()
                .map(|card| card.id.as_str())
                .collect::<Vec<_>>(),
            ["alpha.exact", "beta.lexical", "gamma.lexical"]
        );

        let provider = RecordingProvider {
            document_vector: vec![0.0, 1.0],
            ..RecordingProvider::new()
        };
        let embedded_query =
            TaskQuery::with_provider(signature, Some(&provider)).expect("embedded query");
        let embedded = catalog
            .search_task_query(
                &embedded_query,
                &SearchContext::catalogue(),
                CapabilityRootLimit::new(3).expect("root limit"),
                ExecutionEpochLimit::new(8).expect("epoch limit"),
                Some(&provider),
            )
            .expect("embedded search");
        assert_eq!(
            embedded
                .iter()
                .map(|card| card.id.as_str())
                .collect::<Vec<_>>(),
            ["alpha.exact", "gamma.lexical", "beta.lexical"]
        );
        let calls = provider.calls();
        assert_eq!(
            calls
                .iter()
                .filter(|(purpose, _)| *purpose == EmbeddingPurpose::Query)
                .count(),
            1
        );
        assert_eq!(provider.document_call_count(), 3);
        let document_calls = calls
            .iter()
            .filter(|(purpose, _)| *purpose == EmbeddingPurpose::Document)
            .map(|(_, document)| document)
            .collect::<Vec<_>>();
        assert_eq!(document_calls.len(), 3);
        assert!(document_calls[0].contains("alpha.exact"));
        assert!(document_calls[1].contains("beta.lexical"));
        assert!(document_calls[2].contains("gamma.lexical"));
        assert!(
            calls
                .iter()
                .filter(|(purpose, _)| *purpose == EmbeddingPurpose::Document)
                .all(|(_, document)| !document.contains("zeta.noise"))
        );
    }

    #[test]
    fn v2_ranking_tuple_is_exact_cosine_overlap_preferred_then_id() {
        let mut exact_catalog = CapabilityCatalog::default();
        let mut exact = manifest(
            "device.exact",
            "unrelated",
            &["local"],
            &[],
            &[],
            EffectProfile::default(),
            EffectProfile::default(),
        );
        exact.retrieval.aliases = vec!["exact".into()];
        exact_catalog.insert(exact).expect("exact capability");
        for id in ["device.high", "device.low"] {
            exact_catalog
                .insert(manifest(
                    id,
                    "common unique",
                    &["local"],
                    &[],
                    &[],
                    EffectProfile::default(),
                    EffectProfile::default(),
                ))
                .expect("cosine capability");
        }
        let query = TaskQuery::new(TaskSignatureV2 {
            request: "common unique".into(),
            entities: vec!["exact".into()],
            ..TaskSignatureV2::default()
        })
        .expect("query");
        let provider = RecordingProvider::new();
        let embedded_query = TaskQuery::with_provider(query.signature().clone(), Some(&provider))
            .expect("embedded query");
        let exact_first = exact_catalog
            .search_task_query(
                &embedded_query,
                &SearchContext::catalogue(),
                CapabilityRootLimit::new(3).expect("root limit"),
                ExecutionEpochLimit::new(3).expect("epoch limit"),
                Some(&provider),
            )
            .expect("exact and cosine ranking");
        assert_eq!(
            exact_first
                .iter()
                .map(|card| card.id.as_str())
                .collect::<Vec<_>>(),
            ["device.exact", "device.high", "device.low"]
        );

        let mut overlap_catalog = CapabilityCatalog::default();
        overlap_catalog
            .insert(manifest(
                "device.high-overlap",
                "common unique",
                &["local"],
                &[],
                &[],
                EffectProfile::default(),
                EffectProfile::default(),
            ))
            .expect("high-overlap capability");
        overlap_catalog
            .insert(manifest(
                "device.low-overlap",
                "common",
                &["local"],
                &[],
                &[],
                EffectProfile::default(),
                EffectProfile::default(),
            ))
            .expect("low-overlap capability");
        let overlap_query =
            TaskQuery::new(TaskSignatureV2::new("common unique")).expect("overlap query");
        let overlap = overlap_catalog
            .search_task_query(
                &overlap_query,
                &SearchContext::catalogue(),
                CapabilityRootLimit::new(2).expect("root limit"),
                ExecutionEpochLimit::new(2).expect("epoch limit"),
                None,
            )
            .expect("lexical overlap ranking");
        assert_eq!(
            overlap
                .iter()
                .map(|card| card.id.as_str())
                .collect::<Vec<_>>(),
            ["device.high-overlap", "device.low-overlap"]
        );

        let mut preferred_catalog = CapabilityCatalog::default();
        for (id, placement) in [("device.other", "ssh"), ("device.preferred", "local")] {
            preferred_catalog
                .insert(manifest(
                    id,
                    "common",
                    &[placement],
                    &[],
                    &[],
                    EffectProfile::default(),
                    EffectProfile::default(),
                ))
                .expect("preferred capability");
        }
        let preferred_query =
            TaskQuery::new(TaskSignatureV2::new("common")).expect("preferred query");
        let preferred = preferred_catalog
            .search_task_query(
                &preferred_query,
                &SearchContext {
                    available_placements: Some(vec!["local".into(), "ssh".into()]),
                    preferred_placement: Some("local".into()),
                    ..SearchContext::catalogue()
                },
                CapabilityRootLimit::new(2).expect("root limit"),
                ExecutionEpochLimit::new(2).expect("epoch limit"),
                None,
            )
            .expect("preferred placement ranking");
        assert_eq!(
            preferred
                .iter()
                .map(|card| card.id.as_str())
                .collect::<Vec<_>>(),
            ["device.preferred", "device.other"]
        );

        let mut id_catalog = CapabilityCatalog::default();
        for id in ["device.beta", "device.alpha"] {
            id_catalog
                .insert(manifest(
                    id,
                    "common",
                    &["local"],
                    &[],
                    &[],
                    EffectProfile::default(),
                    EffectProfile::default(),
                ))
                .expect("ID tie capability");
        }
        let id_tie = id_catalog
            .search_task_query(
                &preferred_query,
                &SearchContext::catalogue(),
                CapabilityRootLimit::new(2).expect("root limit"),
                ExecutionEpochLimit::new(2).expect("epoch limit"),
                None,
            )
            .expect("ID tie ranking");
        assert_eq!(
            id_tie
                .iter()
                .map(|card| card.id.as_str())
                .collect::<Vec<_>>(),
            ["device.alpha", "device.beta"]
        );
    }

    #[test]
    fn v2_exact_capability_matches_only_ids_and_aliases_not_namespace_or_summary() {
        let mut catalog = CapabilityCatalog::default();
        catalog
            .insert(manifest(
                "device.run",
                "unrelated summary",
                &["local"],
                &[],
                &[],
                EffectProfile::default(),
                EffectProfile::default(),
            ))
            .expect("exact ID capability");

        let mut exact_alias = manifest(
            "device.alias",
            "unrelated summary",
            &["local"],
            &[],
            &[],
            EffectProfile::default(),
            EffectProfile::default(),
        );
        exact_alias.retrieval.aliases = vec!["Named Alias".into()];
        catalog.insert(exact_alias).expect("exact alias capability");

        catalog
            .insert(manifest(
                "device.summary",
                "named alias",
                &["local"],
                &[],
                &[],
                EffectProfile::default(),
                EffectProfile::default(),
            ))
            .expect("summary-only capability");
        let query = TaskQuery::new(TaskSignatureV2 {
            request: "needle".into(),
            entities: vec!["device.run".into(), "named alias".into()],
            ..TaskSignatureV2::default()
        })
        .expect("query");
        let cards = catalog
            .search_task_query(
                &query,
                &SearchContext::catalogue(),
                CapabilityRootLimit::new(3).expect("root limit"),
                ExecutionEpochLimit::new(3).expect("epoch limit"),
                None,
            )
            .expect("exact search");
        assert_eq!(
            cards
                .iter()
                .map(|card| card.id.as_str())
                .collect::<Vec<_>>(),
            ["device.alias", "device.run", "device.summary"]
        );
    }

    #[test]
    fn invalid_v2_negative_example_fails_only_after_hard_filters() {
        let mut catalog = CapabilityCatalog::default();
        let mut hard_eligible = manifest(
            "device.invalid-negative",
            "run operation",
            &["local"],
            &[],
            &[],
            EffectProfile::default(),
            EffectProfile::default(),
        );
        hard_eligible.retrieval.negative_examples = vec!["x".repeat(4_097)];
        catalog
            .insert(hard_eligible)
            .expect("invalid negative is checked by V2, not manifest validation");
        let query = TaskQuery::new(TaskSignatureV2::new("run")).expect("query");
        let error = catalog
            .search_task_query(
                &query,
                &SearchContext::catalogue(),
                CapabilityRootLimit::new(1).expect("root limit"),
                ExecutionEpochLimit::new(1).expect("epoch limit"),
                None,
            )
            .expect_err("over-bound negative example");
        assert!(matches!(
            error,
            super::CapabilitySearchError::Retrieval(RetrievalError::ComponentTooLong {
                field: "negative_example",
                ..
            })
        ));

        let mut denied = manifest(
            "device.denied-negative",
            "run operation",
            &["ssh"],
            &[],
            &[],
            EffectProfile::default(),
            EffectProfile::default(),
        );
        denied.retrieval.negative_examples = vec!["x".repeat(4_097)];
        let mut denied_catalog = CapabilityCatalog::default();
        denied_catalog.insert(denied).expect("denied capability");
        let cards = denied_catalog
            .search_task_query(
                &query,
                &SearchContext::runtime(vec!["local".into()], vec![], EffectProfile::default()),
                CapabilityRootLimit::new(1).expect("root limit"),
                ExecutionEpochLimit::new(1).expect("epoch limit"),
                None,
            )
            .expect("hard filter runs before negative validation");
        assert!(cards.is_empty());
    }

    #[test]
    fn v2_root_and_expanded_limits_reject_zero_and_n_plus_one_without_clamping() {
        assert!(CapabilityRootLimit::new(0).is_err());
        assert!(CapabilityRootLimit::new(257).is_err());
        assert!(ExecutionEpochLimit::new(0).is_err());
        assert!(ExecutionEpochLimit::new(513).is_err());

        let mut catalog = CapabilityCatalog::default();
        for index in 0..256 {
            catalog
                .insert(manifest(
                    &format!("root.capability{index:03}"),
                    "root operation",
                    &["local"],
                    &[],
                    &[&format!("support.complement{index:03}")],
                    EffectProfile::default(),
                    EffectProfile::default(),
                ))
                .expect("root capability");
        }
        for index in 0..256 {
            catalog
                .insert(manifest(
                    &format!("support.complement{index:03}"),
                    "supplement operation",
                    &["local"],
                    &[],
                    &[],
                    EffectProfile::default(),
                    EffectProfile::default(),
                ))
                .expect("complement capability");
        }

        let query = TaskQuery::new(TaskSignatureV2::new("root")).expect("query");
        let cards = catalog
            .search_task_query(
                &query,
                &SearchContext::catalogue(),
                CapabilityRootLimit::new(256).expect("root limit"),
                ExecutionEpochLimit::new(512).expect("epoch limit"),
                None,
            )
            .expect("maximum roots and expanded epoch");
        assert_eq!(cards.len(), 512);
        assert_eq!(
            cards
                .iter()
                .filter(|card| card.id.starts_with("root."))
                .count(),
            256
        );
        assert_eq!(
            cards
                .iter()
                .filter(|card| card.id.starts_with("support."))
                .count(),
            256
        );
    }

    #[test]
    fn embedding_never_bypasses_runtime_filters_negative_examples_or_complement_checks() {
        let mut catalog = CapabilityCatalog::default();
        catalog
            .insert(manifest(
                "device.allowed",
                "run permitted operation",
                &["local"],
                &["process"],
                &["device.denied-complement", "device.negative-complement"],
                EffectProfile::default(),
                EffectProfile::default(),
            ))
            .expect("allowed capability");
        catalog
            .insert(manifest(
                "device.remote",
                "run permitted operation",
                &["ssh"],
                &["process"],
                &[],
                EffectProfile::default(),
                EffectProfile::default(),
            ))
            .expect("placement-denied capability");
        catalog
            .insert(manifest(
                "device.requirement",
                "run permitted operation",
                &["local"],
                &["missing"],
                &[],
                EffectProfile::default(),
                EffectProfile::default(),
            ))
            .expect("requirement-denied capability");
        catalog
            .insert(manifest(
                "device.expensive",
                "run permitted operation",
                &["local"],
                &["process"],
                &[],
                EffectProfile {
                    access: DataAccess::Content,
                    ..EffectProfile::default()
                },
                EffectProfile {
                    access: DataAccess::Content,
                    ..EffectProfile::default()
                },
            ))
            .expect("effect-denied capability");
        catalog
            .insert(manifest(
                "device.allowlist",
                "run permitted operation",
                &["local"],
                &["process"],
                &[],
                EffectProfile::default(),
                EffectProfile::default(),
            ))
            .expect("allowlist-denied capability");
        let mut negative = manifest(
            "device.negative",
            "run permitted operation",
            &["local"],
            &["process"],
            &[],
            EffectProfile::default(),
            EffectProfile::default(),
        );
        negative.retrieval.negative_examples = vec!["run now".into()];
        catalog.insert(negative).expect("negative capability");
        catalog
            .insert(manifest(
                "device.denied-complement",
                "complement operation",
                &["ssh"],
                &["process"],
                &[],
                EffectProfile::default(),
                EffectProfile::default(),
            ))
            .expect("denied complement");
        let mut negative_complement = manifest(
            "device.negative-complement",
            "complement operation",
            &["local"],
            &["process"],
            &[],
            EffectProfile::default(),
            EffectProfile::default(),
        );
        negative_complement.retrieval.negative_examples = vec!["run now".into()];
        catalog
            .insert(negative_complement)
            .expect("negative complement");
        catalog.validate().expect("known complements");

        let mut context = SearchContext::runtime(
            vec!["local".into()],
            vec!["process".into()],
            EffectProfile::default(),
        );
        context.allowed_capability_ids = Some(vec![
            "device.allowed".into(),
            "device.remote".into(),
            "device.requirement".into(),
            "device.expensive".into(),
            "device.negative".into(),
            "device.denied-complement".into(),
            "device.negative-complement".into(),
        ]);
        let provider = RecordingProvider {
            document_vector: vec![1.0, 0.0],
            ..RecordingProvider::new()
        };
        let query = TaskQuery::with_provider(TaskSignatureV2::new("run now"), Some(&provider))
            .expect("embedded query");
        let cards = catalog
            .search_task_query(
                &query,
                &context,
                CapabilityRootLimit::new(8).expect("root limit"),
                ExecutionEpochLimit::new(8).expect("epoch limit"),
                Some(&provider),
            )
            .expect("filtered search");
        assert_eq!(
            cards
                .iter()
                .map(|card| card.id.as_str())
                .collect::<Vec<_>>(),
            ["device.allowed"]
        );
        assert_eq!(provider.document_call_count(), 1);
        let document_calls = provider
            .calls()
            .into_iter()
            .filter(|(purpose, _)| *purpose == EmbeddingPurpose::Document)
            .map(|(_, document)| document)
            .collect::<Vec<_>>();
        assert!(document_calls[0].contains("device.allowed"));
        for denied_id in [
            "device.remote",
            "device.requirement",
            "device.expensive",
            "device.allowlist",
            "device.negative",
            "device.denied-complement",
            "device.negative-complement",
        ] {
            assert!(
                document_calls
                    .iter()
                    .all(|document| !document.contains(denied_id)),
                "denied capability {denied_id} was embedded"
            );
        }

        let mut invalid_complement = manifest(
            "device.invalid",
            "run invalid operation",
            &["local"],
            &["process"],
            &["device.missing"],
            EffectProfile::default(),
            EffectProfile::default(),
        );
        invalid_complement.retrieval.negative_examples = vec!["run".into()];
        let mut invalid_catalog = CapabilityCatalog::default();
        invalid_catalog
            .insert(invalid_complement)
            .expect("invalid reference can remain unvalidated");
        let error = invalid_catalog
            .search_task_query(
                &query,
                &context,
                CapabilityRootLimit::new(1).expect("root limit"),
                ExecutionEpochLimit::new(1).expect("epoch limit"),
                Some(&provider),
            )
            .expect_err("unknown complement before provider work");
        assert!(matches!(
            error,
            super::CapabilitySearchError::UnknownComplement { .. }
        ));
        assert_eq!(provider.document_call_count(), 1);
    }

    #[test]
    fn v2_catalogue_counts_all_installed_manifests_before_filters_and_rejects_10001() {
        let mut catalog = CapabilityCatalog::default();
        for index in 0..=10_000 {
            catalog
                .insert(manifest(
                    &format!("noise.capability{index:05}"),
                    "unrelated operation",
                    &["ssh"],
                    &["unavailable"],
                    &[],
                    EffectProfile::default(),
                    EffectProfile::default(),
                ))
                .expect("noise capability");
        }

        let query = TaskQuery::new(TaskSignatureV2::new("wanted")).expect("query");
        let error = catalog
            .search_task_query(
                &query,
                &SearchContext::runtime(
                    vec!["local".into()],
                    vec!["process".into()],
                    EffectProfile::default(),
                ),
                CapabilityRootLimit::new(1).expect("root limit"),
                ExecutionEpochLimit::new(1).expect("epoch limit"),
                None,
            )
            .expect_err("candidate ceiling");
        assert!(matches!(
            error,
            super::CapabilitySearchError::Retrieval(RetrievalError::CandidateCountExceeded {
                actual: 10_001,
                maximum: 10_000,
            })
        ));
    }

    #[test]
    fn retired_and_quarantined_manifests_do_not_count_or_page_into_working_sets() {
        let mut catalog = CapabilityCatalog::default();
        for index in 0..=10_000 {
            let mut inactive = manifest(
                &format!("inactive.capability{index:05}"),
                "wanted inactive operation",
                &["local"],
                &[],
                &[],
                EffectProfile::default(),
                EffectProfile::default(),
            );
            inactive.lifecycle = if index % 2 == 0 {
                CapabilityLifecycle::Retired
            } else {
                CapabilityLifecycle::Quarantined
            };
            catalog.insert(inactive).expect("inactive capability");
        }
        catalog
            .insert(manifest(
                "wanted.active",
                "wanted active operation",
                &["local"],
                &[],
                &[],
                EffectProfile::default(),
                EffectProfile::default(),
            ))
            .expect("active capability");

        let query = TaskQuery::new(TaskSignatureV2::new("wanted")).expect("query");
        let cards = catalog
            .search_task_query(
                &query,
                &SearchContext::catalogue(),
                CapabilityRootLimit::new(1).expect("root limit"),
                ExecutionEpochLimit::new(1).expect("epoch limit"),
                None,
            )
            .expect("only active candidates count");
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].id, "wanted.active");
        assert_eq!(catalog.cards().len(), 1);
    }

    #[test]
    fn search_context_is_bounded_canonical_and_validated_before_document_calls() {
        let valid_values = (0..64)
            .map(|index| format!("placement-{index:02}"))
            .collect::<Vec<_>>();
        let valid = SearchContext {
            mode: SearchMode::Runtime,
            available_placements: Some(valid_values.clone()),
            preferred_placement: Some("placement-00".into()),
            available_requirements: Some(Vec::new()),
            effect_ceiling: Some(EffectProfile::default()),
            allowed_capability_ids: Some(vec!["*".into()]),
        };
        valid.validate().expect("exact list maximum");

        let mut too_many = valid.clone();
        too_many
            .available_placements
            .as_mut()
            .expect("placements")
            .push("placement-64".into());
        assert!(matches!(
            too_many.validate(),
            Err(SearchContextError::TooManyEntries {
                field: "available_placements",
                actual: 65,
                maximum: 64,
            })
        ));

        let mut duplicate = valid.clone();
        duplicate.available_requirements = Some(vec!["process".into(), "process".into()]);
        assert!(matches!(
            duplicate.validate(),
            Err(SearchContextError::DuplicateValue {
                field: "available_requirements",
                ..
            })
        ));
        let mut non_canonical = valid.clone();
        non_canonical.available_requirements = Some(vec!["Process".into()]);
        assert_eq!(
            non_canonical.validate(),
            Err(SearchContextError::NonCanonicalValue {
                field: "available_requirements"
            })
        );
        let mut unavailable_preferred = SearchContext::catalogue();
        unavailable_preferred.available_placements = Some(vec!["ssh".into()]);
        unavailable_preferred.preferred_placement = Some("local".into());
        assert_eq!(
            unavailable_preferred.validate(),
            Err(SearchContextError::PreferredPlacementUnavailable {
                preferred: "local".into()
            })
        );

        let mut catalog = CapabilityCatalog::default();
        catalog
            .insert(manifest(
                "device.run",
                "run operation",
                &["local"],
                &[],
                &[],
                EffectProfile::default(),
                EffectProfile::default(),
            ))
            .expect("capability");
        let provider = RecordingProvider::new();
        let query = TaskQuery::with_provider(TaskSignatureV2::new("run"), Some(&provider))
            .expect("embedded query");
        let error = catalog
            .search_task_query(
                &query,
                &unavailable_preferred,
                CapabilityRootLimit::new(1).expect("root limit"),
                ExecutionEpochLimit::new(1).expect("epoch limit"),
                Some(&provider),
            )
            .expect_err("invalid search context");
        assert!(matches!(
            error,
            super::CapabilitySearchError::InvalidSearchContext(
                SearchContextError::PreferredPlacementUnavailable { .. }
            )
        ));
        assert_eq!(provider.document_call_count(), 0);
    }

    #[test]
    fn capability_ranking_shares_provider_budget_and_keeps_only_bounded_roots() {
        let mut catalog = CapabilityCatalog::default();
        for index in 0..1_000 {
            catalog
                .insert(manifest(
                    &format!("device.operation{index:04}"),
                    "common operation",
                    &["local"],
                    &[],
                    &[],
                    EffectProfile::default(),
                    EffectProfile::default(),
                ))
                .expect("capability");
        }
        let lexical_query =
            TaskQuery::new(TaskSignatureV2::new("common operation")).expect("lexical query");
        let cards = catalog
            .search_task_query(
                &lexical_query,
                &SearchContext::catalogue(),
                CapabilityRootLimit::new(1).expect("root limit"),
                ExecutionEpochLimit::new(1).expect("epoch limit"),
                None,
            )
            .expect("streaming top-k search");
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].id, "device.operation0000");

        let provider = RecordingProvider::new();
        let mut budget = RetrievalWorkBudget::new();
        for _ in 0..(MAX_PROVIDER_CALLS - 1) {
            budget
                .charge_provider_call(0)
                .expect("preload provider budget");
        }
        let embedded_query = TaskQuery::with_provider_and_budget(
            TaskSignatureV2::new("common operation"),
            Some(&provider),
            &mut budget,
        )
        .expect("Nth provider call");
        let error = catalog
            .search_task_query_with_budget(
                &embedded_query,
                &SearchContext::catalogue(),
                CapabilityRootLimit::new(1).expect("root limit"),
                ExecutionEpochLimit::new(1).expect("epoch limit"),
                Some(&provider),
                &mut budget,
            )
            .expect_err("N+1 provider call");
        assert!(matches!(
            error,
            super::CapabilitySearchError::Retrieval(RetrievalError::WorkBudgetExceeded {
                kind: RetrievalWorkKind::ProviderCalls,
                attempted,
                maximum,
            }) if attempted == MAX_PROVIDER_CALLS + 1 && maximum == MAX_PROVIDER_CALLS
        ));
        assert_eq!(provider.document_call_count(), 0);
    }

    #[test]
    fn legacy_search_surface_and_historical_bounds_stay_source_compatible() {
        let _: fn(&CapabilityCatalog, &str, usize) -> Vec<CapabilityCard> =
            CapabilityCatalog::search;
        let _: fn(&CapabilityCatalog, &str, &SearchContext, usize) -> Vec<CapabilityCard> =
            CapabilityCatalog::search_with_context;

        let mut catalog = CapabilityCatalog::default();
        catalog
            .insert(manifest(
                "device.alpha",
                "ordinary operation",
                &["local"],
                &[],
                &["device.zeta", "device.eta"],
                EffectProfile::default(),
                EffectProfile::default(),
            ))
            .expect("alpha capability");
        catalog
            .insert(manifest(
                "device.beta",
                "ordinary operation",
                &["local"],
                &[],
                &[],
                EffectProfile::default(),
                EffectProfile::default(),
            ))
            .expect("beta capability");
        catalog
            .insert(manifest(
                "device.eta",
                "ordinary operation",
                &["local"],
                &[],
                &[],
                EffectProfile::default(),
                EffectProfile::default(),
            ))
            .expect("eta capability");
        catalog
            .insert(manifest(
                "device.zeta",
                "ordinary operation",
                &["local"],
                &[],
                &[],
                EffectProfile::default(),
                EffectProfile::default(),
            ))
            .expect("zeta capability");
        let mut one_character = manifest(
            "device.xray",
            "x operation",
            &["local"],
            &[],
            &[],
            EffectProfile::default(),
            EffectProfile::default(),
        );
        one_character.retrieval.aliases = vec!["xray".into()];
        catalog
            .insert(one_character)
            .expect("one-character capability");
        catalog.validate().expect("known complement references");

        assert!(catalog.search("x", 0).is_empty());
        assert!(
            catalog
                .search_with_context("x", &SearchContext::catalogue(), 0)
                .is_empty()
        );
        assert_eq!(
            catalog
                .search("x", 10)
                .iter()
                .map(|card| card.id.as_str())
                .collect::<Vec<_>>(),
            ["device.xray"]
        );
        assert_eq!(
            catalog
                .search("", 4)
                .iter()
                .map(|card| card.id.as_str())
                .collect::<Vec<_>>(),
            ["device.alpha", "device.zeta", "device.eta", "device.beta"]
        );

        let mut tie_catalog = CapabilityCatalog::default();
        for id in ["device.beta", "device.alpha"] {
            tie_catalog
                .insert(manifest(
                    id,
                    "common operation",
                    &["local"],
                    &[],
                    &[],
                    EffectProfile::default(),
                    EffectProfile::default(),
                ))
                .expect("tie capability");
        }
        assert_eq!(
            tie_catalog
                .search("common", 2)
                .iter()
                .map(|card| card.id.as_str())
                .collect::<Vec<_>>(),
            ["device.alpha", "device.beta"]
        );

        let oversized = "q".repeat(MAX_REQUEST_BYTES + 1);
        let historical: Vec<CapabilityCard> = catalog.search(&oversized, 1);
        assert!(historical.is_empty());
    }

    #[test]
    fn direct_complement_lookup_does_not_double_count_scan_but_consumes_epoch_capacity() {
        let mut catalog = CapabilityCatalog::default();
        for index in 0..9_998 {
            catalog
                .insert(manifest(
                    &format!("noise.capability{index:05}"),
                    "unrelated operation",
                    &["local"],
                    &[],
                    &[],
                    EffectProfile::default(),
                    EffectProfile::default(),
                ))
                .expect("noise capability");
        }
        catalog
            .insert(manifest(
                "root.main",
                "root operation",
                &["local"],
                &[],
                &["support.complement"],
                EffectProfile::default(),
                EffectProfile::default(),
            ))
            .expect("root capability");
        catalog
            .insert(manifest(
                "support.complement",
                "supplement operation",
                &["local"],
                &[],
                &[],
                EffectProfile::default(),
                EffectProfile::default(),
            ))
            .expect("complement capability");
        assert_eq!(catalog.len(), 10_000);

        let provider = RecordingProvider::new();
        let query = TaskQuery::with_provider(TaskSignatureV2::new("root"), Some(&provider))
            .expect("embedded query");
        let cards = catalog
            .search_task_query(
                &query,
                &SearchContext::runtime(vec!["local".into()], vec![], EffectProfile::default()),
                CapabilityRootLimit::new(1).expect("root limit"),
                ExecutionEpochLimit::new(2).expect("epoch limit"),
                Some(&provider),
            )
            .expect("one catalogue scan with direct complement lookup");
        assert_eq!(
            cards
                .iter()
                .map(|card| card.id.as_str())
                .collect::<Vec<_>>(),
            ["root.main", "support.complement"]
        );
        assert_eq!(provider.document_call_count(), 1);
        let root_only = catalog
            .search_task_query(
                &query,
                &SearchContext::runtime(vec!["local".into()], vec![], EffectProfile::default()),
                CapabilityRootLimit::new(1).expect("root limit"),
                ExecutionEpochLimit::new(1).expect("epoch limit"),
                Some(&provider),
            )
            .expect("epoch truncation");
        assert_eq!(root_only.len(), 1);
    }

    #[test]
    fn embedded_query_rejects_missing_mismatched_or_failed_provider() {
        let mut catalog = CapabilityCatalog::default();
        catalog
            .insert(manifest(
                "device.run",
                "run operation",
                &["local"],
                &[],
                &[],
                EffectProfile::default(),
                EffectProfile::default(),
            ))
            .expect("capability");
        let context = SearchContext::catalogue();
        let query_provider = RecordingProvider::new();
        let query = TaskQuery::with_provider(TaskSignatureV2::new("run"), Some(&query_provider))
            .expect("embedded query");
        let missing = catalog
            .search_task_query(
                &query,
                &context,
                CapabilityRootLimit::new(1).expect("root limit"),
                ExecutionEpochLimit::new(1).expect("epoch limit"),
                None,
            )
            .expect_err("missing provider");
        assert!(matches!(
            missing,
            super::CapabilitySearchError::ProviderModeMismatch {
                provider_present: false,
                ..
            }
        ));

        let mismatched_provider = RecordingProvider {
            mismatched_documents: true,
            ..RecordingProvider::new()
        };
        let mismatched = catalog
            .search_task_query(
                &query,
                &context,
                CapabilityRootLimit::new(1).expect("root limit"),
                ExecutionEpochLimit::new(1).expect("epoch limit"),
                Some(&mismatched_provider),
            )
            .expect_err("descriptor mismatch");
        assert!(matches!(
            mismatched,
            super::CapabilitySearchError::Retrieval(
                RetrievalError::EmbeddingDescriptorMismatch { .. }
            )
        ));
        assert_eq!(mismatched_provider.document_call_count(), 1);

        let dimension_provider = RecordingProvider {
            mismatched_dimensions: true,
            ..RecordingProvider::new()
        };
        let dimension_mismatch = catalog
            .search_task_query(
                &query,
                &context,
                CapabilityRootLimit::new(1).expect("root limit"),
                ExecutionEpochLimit::new(1).expect("epoch limit"),
                Some(&dimension_provider),
            )
            .expect_err("dimension mismatch");
        assert!(matches!(
            dimension_mismatch,
            super::CapabilitySearchError::Retrieval(
                RetrievalError::EmbeddingDimensionMismatch { .. }
            )
        ));
        assert_eq!(dimension_provider.document_call_count(), 1);

        let failed_provider = RecordingProvider {
            failing_documents: true,
            ..RecordingProvider::new()
        };
        let failed = catalog
            .search_task_query(
                &query,
                &context,
                CapabilityRootLimit::new(1).expect("root limit"),
                ExecutionEpochLimit::new(1).expect("epoch limit"),
                Some(&failed_provider),
            )
            .expect_err("provider failure");
        assert!(matches!(
            failed,
            super::CapabilitySearchError::Retrieval(RetrievalError::ProviderFailure { .. })
        ));

        let lexical = TaskQuery::new(TaskSignatureV2::new("run")).expect("lexical query");
        let unexpected = catalog
            .search_task_query(
                &lexical,
                &context,
                CapabilityRootLimit::new(1).expect("root limit"),
                ExecutionEpochLimit::new(1).expect("epoch limit"),
                Some(&query_provider),
            )
            .expect_err("provider is not accepted for lexical mode");
        assert!(matches!(
            unexpected,
            super::CapabilitySearchError::ProviderModeMismatch {
                provider_present: true,
                ..
            }
        ));
    }

    #[test]
    fn capability_v2_document_has_exact_raw_grammar_and_bounds() {
        let mut capability = manifest(
            "demo.read",
            "raw=summary",
            &["local"],
            &[],
            &[],
            EffectProfile::default(),
            EffectProfile::default(),
        );
        capability.retrieval.aliases = vec!["zeta".into(), "alpha".into(), "é".into()];
        capability.retrieval.intents = vec!["βeta".into(), "alpha intent".into()];
        let document = capability_retrieval_document(&capability).expect("document");
        assert_eq!(
            document.as_str(),
            "id=demo.read\nnamespace=demo\nsummary=raw=summary\nalias=alpha\nalias=zeta\nalias=é\nintent=alpha intent\nintent=βeta"
        );
        assert!(!document.as_str().ends_with('\n'));

        let prefix = "id=demo.read\nnamespace=demo\nsummary=";
        let mut at_limit = capability.clone();
        at_limit.retrieval.aliases.clear();
        at_limit.retrieval.intents.clear();
        at_limit.summary = "x".repeat(MAX_RETRIEVAL_DOCUMENT_BYTES - prefix.len());
        let at_limit_document = capability_retrieval_document(&at_limit).expect("N-byte document");
        assert_eq!(at_limit_document.len(), MAX_RETRIEVAL_DOCUMENT_BYTES);

        at_limit.summary.push('x');
        assert!(matches!(
            capability_retrieval_document(&at_limit),
            Err(RetrievalError::RetrievalDocumentTooLong {
                actual,
                maximum: MAX_RETRIEVAL_DOCUMENT_BYTES,
            }) if actual == MAX_RETRIEVAL_DOCUMENT_BYTES + 1
        ));
    }

    #[test]
    fn legacy_capability_search_retains_zero_limit_behavior_separate_from_v2() {
        let mut catalog = CapabilityCatalog::default();
        catalog
            .insert(manifest(
                "device.x",
                "x operation",
                &["local"],
                &[],
                &[],
                EffectProfile::default(),
                EffectProfile::default(),
            ))
            .expect("capability");
        assert!(catalog.search("x", 0).is_empty());
        let query = TaskQuery::new(TaskSignatureV2::new("x")).expect("V2 query");
        assert_eq!(
            catalog
                .search_task_query(
                    &query,
                    &SearchContext::catalogue(),
                    CapabilityRootLimit::new(1).expect("root limit"),
                    ExecutionEpochLimit::new(1).expect("epoch limit"),
                    None,
                )
                .expect("V2 search")[0]
                .id,
            "device.x"
        );
    }

    fn manifest(
        id: &str,
        summary: &str,
        placement_modes: &[&str],
        requirements: &[&str],
        complements: &[&str],
        minimum: EffectProfile,
        maximum: EffectProfile,
    ) -> CapabilityManifest {
        CapabilityManifest {
            id: id.into(),
            version: "0.1.0".into(),
            namespace: id.split('.').next().unwrap_or("test").into(),
            kind: CapabilityKind::Tool,
            lifecycle: CapabilityLifecycle::Active,
            summary: summary.into(),
            runtime: RuntimeSpec {
                runtime_type: RuntimeType::Builtin,
                command: None,
                lazy: true,
                idle_ttl_ms: 30_000,
            },
            placement: PlacementSpec {
                modes: placement_modes.iter().map(ToString::to_string).collect(),
                requires: requirements.iter().map(ToString::to_string).collect(),
            },
            retrieval: RetrievalSpec {
                intents: Vec::new(),
                negative_examples: Vec::new(),
                complements: complements.iter().map(ToString::to_string).collect(),
                aliases: Vec::new(),
            },
            effects: EffectSpec {
                minimum,
                maximum,
                resources: Vec::new(),
            },
            policy: PolicySpec::default(),
            verification: VerificationSpec::default(),
        }
    }
}

use std::{cmp::Ordering, collections::BTreeSet};

use regex::Regex;
use serde_json::{Map, Value};
use thiserror::Error;

pub const MAX_INVOCATION_SCHEMA_BYTES: usize = 1024 * 1024;
pub const MAX_INVOCATION_ARGUMENT_BYTES: usize = 1024 * 1024;
pub const MAX_INVOCATION_VALUE_DEPTH: usize = 64;
pub const MAX_INVOCATION_VALUE_WORK: usize = 100_000;
const MAX_PATTERN_BYTES: usize = 16 * 1024;
const MAX_ERROR_PATH_BYTES: usize = 1024;

/// Failure from the closed Ditto Invocation Schema Profile V1.
///
/// This profile is intentionally not a complete JSON Schema Draft 2020-12
/// evaluator. Unsupported syntax fails closed at live capability binding.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum InvocationSchemaError {
    #[error("invocation schema is {actual} bytes, exceeding {maximum}")]
    SchemaTooLarge { actual: usize, maximum: usize },
    #[error("invocation arguments are {actual} bytes, exceeding {maximum}")]
    InstanceTooLarge { actual: usize, maximum: usize },
    #[error("invocation schema exceeds JSON depth {maximum}")]
    SchemaDepthExceeded { maximum: usize },
    #[error("invocation arguments exceed JSON depth {maximum}")]
    InstanceDepthExceeded { maximum: usize },
    #[error("invocation schema exceeds {maximum} structural work units")]
    SchemaWorkExceeded { maximum: usize },
    #[error("invocation arguments exceed {maximum} structural work units")]
    InstanceWorkExceeded { maximum: usize },
    #[error("invocation schema is invalid in the Ditto profile: {reason}")]
    InvalidProfileSchema { reason: String },
    #[error("invocation schema keyword is outside the Ditto profile: {keyword}")]
    UnsupportedKeyword { keyword: String },
    #[error("invocation schema type is outside the Ditto profile: {type_name}")]
    UnsupportedType { type_name: String },
    #[error("invocation JSON instance is invalid at {path} ({keyword})")]
    InvalidInstance { path: String, keyword: String },
    #[error("invocation schema pattern is outside the Ditto regex profile")]
    UnsupportedPattern,
    #[error("invocation instance evaluation exceeded {maximum} work units")]
    EvaluationWorkExceeded { maximum: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ValueEnvelopeError {
    TooLarge { actual: usize },
    DepthExceeded,
    WorkExceeded,
}

/// Iteratively prove the complete JSON value fits the fixed byte, depth, and
/// node envelope before any recursive structural validation begins.
pub(crate) fn validate_value_envelope(
    value: &Value,
    maximum_bytes: usize,
) -> Result<usize, ValueEnvelopeError> {
    let mut stack = vec![(value, 0_usize)];
    let mut bytes = 0_usize;
    let mut work = 0_usize;

    while let Some((current, depth)) = stack.pop() {
        if depth > MAX_INVOCATION_VALUE_DEPTH {
            return Err(ValueEnvelopeError::DepthExceeded);
        }
        work = work
            .checked_add(1)
            .ok_or(ValueEnvelopeError::WorkExceeded)?;
        if work > MAX_INVOCATION_VALUE_WORK {
            return Err(ValueEnvelopeError::WorkExceeded);
        }
        match current {
            Value::Null => add_bytes(&mut bytes, 4, maximum_bytes)?,
            Value::Bool(true) => add_bytes(&mut bytes, 4, maximum_bytes)?,
            Value::Bool(false) => add_bytes(&mut bytes, 5, maximum_bytes)?,
            Value::Number(number) => {
                add_bytes(&mut bytes, number.to_string().len(), maximum_bytes)?;
            }
            Value::String(string) => add_bytes(
                &mut bytes,
                serde_json::to_string(string)
                    .expect("serializing one JSON string cannot fail")
                    .len(),
                maximum_bytes,
            )?,
            Value::Array(values) => {
                add_bytes(
                    &mut bytes,
                    2_usize.saturating_add(values.len().saturating_sub(1)),
                    maximum_bytes,
                )?;
                for child in values.iter().rev() {
                    stack.push((child, depth + 1));
                }
            }
            Value::Object(object) => {
                add_bytes(
                    &mut bytes,
                    2_usize.saturating_add(object.len().saturating_sub(1)),
                    maximum_bytes,
                )?;
                for (key, child) in object.iter().rev() {
                    let key_bytes = serde_json::to_string(key)
                        .expect("serializing one JSON object key cannot fail")
                        .len();
                    add_bytes(&mut bytes, key_bytes.saturating_add(1), maximum_bytes)?;
                    stack.push((child, depth + 1));
                }
            }
        }
    }
    Ok(bytes)
}

fn add_bytes(total: &mut usize, amount: usize, maximum: usize) -> Result<(), ValueEnvelopeError> {
    *total = total
        .checked_add(amount)
        .ok_or(ValueEnvelopeError::TooLarge { actual: usize::MAX })?;
    if *total > maximum {
        return Err(ValueEnvelopeError::TooLarge { actual: *total });
    }
    Ok(())
}

/// Validate only the closed schema subset executable by Ditto invocation V1.
pub fn validate_invocation_schema_profile(schema: &Value) -> Result<(), InvocationSchemaError> {
    map_schema_envelope(validate_value_envelope(schema, MAX_INVOCATION_SCHEMA_BYTES))?;
    ProfileValidator {
        remaining_work: MAX_INVOCATION_VALUE_WORK,
    }
    .validate(schema, 0)
}

/// Evaluate one raw or normalized argument value under the closed profile.
pub fn validate_invocation_instance(
    schema: &Value,
    instance: &Value,
) -> Result<(), InvocationSchemaError> {
    validate_invocation_schema_profile(schema)?;
    validate_invocation_argument_envelope(instance)?;
    Evaluator {
        remaining_work: MAX_INVOCATION_VALUE_WORK,
    }
    .evaluate(schema, instance, "$", 0)
}

/// Iteratively establish the complete argument envelope before any recursive
/// canonical projection or profile evaluation.
pub(crate) fn validate_invocation_argument_envelope(
    instance: &Value,
) -> Result<usize, InvocationSchemaError> {
    map_instance_envelope(validate_value_envelope(
        instance,
        MAX_INVOCATION_ARGUMENT_BYTES,
    ))
}

fn map_schema_envelope(
    result: Result<usize, ValueEnvelopeError>,
) -> Result<usize, InvocationSchemaError> {
    result.map_err(|error| match error {
        ValueEnvelopeError::TooLarge { actual } => InvocationSchemaError::SchemaTooLarge {
            actual,
            maximum: MAX_INVOCATION_SCHEMA_BYTES,
        },
        ValueEnvelopeError::DepthExceeded => InvocationSchemaError::SchemaDepthExceeded {
            maximum: MAX_INVOCATION_VALUE_DEPTH,
        },
        ValueEnvelopeError::WorkExceeded => InvocationSchemaError::SchemaWorkExceeded {
            maximum: MAX_INVOCATION_VALUE_WORK,
        },
    })
}

fn map_instance_envelope(
    result: Result<usize, ValueEnvelopeError>,
) -> Result<usize, InvocationSchemaError> {
    result.map_err(|error| match error {
        ValueEnvelopeError::TooLarge { actual } => InvocationSchemaError::InstanceTooLarge {
            actual,
            maximum: MAX_INVOCATION_ARGUMENT_BYTES,
        },
        ValueEnvelopeError::DepthExceeded => InvocationSchemaError::InstanceDepthExceeded {
            maximum: MAX_INVOCATION_VALUE_DEPTH,
        },
        ValueEnvelopeError::WorkExceeded => InvocationSchemaError::InstanceWorkExceeded {
            maximum: MAX_INVOCATION_VALUE_WORK,
        },
    })
}

struct ProfileValidator {
    remaining_work: usize,
}

impl ProfileValidator {
    fn validate(&mut self, schema: &Value, depth: usize) -> Result<(), InvocationSchemaError> {
        if depth > MAX_INVOCATION_VALUE_DEPTH {
            return Err(InvocationSchemaError::SchemaDepthExceeded {
                maximum: MAX_INVOCATION_VALUE_DEPTH,
            });
        }
        self.charge(1)?;
        match schema {
            Value::Bool(_) => Ok(()),
            Value::Object(object) => self.validate_object(object, depth),
            _ => Err(invalid_profile("schema root must be a boolean or object")),
        }
    }

    fn validate_object(
        &mut self,
        schema: &Map<String, Value>,
        depth: usize,
    ) -> Result<(), InvocationSchemaError> {
        const ALLOWED: &[&str] = &[
            "$schema",
            "$comment",
            "title",
            "description",
            "default",
            "examples",
            "deprecated",
            "readOnly",
            "writeOnly",
            "type",
            "const",
            "enum",
            "multipleOf",
            "maximum",
            "exclusiveMaximum",
            "minimum",
            "exclusiveMinimum",
            "maxLength",
            "minLength",
            "pattern",
            "maxItems",
            "minItems",
            "uniqueItems",
            "items",
            "maxProperties",
            "minProperties",
            "required",
            "properties",
            "additionalProperties",
        ];
        for keyword in schema.keys() {
            self.charge(1)?;
            if !ALLOWED.contains(&keyword.as_str()) {
                return Err(InvocationSchemaError::UnsupportedKeyword {
                    keyword: bounded(keyword),
                });
            }
        }

        for keyword in ["$schema", "$comment", "title", "description"] {
            if schema.get(keyword).is_some_and(|value| !value.is_string()) {
                return Err(invalid_profile(&format!("{keyword} must be a string")));
            }
        }
        if let Some(dialect) = schema.get("$schema").and_then(Value::as_str)
            && dialect != super::JSON_SCHEMA_DRAFT_2020_12_URI
        {
            return Err(invalid_profile("$schema dialect is unsupported"));
        }
        if schema
            .get("examples")
            .is_some_and(|value| !value.is_array())
        {
            return Err(invalid_profile("examples must be an array"));
        }
        for keyword in ["deprecated", "readOnly", "writeOnly"] {
            if schema.get(keyword).is_some_and(|value| !value.is_boolean()) {
                return Err(invalid_profile(&format!("{keyword} must be boolean")));
            }
        }
        if let Some(types) = schema.get("type") {
            validate_profile_types(types)?;
        }
        if let Some(values) = schema.get("enum")
            && values.as_array().is_none_or(Vec::is_empty)
        {
            return Err(invalid_profile("enum must be a non-empty array"));
        }
        if let Some(value) = schema.get("multipleOf")
            && exact_positive_integer(value).is_none()
        {
            return Err(invalid_profile(
                "multipleOf must be a positive JSON integer",
            ));
        }
        for keyword in ["maximum", "exclusiveMaximum", "minimum", "exclusiveMinimum"] {
            if let Some(value) = schema.get(keyword)
                && exact_integer(value).is_none()
            {
                return Err(invalid_profile(&format!(
                    "{keyword} must be a JSON integer"
                )));
            }
        }
        for keyword in [
            "maxLength",
            "minLength",
            "maxItems",
            "minItems",
            "maxProperties",
            "minProperties",
        ] {
            if let Some(value) = schema.get(keyword)
                && value.as_u64().is_none()
            {
                return Err(invalid_profile(&format!(
                    "{keyword} must be a non-negative JSON integer"
                )));
            }
        }
        if let Some(pattern) = schema.get("pattern") {
            let Some(pattern) = pattern.as_str() else {
                return Err(invalid_profile("pattern must be a string"));
            };
            if pattern.len() > MAX_PATTERN_BYTES || Regex::new(pattern).is_err() {
                return Err(InvocationSchemaError::UnsupportedPattern);
            }
        }
        if schema
            .get("uniqueItems")
            .is_some_and(|value| !value.is_boolean())
        {
            return Err(invalid_profile("uniqueItems must be boolean"));
        }
        if let Some(required) = schema.get("required") {
            validate_unique_string_array(required)?;
        }
        if let Some(items) = schema.get("items") {
            self.validate(items, depth + 1)?;
        }
        if let Some(additional) = schema.get("additionalProperties") {
            self.validate(additional, depth + 1)?;
        }
        if let Some(properties) = schema.get("properties") {
            let Some(properties) = properties.as_object() else {
                return Err(invalid_profile("properties must be an object"));
            };
            for property_schema in properties.values() {
                self.validate(property_schema, depth + 1)?;
            }
        }
        Ok(())
    }

    fn charge(&mut self, amount: usize) -> Result<(), InvocationSchemaError> {
        self.remaining_work = self.remaining_work.checked_sub(amount).ok_or(
            InvocationSchemaError::SchemaWorkExceeded {
                maximum: MAX_INVOCATION_VALUE_WORK,
            },
        )?;
        Ok(())
    }
}

fn validate_profile_types(types: &Value) -> Result<(), InvocationSchemaError> {
    let names = match types {
        Value::String(name) => vec![name.as_str()],
        Value::Array(values) if !values.is_empty() => values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .ok_or_else(|| invalid_profile("type array entries must be strings"))
            })
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Err(invalid_profile("type must be a string or non-empty array")),
    };
    let mut unique = BTreeSet::new();
    for name in names {
        if !matches!(
            name,
            "null" | "boolean" | "object" | "array" | "string" | "integer"
        ) {
            return Err(InvocationSchemaError::UnsupportedType {
                type_name: bounded(name),
            });
        }
        if !unique.insert(name) {
            return Err(invalid_profile("type entries must be unique"));
        }
    }
    Ok(())
}

fn validate_unique_string_array(value: &Value) -> Result<(), InvocationSchemaError> {
    let Some(values) = value.as_array() else {
        return Err(invalid_profile("required must be an array"));
    };
    let mut unique = BTreeSet::new();
    for value in values {
        let Some(value) = value.as_str() else {
            return Err(invalid_profile("required entries must be strings"));
        };
        if !unique.insert(value) {
            return Err(invalid_profile("required entries must be unique"));
        }
    }
    Ok(())
}

struct Evaluator {
    remaining_work: usize,
}

impl Evaluator {
    fn evaluate(
        &mut self,
        schema: &Value,
        instance: &Value,
        path: &str,
        depth: usize,
    ) -> Result<(), InvocationSchemaError> {
        if depth > MAX_INVOCATION_VALUE_DEPTH {
            return Err(InvocationSchemaError::InstanceDepthExceeded {
                maximum: MAX_INVOCATION_VALUE_DEPTH,
            });
        }
        self.charge(1)?;
        match schema {
            Value::Bool(true) => Ok(()),
            Value::Bool(false) => Err(invalid_instance(path, "false_schema")),
            Value::Object(object) => self.evaluate_object(object, instance, path, depth),
            _ => Err(invalid_profile("schema root must be a boolean or object")),
        }
    }

    fn evaluate_object(
        &mut self,
        schema: &Map<String, Value>,
        instance: &Value,
        path: &str,
        depth: usize,
    ) -> Result<(), InvocationSchemaError> {
        if let Some(types) = schema.get("type")
            && !matches_type(types, instance)
        {
            return Err(invalid_instance(path, "type"));
        }
        if let Some(expected) = schema.get("const")
            && !self.instance_equal(expected, instance)?
        {
            return Err(invalid_instance(path, "const"));
        }
        if let Some(values) = schema.get("enum").and_then(Value::as_array) {
            let mut matched = false;
            for expected in values {
                if self.instance_equal(expected, instance)? {
                    matched = true;
                    break;
                }
            }
            if !matched {
                return Err(invalid_instance(path, "enum"));
            }
        }
        if exact_integer(instance).is_some() {
            self.evaluate_integer(schema, instance, path)?;
        }
        if let Some(value) = instance.as_str() {
            self.evaluate_string(schema, value, path)?;
        }
        if let Some(values) = instance.as_array() {
            self.evaluate_array(schema, values, path, depth)?;
        }
        if let Some(object) = instance.as_object() {
            self.evaluate_instance_object(schema, object, path, depth)?;
        }
        Ok(())
    }

    fn evaluate_integer(
        &mut self,
        schema: &Map<String, Value>,
        instance: &Value,
        path: &str,
    ) -> Result<(), InvocationSchemaError> {
        let actual = exact_integer(instance).expect("caller established exact integer");
        for (keyword, allowed) in [
            (
                "minimum",
                schema
                    .get("minimum")
                    .and_then(exact_integer)
                    .is_none_or(|bound| actual.cmp(&bound) != Ordering::Less),
            ),
            (
                "maximum",
                schema
                    .get("maximum")
                    .and_then(exact_integer)
                    .is_none_or(|bound| actual.cmp(&bound) != Ordering::Greater),
            ),
            (
                "exclusiveMinimum",
                schema
                    .get("exclusiveMinimum")
                    .and_then(exact_integer)
                    .is_none_or(|bound| actual.cmp(&bound) == Ordering::Greater),
            ),
            (
                "exclusiveMaximum",
                schema
                    .get("exclusiveMaximum")
                    .and_then(exact_integer)
                    .is_none_or(|bound| actual.cmp(&bound) == Ordering::Less),
            ),
        ] {
            if !allowed {
                return Err(invalid_instance(path, keyword));
            }
        }
        if let Some(multiple) = schema.get("multipleOf").and_then(exact_positive_integer)
            && actual.as_i128() % multiple.as_i128() != 0
        {
            return Err(invalid_instance(path, "multipleOf"));
        }
        Ok(())
    }

    fn evaluate_string(
        &mut self,
        schema: &Map<String, Value>,
        value: &str,
        path: &str,
    ) -> Result<(), InvocationSchemaError> {
        let length = value.chars().count() as u64;
        if schema
            .get("minLength")
            .and_then(Value::as_u64)
            .is_some_and(|minimum| length < minimum)
        {
            return Err(invalid_instance(path, "minLength"));
        }
        if schema
            .get("maxLength")
            .and_then(Value::as_u64)
            .is_some_and(|maximum| length > maximum)
        {
            return Err(invalid_instance(path, "maxLength"));
        }
        if let Some(pattern) = schema.get("pattern").and_then(Value::as_str) {
            self.charge(pattern.len().max(1))?;
            let expression =
                Regex::new(pattern).map_err(|_| InvocationSchemaError::UnsupportedPattern)?;
            if !expression.is_match(value) {
                return Err(invalid_instance(path, "pattern"));
            }
        }
        Ok(())
    }

    fn evaluate_array(
        &mut self,
        schema: &Map<String, Value>,
        values: &[Value],
        path: &str,
        depth: usize,
    ) -> Result<(), InvocationSchemaError> {
        let length = values.len() as u64;
        if schema
            .get("minItems")
            .and_then(Value::as_u64)
            .is_some_and(|minimum| length < minimum)
        {
            return Err(invalid_instance(path, "minItems"));
        }
        if schema
            .get("maxItems")
            .and_then(Value::as_u64)
            .is_some_and(|maximum| length > maximum)
        {
            return Err(invalid_instance(path, "maxItems"));
        }
        if schema.get("uniqueItems") == Some(&Value::Bool(true)) {
            for left in 0..values.len() {
                for right in left + 1..values.len() {
                    if self.instance_equal(&values[left], &values[right])? {
                        return Err(invalid_instance(path, "uniqueItems"));
                    }
                }
            }
        }
        if let Some(item_schema) = schema.get("items") {
            for (index, value) in values.iter().enumerate() {
                self.evaluate(
                    item_schema,
                    value,
                    &child_path(path, &index.to_string()),
                    depth + 1,
                )?;
            }
        }
        Ok(())
    }

    fn instance_equal(
        &mut self,
        left: &Value,
        right: &Value,
    ) -> Result<bool, InvocationSchemaError> {
        self.charge(1)?;
        match (left, right) {
            (Value::Null, Value::Null) => Ok(true),
            (Value::Bool(left), Value::Bool(right)) => Ok(left == right),
            (Value::Number(left), Value::Number(right)) => Ok(left == right),
            (Value::String(left), Value::String(right)) => Ok(left == right),
            (Value::Array(left), Value::Array(right)) => {
                if left.len() != right.len() {
                    return Ok(false);
                }
                for (left, right) in left.iter().zip(right) {
                    if !self.instance_equal(left, right)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            (Value::Object(left), Value::Object(right)) => {
                if left.len() != right.len() {
                    return Ok(false);
                }
                for (key, left) in left {
                    let Some(right) = right.get(key) else {
                        return Ok(false);
                    };
                    if !self.instance_equal(left, right)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn evaluate_instance_object(
        &mut self,
        schema: &Map<String, Value>,
        object: &Map<String, Value>,
        path: &str,
        depth: usize,
    ) -> Result<(), InvocationSchemaError> {
        let length = object.len() as u64;
        if schema
            .get("minProperties")
            .and_then(Value::as_u64)
            .is_some_and(|minimum| length < minimum)
        {
            return Err(invalid_instance(path, "minProperties"));
        }
        if schema
            .get("maxProperties")
            .and_then(Value::as_u64)
            .is_some_and(|maximum| length > maximum)
        {
            return Err(invalid_instance(path, "maxProperties"));
        }
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for key in required.iter().filter_map(Value::as_str) {
                self.charge(1)?;
                if !object.contains_key(key) {
                    return Err(invalid_instance(path, "required"));
                }
            }
        }
        let properties = schema.get("properties").and_then(Value::as_object);
        for (key, value) in object {
            if let Some(property_schema) = properties.and_then(|map| map.get(key)) {
                self.evaluate(property_schema, value, &child_path(path, key), depth + 1)?;
            } else if let Some(additional) = schema.get("additionalProperties") {
                self.evaluate(additional, value, &child_path(path, key), depth + 1)?;
            }
        }
        Ok(())
    }

    fn charge(&mut self, amount: usize) -> Result<(), InvocationSchemaError> {
        self.remaining_work = self.remaining_work.checked_sub(amount).ok_or(
            InvocationSchemaError::EvaluationWorkExceeded {
                maximum: MAX_INVOCATION_VALUE_WORK,
            },
        )?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExactInteger {
    Negative(i64),
    NonNegative(u64),
}

impl ExactInteger {
    fn as_i128(self) -> i128 {
        match self {
            Self::Negative(value) => i128::from(value),
            Self::NonNegative(value) => i128::from(value),
        }
    }

    fn cmp(&self, other: &Self) -> Ordering {
        self.as_i128().cmp(&other.as_i128())
    }
}

fn exact_integer(value: &Value) -> Option<ExactInteger> {
    let number = value.as_number()?;
    if let Some(value) = number.as_i64() {
        return Some(if value < 0 {
            ExactInteger::Negative(value)
        } else {
            ExactInteger::NonNegative(value as u64)
        });
    }
    number.as_u64().map(ExactInteger::NonNegative)
}

fn exact_positive_integer(value: &Value) -> Option<ExactInteger> {
    exact_integer(value).filter(|integer| integer.as_i128() > 0)
}

fn matches_type(schema_type: &Value, instance: &Value) -> bool {
    match schema_type {
        Value::String(name) => matches_type_name(name, instance),
        Value::Array(names) => names
            .iter()
            .filter_map(Value::as_str)
            .any(|name| matches_type_name(name, instance)),
        _ => false,
    }
}

fn matches_type_name(name: &str, instance: &Value) -> bool {
    match name {
        "null" => instance.is_null(),
        "boolean" => instance.is_boolean(),
        "object" => instance.is_object(),
        "array" => instance.is_array(),
        "string" => instance.is_string(),
        "integer" => exact_integer(instance).is_some(),
        _ => false,
    }
}

fn invalid_profile(reason: &str) -> InvocationSchemaError {
    InvocationSchemaError::InvalidProfileSchema {
        reason: bounded(reason),
    }
}

fn invalid_instance(path: &str, keyword: &str) -> InvocationSchemaError {
    InvocationSchemaError::InvalidInstance {
        path: bounded(path),
        keyword: bounded(keyword),
    }
}

fn child_path(parent: &str, key: &str) -> String {
    let escaped = key.replace('~', "~0").replace('/', "~1");
    bounded(&format!("{parent}/{escaped}"))
}

fn bounded(value: &str) -> String {
    if value.len() <= MAX_ERROR_PATH_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_ERROR_PATH_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{
        InvocationSchemaError, MAX_INVOCATION_VALUE_DEPTH, validate_invocation_instance,
        validate_invocation_schema_profile,
    };

    fn artifact_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "reference": {"type": "string", "pattern": "^artifact:sha256:[0-9a-f]{64}$"},
                "offset": {"type": "integer", "minimum": 0, "maximum": u64::MAX},
                "length": {"type": "integer", "minimum": 1, "maximum": 16384}
            },
            "required": ["reference", "offset", "length"],
            "additionalProperties": false
        })
    }

    #[test]
    fn artifact_profile_is_exact_and_rejects_float_integer_spelling() {
        let schema = artifact_schema();
        let reference = format!("artifact:sha256:{}", "a".repeat(64));
        validate_invocation_instance(
            &schema,
            &json!({"reference": reference, "offset": 0, "length": 16384}),
        )
        .expect("valid artifact arguments");
        for invalid in [
            json!({"reference": format!("artifact:sha256:{}", "a".repeat(64)), "offset": 0, "length": 1.0}),
            json!({"reference": "artifact:sha256:AA", "offset": 0, "length": 1}),
            json!({"reference": format!("artifact:sha256:{}", "a".repeat(64)), "offset": -1, "length": 1}),
            json!({"reference": format!("artifact:sha256:{}", "a".repeat(64)), "offset": 0, "length": 0}),
            json!({"reference": format!("artifact:sha256:{}", "a".repeat(64)), "offset": 0, "length": 1, "effect": "content"}),
        ] {
            assert!(matches!(
                validate_invocation_instance(&schema, &invalid),
                Err(InvocationSchemaError::InvalidInstance { .. })
            ));
        }
    }

    #[test]
    fn equality_keywords_distinguish_integer_and_float_representations() {
        assert!(validate_invocation_instance(&json!({"const": 1}), &json!(1)).is_ok());
        assert!(validate_invocation_instance(&json!({"const": 1}), &json!(1.0)).is_err());
        assert!(validate_invocation_instance(&json!({"enum": [1]}), &json!(1)).is_ok());
        assert!(validate_invocation_instance(&json!({"enum": [1]}), &json!(1.0)).is_err());
        validate_invocation_instance(
            &json!({"type": "array", "uniqueItems": true}),
            &json!([1, 1.0]),
        )
        .expect("representationally distinct numbers are unique");
        assert!(
            validate_invocation_instance(
                &json!({"type": "array", "uniqueItems": true}),
                &json!([1, 1])
            )
            .is_err()
        );
    }

    #[test]
    fn nested_unique_items_charge_recursive_comparison_work() {
        let values = (0_u64..400)
            .map(|suffix| {
                let mut value = vec![json!(0); 199];
                value.push(json!(suffix));
                Value::Array(value)
            })
            .collect::<Vec<_>>();
        assert!(values.len() * (values.len() - 1) / 2 < super::MAX_INVOCATION_VALUE_WORK);
        assert!(
            super::validate_invocation_argument_envelope(&Value::Array(values.clone())).is_ok()
        );
        assert_eq!(
            validate_invocation_instance(
                &json!({"type": "array", "uniqueItems": true}),
                &Value::Array(values)
            ),
            Err(InvocationSchemaError::EvaluationWorkExceeded {
                maximum: super::MAX_INVOCATION_VALUE_WORK
            })
        );
    }

    #[test]
    fn large_integers_and_multiple_of_use_exact_arithmetic() {
        let exact = json!({
            "type": "integer",
            "minimum": 9_007_199_254_740_993_u64,
            "maximum": 9_007_199_254_740_995_u64,
            "multipleOf": 3
        });
        validate_invocation_instance(&exact, &json!(9_007_199_254_740_993_u64))
            .expect("large exact multiple");
        assert!(validate_invocation_instance(&exact, &json!(9_007_199_254_740_994_u64)).is_err());
        assert!(validate_invocation_instance(&exact, &json!(9_007_199_254_740_996_u64)).is_err());
    }

    #[test]
    fn structural_depth_is_rejected_by_iterative_preflight() {
        let mut schema = json!({"type": "integer"});
        for _ in 0..=MAX_INVOCATION_VALUE_DEPTH {
            schema = json!({"type": "array", "items": schema});
        }
        assert_eq!(
            validate_invocation_schema_profile(&schema),
            Err(InvocationSchemaError::SchemaDepthExceeded {
                maximum: MAX_INVOCATION_VALUE_DEPTH
            })
        );
    }

    #[test]
    fn structural_work_is_rejected_by_iterative_preflight() {
        let schema = json!({"const": vec![Value::Null; super::MAX_INVOCATION_VALUE_WORK]});
        assert_eq!(
            validate_invocation_schema_profile(&schema),
            Err(InvocationSchemaError::SchemaWorkExceeded {
                maximum: super::MAX_INVOCATION_VALUE_WORK
            })
        );
    }

    #[test]
    fn unsupported_semantics_fail_at_profile_binding() {
        for schema in [
            json!({"type": "number"}),
            json!({"$ref": "#/definitions/value"}),
            json!({"allOf": [{"type": "integer"}]}),
        ] {
            assert!(validate_invocation_schema_profile(&schema).is_err());
        }
    }
}

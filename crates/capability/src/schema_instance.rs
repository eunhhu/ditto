use std::collections::BTreeSet;

use regex::Regex;
use serde_json::{Map, Value};
use thiserror::Error;

use super::validate_json_schema;

pub const MAX_INVOCATION_SCHEMA_BYTES: usize = 1024 * 1024;
pub const MAX_INVOCATION_ARGUMENT_BYTES: usize = 1024 * 1024;
pub const MAX_SCHEMA_INSTANCE_DEPTH: usize = 64;
pub const MAX_SCHEMA_INSTANCE_WORK: usize = 100_000;
const MAX_PATTERN_BYTES: usize = 16 * 1024;
const MAX_ERROR_PATH_BYTES: usize = 1024;

/// Fixed-budget failure from evaluating an invocation argument as a JSON
/// Schema Draft 2020-12 instance.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum JsonSchemaInstanceError {
    #[error("invocation JSON Schema is structurally invalid: {reason}")]
    InvalidSchema { reason: String },
    #[error("invocation JSON Schema is {actual} bytes, exceeding {maximum}")]
    SchemaTooLarge { actual: usize, maximum: usize },
    #[error("invocation arguments are {actual} bytes, exceeding {maximum}")]
    InstanceTooLarge { actual: usize, maximum: usize },
    #[error("JSON Schema instance evaluation exceeded depth {maximum}")]
    DepthExceeded { maximum: usize },
    #[error("JSON Schema instance evaluation exceeded {maximum} work units")]
    WorkExceeded { maximum: usize },
    #[error("JSON Schema instance is invalid at {path} ({keyword})")]
    InvalidInstance { path: String, keyword: String },
    #[error("JSON Schema reference is unsupported during invocation: {reference}")]
    UnsupportedReference { reference: String },
    #[error("JSON Schema keyword is unsupported during invocation: {keyword}")]
    UnsupportedKeyword { keyword: &'static str },
    #[error("JSON Schema pattern is unsupported during invocation")]
    UnsupportedPattern,
}

/// Evaluate one JSON value against a structurally valid provider-neutral
/// Draft 2020-12 schema under fixed deterministic limits.
pub fn validate_json_schema_instance(
    schema: &Value,
    instance: &Value,
) -> Result<(), JsonSchemaInstanceError> {
    validate_json_schema(schema).map_err(|error| JsonSchemaInstanceError::InvalidSchema {
        reason: error.to_string(),
    })?;
    let schema_bytes = serialized_len(schema);
    if schema_bytes > MAX_INVOCATION_SCHEMA_BYTES {
        return Err(JsonSchemaInstanceError::SchemaTooLarge {
            actual: schema_bytes,
            maximum: MAX_INVOCATION_SCHEMA_BYTES,
        });
    }
    let instance_bytes = serialized_len(instance);
    if instance_bytes > MAX_INVOCATION_ARGUMENT_BYTES {
        return Err(JsonSchemaInstanceError::InstanceTooLarge {
            actual: instance_bytes,
            maximum: MAX_INVOCATION_ARGUMENT_BYTES,
        });
    }

    Evaluator {
        root: schema,
        remaining_work: MAX_SCHEMA_INSTANCE_WORK,
    }
    .evaluate(schema, instance, "$", 0)
}

fn serialized_len(value: &Value) -> usize {
    serde_json::to_vec(value)
        .expect("serializing serde_json::Value cannot fail")
        .len()
}

struct Evaluator<'a> {
    root: &'a Value,
    remaining_work: usize,
}

impl Evaluator<'_> {
    fn evaluate(
        &mut self,
        schema: &Value,
        instance: &Value,
        path: &str,
        depth: usize,
    ) -> Result<(), JsonSchemaInstanceError> {
        if depth > MAX_SCHEMA_INSTANCE_DEPTH {
            return Err(JsonSchemaInstanceError::DepthExceeded {
                maximum: MAX_SCHEMA_INSTANCE_DEPTH,
            });
        }
        self.charge(1)?;
        match schema {
            Value::Bool(true) => Ok(()),
            Value::Bool(false) => Err(invalid(path, "false_schema")),
            Value::Object(object) => self.evaluate_object(object, instance, path, depth),
            _ => Err(JsonSchemaInstanceError::InvalidSchema {
                reason: "schema root is not a boolean or object".into(),
            }),
        }
    }

    fn evaluate_object(
        &mut self,
        schema: &Map<String, Value>,
        instance: &Value,
        path: &str,
        depth: usize,
    ) -> Result<(), JsonSchemaInstanceError> {
        for keyword in ["$dynamicRef", "unevaluatedItems", "unevaluatedProperties"] {
            if schema.contains_key(keyword) {
                return Err(JsonSchemaInstanceError::UnsupportedKeyword { keyword });
            }
        }

        if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
            let target = resolve_local_reference(self.root, reference)?;
            self.evaluate(target, instance, path, depth + 1)?;
        }

        if let Some(types) = schema.get("type")
            && !matches_type(types, instance)
        {
            return Err(invalid(path, "type"));
        }
        if let Some(expected) = schema.get("const")
            && expected != instance
        {
            return Err(invalid(path, "const"));
        }
        if let Some(Value::Array(values)) = schema.get("enum")
            && !values.iter().any(|value| value == instance)
        {
            return Err(invalid(path, "enum"));
        }

        if instance.is_number() {
            self.evaluate_number(schema, instance, path)?;
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

        self.evaluate_combiners(schema, instance, path, depth)
    }

    fn evaluate_number(
        &mut self,
        schema: &Map<String, Value>,
        instance: &Value,
        path: &str,
    ) -> Result<(), JsonSchemaInstanceError> {
        let Some(actual) = instance.as_f64() else {
            return Err(invalid(path, "number"));
        };
        for (keyword, predicate) in [
            ("minimum", actual >= number(schema.get("minimum"))),
            ("maximum", actual <= number(schema.get("maximum"))),
            (
                "exclusiveMinimum",
                actual > number(schema.get("exclusiveMinimum")),
            ),
            (
                "exclusiveMaximum",
                actual < number(schema.get("exclusiveMaximum")),
            ),
        ] {
            if schema.contains_key(keyword) && !predicate {
                return Err(invalid(path, keyword));
            }
        }
        if let Some(multiple) = schema.get("multipleOf").and_then(Value::as_f64) {
            let quotient = actual / multiple;
            let tolerance = f64::EPSILON * quotient.abs().max(1.0) * 8.0;
            if (quotient - quotient.round()).abs() > tolerance {
                return Err(invalid(path, "multipleOf"));
            }
        }
        Ok(())
    }

    fn evaluate_string(
        &mut self,
        schema: &Map<String, Value>,
        value: &str,
        path: &str,
    ) -> Result<(), JsonSchemaInstanceError> {
        let length = value.chars().count() as u64;
        if schema
            .get("minLength")
            .and_then(Value::as_u64)
            .is_some_and(|minimum| length < minimum)
        {
            return Err(invalid(path, "minLength"));
        }
        if schema
            .get("maxLength")
            .and_then(Value::as_u64)
            .is_some_and(|maximum| length > maximum)
        {
            return Err(invalid(path, "maxLength"));
        }
        if let Some(pattern) = schema.get("pattern").and_then(Value::as_str) {
            if pattern.len() > MAX_PATTERN_BYTES {
                return Err(JsonSchemaInstanceError::UnsupportedPattern);
            }
            self.charge(pattern.len().max(1))?;
            let expression =
                Regex::new(pattern).map_err(|_| JsonSchemaInstanceError::UnsupportedPattern)?;
            if !expression.is_match(value) {
                return Err(invalid(path, "pattern"));
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
    ) -> Result<(), JsonSchemaInstanceError> {
        let length = values.len() as u64;
        if schema
            .get("minItems")
            .and_then(Value::as_u64)
            .is_some_and(|minimum| length < minimum)
        {
            return Err(invalid(path, "minItems"));
        }
        if schema
            .get("maxItems")
            .and_then(Value::as_u64)
            .is_some_and(|maximum| length > maximum)
        {
            return Err(invalid(path, "maxItems"));
        }
        if schema.get("uniqueItems") == Some(&Value::Bool(true)) {
            for left in 0..values.len() {
                for right in left + 1..values.len() {
                    self.charge(1)?;
                    if values[left] == values[right] {
                        return Err(invalid(path, "uniqueItems"));
                    }
                }
            }
        }

        let prefix = schema
            .get("prefixItems")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        for (index, (item_schema, value)) in prefix.iter().zip(values).enumerate() {
            self.evaluate(
                item_schema,
                value,
                &child_path(path, &index.to_string()),
                depth + 1,
            )?;
        }
        if let Some(item_schema) = schema.get("items") {
            for (index, value) in values.iter().enumerate().skip(prefix.len()) {
                self.evaluate(
                    item_schema,
                    value,
                    &child_path(path, &index.to_string()),
                    depth + 1,
                )?;
            }
        }

        if let Some(contains) = schema.get("contains") {
            let mut matches = 0_u64;
            for (index, value) in values.iter().enumerate() {
                match self.evaluate(
                    contains,
                    value,
                    &child_path(path, &index.to_string()),
                    depth + 1,
                ) {
                    Ok(()) => matches += 1,
                    Err(error) if is_mismatch(&error) => {}
                    Err(error) => return Err(error),
                }
            }
            let minimum = schema
                .get("minContains")
                .and_then(Value::as_u64)
                .unwrap_or(1);
            let maximum = schema
                .get("maxContains")
                .and_then(Value::as_u64)
                .unwrap_or(u64::MAX);
            if matches < minimum || matches > maximum {
                return Err(invalid(path, "contains"));
            }
        }
        Ok(())
    }

    fn evaluate_instance_object(
        &mut self,
        schema: &Map<String, Value>,
        object: &Map<String, Value>,
        path: &str,
        depth: usize,
    ) -> Result<(), JsonSchemaInstanceError> {
        let length = object.len() as u64;
        if schema
            .get("minProperties")
            .and_then(Value::as_u64)
            .is_some_and(|minimum| length < minimum)
        {
            return Err(invalid(path, "minProperties"));
        }
        if schema
            .get("maxProperties")
            .and_then(Value::as_u64)
            .is_some_and(|maximum| length > maximum)
        {
            return Err(invalid(path, "maxProperties"));
        }
        if let Some(Value::Array(required)) = schema.get("required") {
            for key in required.iter().filter_map(Value::as_str) {
                self.charge(1)?;
                if !object.contains_key(key) {
                    return Err(invalid(path, "required"));
                }
            }
        }

        let properties = schema.get("properties").and_then(Value::as_object);
        let patterns = compile_pattern_map(schema.get("patternProperties"), self)?;
        let mut evaluated = BTreeSet::new();
        for (key, value) in object {
            let item_path = child_path(path, key);
            if let Some(property_schema) = properties.and_then(|map| map.get(key)) {
                self.evaluate(property_schema, value, &item_path, depth + 1)?;
                evaluated.insert(key.as_str());
            }
            for (pattern, pattern_schema) in &patterns {
                self.charge(1)?;
                if pattern.is_match(key) {
                    self.evaluate(pattern_schema, value, &item_path, depth + 1)?;
                    evaluated.insert(key.as_str());
                }
            }
        }
        if let Some(additional) = schema.get("additionalProperties") {
            for (key, value) in object {
                if !evaluated.contains(key.as_str()) {
                    self.evaluate(additional, value, &child_path(path, key), depth + 1)?;
                }
            }
        }

        if let Some(Value::Object(dependencies)) = schema.get("dependentRequired") {
            for (trigger, required) in dependencies {
                if object.contains_key(trigger)
                    && let Some(required) = required.as_array()
                {
                    for key in required.iter().filter_map(Value::as_str) {
                        self.charge(1)?;
                        if !object.contains_key(key) {
                            return Err(invalid(path, "dependentRequired"));
                        }
                    }
                }
            }
        }
        if let Some(Value::Object(dependencies)) = schema.get("dependentSchemas") {
            for (trigger, dependent_schema) in dependencies {
                if object.contains_key(trigger) {
                    self.evaluate(
                        dependent_schema,
                        &Value::Object(object.clone()),
                        path,
                        depth + 1,
                    )?;
                }
            }
        }
        if let Some(property_names) = schema.get("propertyNames") {
            for key in object.keys() {
                self.evaluate(
                    property_names,
                    &Value::String(key.clone()),
                    &child_path(path, key),
                    depth + 1,
                )?;
            }
        }
        Ok(())
    }

    fn evaluate_combiners(
        &mut self,
        schema: &Map<String, Value>,
        instance: &Value,
        path: &str,
        depth: usize,
    ) -> Result<(), JsonSchemaInstanceError> {
        if let Some(Value::Array(branches)) = schema.get("allOf") {
            for branch in branches {
                self.evaluate(branch, instance, path, depth + 1)?;
            }
        }
        if let Some(Value::Array(branches)) = schema.get("anyOf") {
            let mut matched = false;
            for branch in branches {
                match self.evaluate(branch, instance, path, depth + 1) {
                    Ok(()) => matched = true,
                    Err(error) if is_mismatch(&error) => {}
                    Err(error) => return Err(error),
                }
            }
            if !matched {
                return Err(invalid(path, "anyOf"));
            }
        }
        if let Some(Value::Array(branches)) = schema.get("oneOf") {
            let mut matches = 0;
            for branch in branches {
                match self.evaluate(branch, instance, path, depth + 1) {
                    Ok(()) => matches += 1,
                    Err(error) if is_mismatch(&error) => {}
                    Err(error) => return Err(error),
                }
            }
            if matches != 1 {
                return Err(invalid(path, "oneOf"));
            }
        }
        if let Some(not_schema) = schema.get("not") {
            match self.evaluate(not_schema, instance, path, depth + 1) {
                Ok(()) => return Err(invalid(path, "not")),
                Err(error) if is_mismatch(&error) => {}
                Err(error) => return Err(error),
            }
        }
        if let Some(if_schema) = schema.get("if") {
            match self.evaluate(if_schema, instance, path, depth + 1) {
                Ok(()) => {
                    if let Some(then_schema) = schema.get("then") {
                        self.evaluate(then_schema, instance, path, depth + 1)?;
                    }
                }
                Err(error) if is_mismatch(&error) => {
                    if let Some(else_schema) = schema.get("else") {
                        self.evaluate(else_schema, instance, path, depth + 1)?;
                    }
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    fn charge(&mut self, amount: usize) -> Result<(), JsonSchemaInstanceError> {
        self.remaining_work = self.remaining_work.checked_sub(amount).ok_or(
            JsonSchemaInstanceError::WorkExceeded {
                maximum: MAX_SCHEMA_INSTANCE_WORK,
            },
        )?;
        Ok(())
    }
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
        "number" => instance.is_number(),
        "string" => instance.is_string(),
        "integer" => instance
            .as_number()
            .is_some_and(|number| number.as_i64().is_some() || number.as_u64().is_some()),
        _ => false,
    }
}

fn number(value: Option<&Value>) -> f64 {
    value.and_then(Value::as_f64).unwrap_or(0.0)
}

fn compile_pattern_map<'a>(
    value: Option<&'a Value>,
    evaluator: &mut Evaluator<'_>,
) -> Result<Vec<(Regex, &'a Value)>, JsonSchemaInstanceError> {
    let Some(map) = value.and_then(Value::as_object) else {
        return Ok(Vec::new());
    };
    let mut patterns = Vec::with_capacity(map.len());
    for (pattern, schema) in map {
        if pattern.len() > MAX_PATTERN_BYTES {
            return Err(JsonSchemaInstanceError::UnsupportedPattern);
        }
        evaluator.charge(pattern.len().max(1))?;
        patterns.push((
            Regex::new(pattern).map_err(|_| JsonSchemaInstanceError::UnsupportedPattern)?,
            schema,
        ));
    }
    Ok(patterns)
}

fn resolve_local_reference<'a>(
    root: &'a Value,
    reference: &str,
) -> Result<&'a Value, JsonSchemaInstanceError> {
    if reference == "#" {
        return Ok(root);
    }
    let Some(pointer) = reference.strip_prefix("#/") else {
        return Err(JsonSchemaInstanceError::UnsupportedReference {
            reference: bounded(reference),
        });
    };
    let mut current = root;
    for raw in pointer.split('/') {
        let token = raw.replace("~1", "/").replace("~0", "~");
        current = current
            .as_object()
            .and_then(|object| object.get(&token))
            .or_else(|| {
                token
                    .parse::<usize>()
                    .ok()
                    .and_then(|index| current.as_array().and_then(|array| array.get(index)))
            })
            .ok_or_else(|| JsonSchemaInstanceError::UnsupportedReference {
                reference: bounded(reference),
            })?;
    }
    Ok(current)
}

fn invalid(path: &str, keyword: &str) -> JsonSchemaInstanceError {
    JsonSchemaInstanceError::InvalidInstance {
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

fn is_mismatch(error: &JsonSchemaInstanceError) -> bool {
    matches!(error, JsonSchemaInstanceError::InvalidInstance { .. })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        JsonSchemaInstanceError, MAX_SCHEMA_INSTANCE_DEPTH, validate_json_schema_instance,
    };

    #[test]
    fn validates_object_ranges_patterns_and_unknown_fields() {
        let schema = json!({
            "type": "object",
            "properties": {
                "reference": {"type": "string", "pattern": "^artifact:sha256:[0-9a-f]{64}$"},
                "offset": {"type": "integer", "minimum": 0},
                "length": {"type": "integer", "minimum": 1, "maximum": 16384}
            },
            "required": ["reference", "offset", "length"],
            "additionalProperties": false
        });
        validate_json_schema_instance(
            &schema,
            &json!({
                "reference": format!("artifact:sha256:{}", "a".repeat(64)),
                "offset": 0,
                "length": 16384
            }),
        )
        .expect("valid instance");
        for invalid in [
            json!({"reference": "artifact:sha256:AA", "offset": 0, "length": 1}),
            json!({"reference": format!("artifact:sha256:{}", "a".repeat(64)), "offset": -1, "length": 1}),
            json!({"reference": format!("artifact:sha256:{}", "a".repeat(64)), "offset": 0, "length": 0}),
            json!({"reference": format!("artifact:sha256:{}", "a".repeat(64)), "offset": 0, "length": 1, "effect": "content"}),
        ] {
            assert!(matches!(
                validate_json_schema_instance(&schema, &invalid),
                Err(JsonSchemaInstanceError::InvalidInstance { .. })
            ));
        }
    }

    #[test]
    fn combiners_and_local_refs_are_evaluated() {
        let schema = json!({
            "$defs": {"positive": {"type": "integer", "minimum": 1}},
            "allOf": [
                {"$ref": "#/$defs/positive"},
                {"not": {"const": 2}},
                {"anyOf": [{"const": 1}, {"const": 3}]}
            ]
        });
        validate_json_schema_instance(&schema, &json!(3)).expect("matches all branches");
        assert!(validate_json_schema_instance(&schema, &json!(2)).is_err());
    }

    #[test]
    fn recursive_refs_stop_at_the_fixed_depth() {
        let schema = json!({"$ref": "#"});
        assert_eq!(
            validate_json_schema_instance(&schema, &json!(null)),
            Err(JsonSchemaInstanceError::DepthExceeded {
                maximum: MAX_SCHEMA_INSTANCE_DEPTH
            })
        );
    }

    #[test]
    fn unsupported_runtime_keywords_fail_closed() {
        let schema = json!({"unevaluatedProperties": false});
        assert_eq!(
            validate_json_schema_instance(&schema, &json!({})),
            Err(JsonSchemaInstanceError::UnsupportedKeyword {
                keyword: "unevaluatedProperties"
            })
        );
    }
}

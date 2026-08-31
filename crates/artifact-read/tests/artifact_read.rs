use std::fs;

#[allow(deprecated)]
use ditto_artifact_read::{
    ARTIFACT_READ_ID, ARTIFACT_READ_VERSION, ArtifactReadArguments, ArtifactReadAuthority,
    ArtifactReadError, ArtifactReadErrorCode, ArtifactReadExecutor, ArtifactReadNormalizer,
    ArtifactReadRequest, ArtifactReadResource, ArtifactReadResult, MAX_READ_BYTES,
};
use ditto_artifact_store::ArtifactStore;
use serde_json::json;
use tempfile::tempdir;

fn authority_with_store() -> (tempfile::TempDir, ArtifactStore, ArtifactReadAuthority) {
    let directory = tempdir().expect("temporary directory");
    let store = ArtifactStore::open(directory.path()).expect("open artifact store");
    let authority = ArtifactReadAuthority::new(store.clone());
    (directory, store, authority)
}

fn result_success(result: &ArtifactReadResult) -> &ditto_artifact_read::ArtifactReadSuccess {
    result
        .success_projection()
        .expect("expected successful artifact read")
}

fn result_error(result: &ArtifactReadResult) -> &ditto_artifact_read::ArtifactReadError {
    result
        .error_projection()
        .expect("expected artifact read error")
}

fn invoke(authority: &ArtifactReadAuthority, arguments: serde_json::Value) -> ArtifactReadResult {
    match ArtifactReadNormalizer.normalize(arguments) {
        Ok(resource) => authority.execute(&resource),
        Err(error) => ArtifactReadResult::error(error),
    }
}

#[test]
fn successful_read_is_binary_safe_and_bounded() {
    let (_directory, store, authority) = authority_with_store();
    let bytes = [0_u8, 1, 2, 127, 128, 254, 255];
    let metadata = store.put(bytes.as_slice()).expect("store binary artifact");

    let result = invoke(
        &authority,
        json!({
            "reference": metadata.reference,
            "offset": 2,
            "length": 4,
        }),
    );
    let success = result_success(&result);
    assert!(!result.is_error());
    assert_eq!(success.reference(), &metadata.reference);
    assert_eq!(success.offset(), 2);
    assert_eq!(success.requested_bytes(), 4);
    assert_eq!(success.returned_bytes(), 4);
    assert_eq!(success.total_bytes(), bytes.len() as u64);
    assert!(!success.eof());
    assert_eq!(success.decoded_data().expect("decode base64"), &bytes[2..6]);
    let wire = serde_json::to_value(&result).expect("serialize binary result");
    assert_eq!(wire["data"], "An+A/g==");
}

#[test]
fn eof_and_crossing_ranges_have_stable_semantics() {
    let (_directory, store, authority) = authority_with_store();
    let metadata = store.put(b"012345").expect("store artifact");
    let reference = metadata.reference.to_string();

    let exact_eof = invoke(
        &authority,
        json!({
            "reference": reference,
            "offset": 6,
            "length": 1,
        }),
    );
    let exact = result_success(&exact_eof);
    assert_eq!(exact.returned_bytes(), 0);
    assert_eq!(exact.data(), "");
    assert!(exact.eof());

    let crossing = invoke(
        &authority,
        json!({
            "reference": metadata.reference,
            "offset": 4,
            "length": 10,
        }),
    );
    let truncated = result_success(&crossing);
    assert_eq!(truncated.requested_bytes(), 10);
    assert_eq!(truncated.returned_bytes(), 2);
    assert_eq!(
        truncated.decoded_data().expect("decode truncated range"),
        b"45"
    );
    assert!(truncated.eof());

    let beyond = invoke(
        &authority,
        json!({
            "reference": reference,
            "offset": 7,
            "length": 1,
        }),
    );
    let error = result_error(&beyond);
    assert!(beyond.is_error());
    assert_eq!(error.code(), "range_out_of_bounds");
    assert_eq!(
        error.message(),
        "artifact offset is beyond the end of the artifact"
    );
    assert_eq!(error.reference().map(ToString::to_string), Some(reference));
}

#[test]
fn max_read_is_accepted_and_n_plus_one_is_rejected_before_storage_access() {
    assert_eq!(MAX_READ_BYTES, 16 * 1024);
    let normalizer = ArtifactReadNormalizer;
    let reference = format!("artifact:sha256:{}", "c".repeat(64));
    let accepted = normalizer
        .normalize(json!({
            "reference": reference,
            "offset": 0,
            "length": MAX_READ_BYTES,
        }))
        .expect("accept max read length");
    assert_eq!(accepted.length(), MAX_READ_BYTES as u64);

    let rejected = normalizer
        .normalize(json!({
            "reference": reference,
            "offset": 0,
            "length": MAX_READ_BYTES + 1,
        }))
        .expect_err("reject max read plus one");
    assert_eq!(rejected.code(), "invalid_arguments");
}

#[test]
fn malformed_references_and_unknown_fields_never_reach_store() {
    let (_directory, _store, authority) = authority_with_store();
    let valid_reference = format!("artifact:sha256:{}", "d".repeat(64));
    let invalid_inputs = [
        json!({"reference": valid_reference, "offset": 0, "length": 1, "unknown": true}),
        json!({"reference": valid_reference, "offset": 0}),
        json!({"reference": valid_reference, "offset": -1, "length": 1}),
        json!({"reference": valid_reference, "offset": 0, "length": 0}),
        json!({"reference": valid_reference, "offset": 0, "length": MAX_READ_BYTES + 1}),
    ];
    for input in invalid_inputs {
        let result = invoke(&authority, input);
        assert!(result.is_error());
        assert_eq!(result_error(&result).code(), "invalid_arguments");
    }

    for malformed in ["../../state.db", "artifact:sha256:ABC"] {
        let result = invoke(
            &authority,
            json!({
                "reference": malformed,
                "offset": 0,
                "length": 1,
            }),
        );
        assert!(result.is_error());
        assert_eq!(result_error(&result).code(), "invalid_reference");
    }

    let malformed_reference = json!({
        "reference": "../../state.db",
        "offset": 0,
        "length": 1,
    });
    let result = invoke(&authority, malformed_reference);
    assert_eq!(result_error(&result).code(), "invalid_reference");
    let encoded = serde_json::to_string(&result).expect("serialize invalid reference result");
    assert!(!encoded.contains("state.db"));
}

#[test]
fn serde_arguments_are_strict_before_normalization() {
    let reference = format!("artifact:sha256:{}", "f".repeat(64));
    let valid = json!({
        "reference": reference,
        "offset": 0,
        "length": 1,
    });
    let parsed: ArtifactReadArguments = serde_json::from_value(valid).expect("parse arguments");
    assert_eq!(parsed.offset(), 0);
    assert_eq!(parsed.length(), 1);

    for invalid in [
        json!({"reference": reference, "offset": 0, "length": 0}),
        json!({"reference": reference, "offset": 0, "length": MAX_READ_BYTES + 1}),
        json!({"reference": reference, "offset": -1, "length": 1}),
        json!({"reference": reference, "offset": 0, "length": 1, "extra": true}),
        json!({"reference": reference, "length": 1}),
    ] {
        assert!(serde_json::from_value::<ArtifactReadArguments>(invalid).is_err());
    }
}

#[test]
fn tampered_object_returns_integrity_error_without_local_details() {
    let (directory, store, authority) = authority_with_store();
    let metadata = store.put(b"original").expect("store artifact");
    let object_path = directory
        .path()
        .join("sha256")
        .join(metadata.reference.sha256());
    fs::write(&object_path, b"tampered").expect("tamper object");

    let result = invoke(
        &authority,
        json!({
            "reference": metadata.reference,
            "offset": 0,
            "length": 1,
        }),
    );
    let error = result_error(&result);
    assert!(result.is_error());
    assert_eq!(error.code(), "integrity_failure");
    assert_eq!(error.message(), "artifact integrity verification failed");
    assert!(
        !error
            .message()
            .contains(directory.path().to_string_lossy().as_ref())
    );
    let encoded = serde_json::to_string(&result).expect("serialize integrity result");
    assert!(!encoded.contains(directory.path().to_string_lossy().as_ref()));
    assert!(!encoded.contains("calculated"));
}

#[test]
fn tampered_metadata_length_returns_serializable_integrity_error() {
    for tampered_length in [7_u64, 9_u64] {
        let (directory, store, authority) = authority_with_store();
        let metadata = store.put(b"original").expect("store artifact");
        let metadata_path = directory
            .path()
            .join("metadata")
            .join(format!("{}.json", metadata.reference.sha256()));
        let mut encoded: serde_json::Value =
            serde_json::from_slice(&fs::read(&metadata_path).expect("read metadata"))
                .expect("decode metadata");
        encoded["bytes"] = json!(tampered_length);
        fs::write(
            &metadata_path,
            serde_json::to_vec(&encoded).expect("encode metadata"),
        )
        .expect("tamper metadata length");

        let result = invoke(
            &authority,
            json!({
                "reference": metadata.reference,
                "offset": 0,
                "length": 4,
            }),
        );
        let error = result_error(&result);
        assert!(result.is_error());
        assert_eq!(error.code(), "integrity_failure");
        assert_eq!(error.message(), "artifact integrity verification failed");

        let wire = serde_json::to_value(&result).expect("serialize metadata integrity result");
        assert_eq!(wire["is_error"], true);
        assert_eq!(wire["error"]["code"], "integrity_failure");
        let decoded: ArtifactReadResult =
            serde_json::from_value(wire).expect("decode metadata integrity result");
        assert_eq!(decoded, result);
    }
}

#[test]
fn malformed_metadata_returns_serializable_integrity_error() {
    let (directory, store, authority) = authority_with_store();
    let metadata = store.put(b"original").expect("store artifact");
    let metadata_path = directory
        .path()
        .join("metadata")
        .join(format!("{}.json", metadata.reference.sha256()));
    fs::write(&metadata_path, b"not-json").expect("tamper metadata");

    let result = invoke(
        &authority,
        json!({
            "reference": metadata.reference,
            "offset": 0,
            "length": 4,
        }),
    );
    let error = result_error(&result);
    assert!(result.is_error());
    assert_eq!(error.code(), "integrity_failure");
    assert_eq!(error.message(), "artifact integrity verification failed");
    assert!(serde_json::to_value(&result).is_ok());
}

#[test]
fn unauthorized_error_is_schema_valid_and_result_decode_is_strict() {
    let reference = ditto_artifact_store::ArtifactRef::from_sha256("e".repeat(64))
        .expect("canonical reference");
    let result = ArtifactReadResult::error(ditto_artifact_read::ArtifactReadError::not_authorized(
        reference.clone(),
    ));
    let wire = serde_json::to_value(&result).expect("serialize authorization result");
    assert_eq!(wire["error"]["code"], "unauthorized_reference");
    assert_eq!(wire["error"]["reference"], reference.to_string());
    assert_eq!(
        result_error(&result).code_kind(),
        Some(ArtifactReadErrorCode::UnauthorizedReference)
    );
    let decoded: ArtifactReadResult = serde_json::from_value(wire).expect("decode result");
    assert_eq!(decoded, result);

    let unknown_field = json!({
        "is_error": true,
        "error": {
            "code": "unauthorized_reference",
            "message": "artifact reference is not authorized for this turn",
            "reference": reference,
            "extra": "reject",
        }
    });
    assert!(serde_json::from_value::<ArtifactReadResult>(unknown_field).is_err());

    let noncanonical_message = json!({
        "is_error": true,
        "error": {
            "code": "invalid_arguments",
            "message": "arbitrary",
        }
    });
    assert!(serde_json::from_value::<ArtifactReadResult>(noncanonical_message).is_err());

    let missing_reference = json!({
        "is_error": true,
        "error": {
            "code": "range_out_of_bounds",
            "message": "artifact offset is beyond the end of the artifact",
        }
    });
    assert!(serde_json::from_value::<ArtifactReadResult>(missing_reference).is_err());

    let explicit_null_reference = json!({
        "is_error": true,
        "error": {
            "code": "invalid_arguments",
            "message": "artifact.read arguments are invalid",
            "reference": null,
        }
    });
    assert!(serde_json::from_value::<ArtifactReadResult>(explicit_null_reference).is_err());

    let null_required_reference = json!({
        "is_error": true,
        "error": {
            "code": "range_out_of_bounds",
            "message": "artifact offset is beyond the end of the artifact",
            "reference": null,
        }
    });
    assert!(serde_json::from_value::<ArtifactReadResult>(null_required_reference).is_err());

    let impossible_success = json!({
        "is_error": false,
        "reference": reference,
        "offset": 0,
        "requested_bytes": 4,
        "returned_bytes": 3,
        "total_bytes": 4,
        "eof": true,
        "data": "AA==",
    });
    assert!(serde_json::from_value::<ArtifactReadResult>(impossible_success).is_err());
}

#[test]
fn constants_match_capability_schema() {
    let schema = ditto_artifact_read::capability_schema();
    assert_eq!(env!("CARGO_PKG_VERSION"), "0.2.0");
    assert_eq!(schema.id, ARTIFACT_READ_ID);
    assert_eq!(schema.version, ARTIFACT_READ_VERSION);
    assert_eq!(ARTIFACT_READ_VERSION, "0.1.0");
    assert_eq!(schema.input_schema["additionalProperties"], false);
}

#[test]
#[allow(deprecated)]
fn legacy_api_wrappers_preserve_checked_behavior() {
    let (_directory, store, _authority) = authority_with_store();
    let metadata = store.put(b"legacy").expect("store artifact");
    let reference = metadata.reference.clone();
    let value = json!({
        "reference": reference,
        "offset": 0,
        "length": 3,
    });

    let resource = ArtifactReadResource::new(metadata.reference.clone(), 0, 3)
        .expect("legacy resource constructor remains checked");
    let request: ArtifactReadRequest = resource.clone();
    assert_eq!(request, resource);
    assert!(ArtifactReadResource::new(metadata.reference.clone(), 0, 0).is_err());

    let arguments = ArtifactReadArguments::new(metadata.reference.clone(), 0, 3)
        .expect("legacy argument constructor remains checked");
    assert_eq!(arguments.normalize(), resource);

    let normalizer = ArtifactReadNormalizer;
    let encoded = value.to_string();
    assert_eq!(
        normalizer.parse_json(&encoded).expect("parse JSON"),
        resource
    );
    assert_eq!(
        normalizer
            .parse_bytes(encoded.as_bytes())
            .expect("parse JSON bytes"),
        resource
    );
    assert_eq!(
        ditto_artifact_read::normalize_arguments(value.clone()).expect("free normalizer"),
        resource
    );
    assert_eq!(
        ditto_artifact_read::normalize(value.clone()).expect("short free normalizer"),
        resource
    );

    let executor: ArtifactReadExecutor = ArtifactReadAuthority::from_store(store);
    assert_eq!(executor.schema(), ditto_artifact_read::capability_schema());
    assert_eq!(
        executor
            .normalizer()
            .normalize(value.clone())
            .expect("authority normalizer"),
        resource
    );
    assert_eq!(
        executor
            .normalize(value.clone())
            .expect("authority normalize"),
        resource
    );
    assert_eq!(
        executor
            .normalize_arguments(value.clone())
            .expect("authority normalize arguments"),
        resource
    );

    let success = executor
        .invoke(value.clone())
        .into_success()
        .expect("legacy invoke success");
    assert_eq!(success.decoded_data().expect("decode legacy data"), b"leg");
    assert!(
        executor
            .execute_arguments(value.clone())
            .into_success()
            .is_some()
    );
    assert!(executor.read(&resource).into_success().is_some());

    let legacy_error = ArtifactReadError::new(
        ArtifactReadErrorCode::InvalidArguments,
        "ignored legacy message",
        Some(metadata.reference.clone()),
    );
    assert_eq!(legacy_error.code(), "invalid_arguments");
    assert!(legacy_error.reference().is_none());
    let legacy_error_result = ArtifactReadResult::error(legacy_error);
    assert!(legacy_error_result.into_error().is_some());

    let legacy_range = ArtifactReadError::new(
        ArtifactReadErrorCode::RangeOutOfBounds,
        "ignored legacy message",
        Some(metadata.reference.clone()),
    );
    assert_eq!(legacy_range.code(), "range_out_of_bounds");
    assert_eq!(legacy_range.reference(), Some(&metadata.reference));
    assert!(serde_json::to_value(ArtifactReadResult::error(legacy_range)).is_ok());

    let missing_reference = ArtifactReadError::new(
        ArtifactReadErrorCode::RangeOutOfBounds,
        "ignored legacy message",
        None,
    );
    assert_eq!(missing_reference.code(), "invalid_reference");
    assert!(serde_json::to_value(ArtifactReadResult::error(missing_reference)).is_ok());
}

use std::sync::{Arc, Mutex};

use ditto_retrieval::{
    CandidateCount, CapabilityRootLimit, Embedding, EmbeddingProvider, EmbeddingProviderError,
    EmbeddingPurpose, EmbeddingVector, ExecutionEpochLimit, MAX_CANONICAL_QUERY_BYTES,
    MAX_COMPONENT_BYTES, MAX_CONTEXT_RESULT_LIMIT, MAX_EMBEDDING_DESCRIPTOR_BYTES,
    MAX_EMBEDDING_DIMENSION, MAX_LEXICAL_TOKENS, MAX_REQUEST_BYTES, MAX_RETRIEVAL_DOCUMENT_BYTES,
    MAX_SET_ENTRIES, RetrievalDocument, RetrievalError, RetrievalMode, TaskQuery, TaskSignatureV2,
    cosine_similarity,
};

#[derive(Clone)]
struct FixtureProvider {
    calls: Arc<Mutex<Vec<(EmbeddingPurpose, String)>>>,
    result: Result<Embedding, EmbeddingProviderError>,
}

impl FixtureProvider {
    fn success() -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            result: Ok(Embedding::new("fixture-v1", vec![3.0, 4.0])),
        }
    }

    fn failure(detail: impl Into<String>) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            result: Err(EmbeddingProviderError::failure(detail)),
        }
    }

    fn calls(&self) -> Vec<(EmbeddingPurpose, String)> {
        self.calls.lock().expect("fixture mutex poisoned").clone()
    }
}

impl EmbeddingProvider for FixtureProvider {
    fn embed(
        &self,
        purpose: EmbeddingPurpose,
        text: &str,
    ) -> Result<Embedding, EmbeddingProviderError> {
        self.calls
            .lock()
            .expect("fixture mutex poisoned")
            .push((purpose, text.to_owned()));
        self.result.clone()
    }
}

#[test]
fn canonical_query_normalizes_all_signature_fields_and_embeds_once() {
    let signature = TaskSignatureV2 {
        request: "  Deploy\tDÉMO  ".into(),
        active_goal: Some("  Release\nCandidate ".into()),
        entities: vec![" Workspace ".into(), "workspace".into(), "App".into()],
        resources: vec!["  Device:Kitchen ".into(), "device:kitchen".into()],
        constraints: vec!["No   Network".into(), "no network".into()],
        expected_effect: Some("  Local   Preview ".into()),
    };
    let provider = FixtureProvider::success();

    let query = TaskQuery::with_provider(signature, Some(&provider)).expect("valid query");

    assert_eq!(query.version(), 1);
    assert_eq!(query.mode(), RetrievalMode::Embedded);
    assert_eq!(
        query.canonical_text(),
        "deploy démo release candidate app workspace device:kitchen no network local preview"
    );
    assert_eq!(
        query.lexical_tokens(),
        &[
            "app".to_owned(),
            "candidate".to_owned(),
            "deploy".to_owned(),
            "device".to_owned(),
            "démo".to_owned(),
            "kitchen".to_owned(),
            "local".to_owned(),
            "network".to_owned(),
            "no".to_owned(),
            "preview".to_owned(),
            "release".to_owned(),
            "workspace".to_owned(),
        ]
    );
    assert_eq!(
        query.exact_terms(),
        &[
            "app".to_owned(),
            "device:kitchen".to_owned(),
            "workspace".to_owned()
        ]
    );
    assert_eq!(query.signature().resources, vec!["device:kitchen"]);
    assert_eq!(
        query.embedding_descriptor().expect("descriptor").as_str(),
        "fixture-v1"
    );
    assert_eq!(query.embedding_dimension(), Some(2));

    let calls = provider.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, EmbeddingPurpose::Query);
    assert_eq!(calls[0].1, query.canonical_text());
}

#[test]
fn lexical_absence_never_calls_provider_and_configured_failure_never_falls_back() {
    let signature = TaskSignatureV2::new("inspect local state");
    let absent = FixtureProvider::success();
    let lexical = TaskQuery::with_provider(signature.clone(), None).expect("lexical query");
    assert_eq!(lexical.mode(), RetrievalMode::LexicalOnly);
    assert!(lexical.query_embedding().is_none());
    assert!(absent.calls().is_empty());

    let failing = FixtureProvider::failure("provider is unavailable");
    let error = TaskQuery::with_provider(signature, Some(&failing)).expect_err("must fail");
    assert_eq!(
        error,
        RetrievalError::ProviderFailure {
            detail: "provider is unavailable".into()
        }
    );
    assert_eq!(failing.calls().len(), 1);

    let exact_bound = "x".repeat(4_096);
    let exact_bound_provider = FixtureProvider::failure(exact_bound.clone());
    let error =
        TaskQuery::with_provider(TaskSignatureV2::new("inspect"), Some(&exact_bound_provider))
            .expect_err("provider failure at the detail bound must propagate");
    assert_eq!(
        error,
        RetrievalError::ProviderFailure {
            detail: exact_bound
        }
    );
    assert_eq!(exact_bound_provider.calls().len(), 1);

    let oversized = FixtureProvider::failure("x".repeat(4_097));
    let error = TaskQuery::with_provider(TaskSignatureV2::new("inspect"), Some(&oversized))
        .expect_err("oversized provider detail must fail closed");
    assert_eq!(
        error,
        RetrievalError::ProviderFailureDetailTooLong {
            actual: 4_097,
            maximum: 4_096,
        }
    );
    assert_eq!(oversized.calls().len(), 1);
}

#[test]
fn invalid_provider_output_is_typed_and_called_once_without_a_query() {
    let invalid = FixtureProvider {
        calls: Arc::new(Mutex::new(Vec::new())),
        result: Ok(Embedding::new("fixture-v1", vec![f32::NAN])),
    };
    let result = TaskQuery::with_provider(TaskSignatureV2::new("inspect"), Some(&invalid));
    assert!(matches!(
        result,
        Err(RetrievalError::NonFiniteEmbeddingValue { index: 0 })
    ));
    assert_eq!(invalid.calls().len(), 1);

    let empty_descriptor = FixtureProvider {
        calls: Arc::new(Mutex::new(Vec::new())),
        result: Ok(Embedding::new("", vec![1.0])),
    };
    let result = TaskQuery::with_provider(TaskSignatureV2::new("inspect"), Some(&empty_descriptor));
    assert_eq!(result, Err(RetrievalError::EmptyEmbeddingDescriptor));
    assert_eq!(empty_descriptor.calls().len(), 1);
}

#[test]
fn normalization_rejects_non_whitespace_controls_and_empty_components() {
    let mut request_control = TaskSignatureV2::new("inspect\0 state");
    assert_eq!(
        TaskQuery::new(request_control.clone()),
        Err(RetrievalError::ControlCharacter { field: "request" })
    );

    request_control.request = "inspect\u{1b} state".into();
    assert_eq!(
        TaskQuery::new(request_control),
        Err(RetrievalError::ControlCharacter { field: "request" })
    );

    let mut optional_control = TaskSignatureV2::new("inspect state");
    optional_control.active_goal = Some("goal\0state".into());
    assert_eq!(
        TaskQuery::new(optional_control),
        Err(RetrievalError::ControlCharacter {
            field: "active_goal"
        })
    );

    let mut expected_control = TaskSignatureV2::new("inspect state");
    expected_control.expected_effect = Some("effect\u{1b}state".into());
    assert_eq!(
        TaskQuery::new(expected_control),
        Err(RetrievalError::ControlCharacter {
            field: "expected_effect"
        })
    );

    let mut set_control = TaskSignatureV2::new("inspect state");
    set_control.entities = vec!["entity\0state".into()];
    assert_eq!(
        TaskQuery::new(set_control),
        Err(RetrievalError::ControlCharacter { field: "entities" })
    );

    let mut resource_control = TaskSignatureV2::new("inspect state");
    resource_control.resources = vec!["resource\u{1b}state".into()];
    assert_eq!(
        TaskQuery::new(resource_control),
        Err(RetrievalError::ControlCharacter { field: "resources" })
    );

    let mut constraint_control = TaskSignatureV2::new("inspect state");
    constraint_control.constraints = vec!["constraint\0state".into()];
    assert_eq!(
        TaskQuery::new(constraint_control),
        Err(RetrievalError::ControlCharacter {
            field: "constraints"
        })
    );

    assert_eq!(
        TaskQuery::new(TaskSignatureV2::new(" \t\n")),
        Err(RetrievalError::EmptyRequest)
    );
    let mut whitespace_optional = TaskSignatureV2::new("inspect state");
    whitespace_optional.active_goal = Some(" \t\n".into());
    assert_eq!(
        TaskQuery::new(whitespace_optional),
        Err(RetrievalError::EmptyComponent {
            field: "active_goal",
            index: None
        })
    );
    let mut whitespace_set = TaskSignatureV2::new("inspect state");
    whitespace_set.resources = vec![" \t\n".into()];
    assert_eq!(
        TaskQuery::new(whitespace_set),
        Err(RetrievalError::EmptyComponent {
            field: "resources",
            index: Some(0)
        })
    );

    let mut whitespace_controls = TaskSignatureV2::new(" inspect\t state\n");
    whitespace_controls.active_goal = Some(" goal\r state ".into());
    whitespace_controls.entities = vec![" entity\tstate ".into()];
    let query = TaskQuery::new(whitespace_controls).expect("whitespace controls collapse");
    assert_eq!(
        query.canonical_text(),
        "inspect state goal state entity state"
    );
}

#[test]
fn all_task_query_and_embedding_bounds_accept_n_and_reject_n_plus_one() {
    let max_request = TaskSignatureV2::new("r".repeat(MAX_REQUEST_BYTES));
    assert!(TaskQuery::new(max_request).is_ok());
    assert!(matches!(
        TaskQuery::new(TaskSignatureV2::new("r".repeat(MAX_REQUEST_BYTES + 1))),
        Err(RetrievalError::ComponentTooLong {
            field: "request",
            actual,
            maximum: MAX_REQUEST_BYTES
        }) if actual == MAX_REQUEST_BYTES + 1
    ));

    let max_component = "c".repeat(MAX_COMPONENT_BYTES);
    let mut components = TaskSignatureV2::new("request");
    components.active_goal = Some(max_component.clone());
    components.expected_effect = Some(max_component.clone());
    components.entities = vec![max_component.clone()];
    components.resources = vec![max_component.clone()];
    components.constraints = vec![max_component.clone()];
    assert!(TaskQuery::new(components).is_ok());
    let mut over_component = TaskSignatureV2::new("request");
    over_component.active_goal = Some("c".repeat(MAX_COMPONENT_BYTES + 1));
    assert!(matches!(
        TaskQuery::new(over_component),
        Err(RetrievalError::ComponentTooLong {
            field: "active_goal",
            actual,
            maximum: MAX_COMPONENT_BYTES
        }) if actual == MAX_COMPONENT_BYTES + 1
    ));

    let max_entries = vec!["entry".to_owned(); MAX_SET_ENTRIES];
    let mut entries = TaskSignatureV2::new("request");
    entries.entities = max_entries;
    assert!(TaskQuery::new(entries).is_ok());
    let mut too_many_entries = TaskSignatureV2::new("request");
    too_many_entries.entities = vec!["entry".to_owned(); MAX_SET_ENTRIES + 1];
    assert!(matches!(
        TaskQuery::new(too_many_entries),
        Err(RetrievalError::TooManyEntries {
            field: "entities",
            actual,
            maximum: MAX_SET_ENTRIES
        }) if actual == MAX_SET_ENTRIES + 1
    ));

    // The values-only canonical representation reaches the exact query ceiling
    // with one request, fifteen full entities, and a 4,080-byte active goal.
    let mut exact_query = TaskSignatureV2::new("r".repeat(MAX_REQUEST_BYTES));
    exact_query.active_goal = Some("g".repeat(4_080));
    exact_query.entities = (0..15)
        .map(|index| {
            let mut value = "e".repeat(MAX_COMPONENT_BYTES - 2);
            value.push_str(&format!("{index:02}"));
            value
        })
        .collect();
    let exact_query = TaskQuery::new(exact_query).expect("canonical query at exact maximum");
    assert_eq!(
        exact_query.canonical_text().len(),
        MAX_CANONICAL_QUERY_BYTES
    );
    let mut over_query = exact_query.signature().clone();
    over_query.active_goal = Some("g".repeat(4_081));
    assert!(matches!(
        TaskQuery::new(over_query),
        Err(RetrievalError::CanonicalQueryTooLong {
            actual,
            maximum: MAX_CANONICAL_QUERY_BYTES
        }) if actual == MAX_CANONICAL_QUERY_BYTES + 1
    ));

    let tokens = (0..MAX_LEXICAL_TOKENS)
        .map(|index| format!("token{index}"))
        .collect::<Vec<_>>()
        .join(" ");
    assert!(TaskQuery::new(TaskSignatureV2::new(tokens)).is_ok());
    let tokens_plus_one = (0..=MAX_LEXICAL_TOKENS)
        .map(|index| format!("token{index}"))
        .collect::<Vec<_>>()
        .join(" ");
    assert!(matches!(
        TaskQuery::new(TaskSignatureV2::new(tokens_plus_one)),
        Err(RetrievalError::TooManyLexicalTokens {
            actual,
            maximum: MAX_LEXICAL_TOKENS
        }) if actual == MAX_LEXICAL_TOKENS + 1
    ));

    assert!(RetrievalDocument::new("d".repeat(MAX_RETRIEVAL_DOCUMENT_BYTES)).is_ok());
    assert!(matches!(
        RetrievalDocument::new("d".repeat(MAX_RETRIEVAL_DOCUMENT_BYTES + 1)),
        Err(RetrievalError::RetrievalDocumentTooLong {
            actual,
            maximum: MAX_RETRIEVAL_DOCUMENT_BYTES
        }) if actual == MAX_RETRIEVAL_DOCUMENT_BYTES + 1
    ));
    assert!(EmbeddingVector::new(vec![1.0; MAX_EMBEDDING_DIMENSION]).is_ok());
    assert!(matches!(
        EmbeddingVector::new(vec![1.0; MAX_EMBEDDING_DIMENSION + 1]),
        Err(RetrievalError::EmbeddingDimensionOutOfRange {
            actual,
            minimum: 1,
            maximum: MAX_EMBEDDING_DIMENSION
        }) if actual == MAX_EMBEDDING_DIMENSION + 1
    ));
    assert!(matches!(
        EmbeddingVector::new(Vec::<f32>::new()),
        Err(RetrievalError::EmbeddingDimensionOutOfRange {
            actual: 0,
            minimum: 1,
            maximum: MAX_EMBEDDING_DIMENSION
        })
    ));
}

#[test]
fn v2_operational_limits_accept_exact_maxima_and_reject_zero_or_n_plus_one_without_clamping() {
    assert_eq!(CandidateCount::new(10_000).expect("N").get(), 10_000);
    assert!(matches!(
        CandidateCount::new(10_001),
        Err(RetrievalError::CandidateCountExceeded {
            actual: 10_001,
            maximum: 10_000
        })
    ));

    assert_eq!(
        ditto_retrieval::ContextResultLimit::new(MAX_CONTEXT_RESULT_LIMIT)
            .expect("maximum context result limit")
            .get(),
        MAX_CONTEXT_RESULT_LIMIT
    );
    assert!(matches!(
        ditto_retrieval::ContextResultLimit::new(0),
        Err(RetrievalError::ResultLimitOutOfRange {
            kind: "context result",
            requested: 0,
            minimum: 1,
            maximum: 256
        })
    ));
    assert!(matches!(
        ditto_retrieval::ContextResultLimit::new(257),
        Err(RetrievalError::ResultLimitOutOfRange {
            kind: "context result",
            requested: 257,
            minimum: 1,
            maximum: 256
        })
    ));

    assert_eq!(
        CapabilityRootLimit::new(256)
            .expect("maximum root limit")
            .get(),
        256
    );
    assert!(matches!(
        CapabilityRootLimit::new(0),
        Err(RetrievalError::ResultLimitOutOfRange {
            kind: "capability root",
            requested: 0,
            minimum: 1,
            maximum: 256
        })
    ));
    assert!(matches!(
        CapabilityRootLimit::new(257),
        Err(RetrievalError::ResultLimitOutOfRange {
            kind: "capability root",
            requested: 257,
            minimum: 1,
            maximum: 256
        })
    ));

    assert_eq!(
        ExecutionEpochLimit::new(512)
            .expect("maximum epoch limit")
            .get(),
        512
    );
    assert!(matches!(
        ExecutionEpochLimit::new(0),
        Err(RetrievalError::ResultLimitOutOfRange {
            kind: "execution epoch",
            requested: 0,
            minimum: 1,
            maximum: 512
        })
    ));
    assert!(matches!(
        ExecutionEpochLimit::new(513),
        Err(RetrievalError::ResultLimitOutOfRange {
            kind: "execution epoch",
            requested: 513,
            minimum: 1,
            maximum: 512
        })
    ));
}

#[test]
fn embedding_validation_rejects_non_finite_zero_and_keeps_cosine_unit_normalized() {
    assert!(matches!(
        EmbeddingVector::new(vec![f32::NAN]),
        Err(RetrievalError::NonFiniteEmbeddingValue { index: 0 })
    ));
    assert!(matches!(
        EmbeddingVector::new(vec![f32::INFINITY]),
        Err(RetrievalError::NonFiniteEmbeddingValue { index: 0 })
    ));
    assert!(matches!(
        EmbeddingVector::new(vec![0.0, 0.0]),
        Err(RetrievalError::ZeroEmbeddingVector)
    ));
    let one = EmbeddingVector::new(vec![1.0, 0.0]).expect("valid vector");
    let diagonal = EmbeddingVector::new(vec![1.0, 1.0]).expect("valid vector");
    assert!((one.norm() - 1.0).abs() < 1e-6);
    assert!(
        (cosine_similarity(&one, &diagonal).expect("same dimension") - 0.70710677).abs() < 1e-6
    );
    assert!(matches!(
        cosine_similarity(
            &one,
            &EmbeddingVector::new(vec![1.0]).expect("valid vector")
        ),
        Err(RetrievalError::EmbeddingDimensionMismatch {
            expected: 2,
            actual: 1
        })
    ));
}

#[test]
fn document_embedding_requires_descriptor_and_dimension_continuity() {
    let provider = FixtureProvider::success();
    let query = TaskQuery::with_provider(TaskSignatureV2::new("query"), Some(&provider))
        .expect("valid query");
    let document = RetrievalDocument::new("document").expect("valid document");
    let vector = query
        .embed_document(&provider, &document)
        .expect("matching output");
    assert_eq!(vector.len(), 2);
    assert_eq!(provider.calls().len(), 2);
    assert_eq!(provider.calls()[1].0, EmbeddingPurpose::Document);

    let output = Embedding::new("other", vec![3.0, 4.0]);
    assert!(matches!(
        query.validate_document_embedding(output),
        Err(RetrievalError::EmbeddingDescriptorMismatch { .. })
    ));
    let wrong_dimension = Embedding::new("fixture-v1", vec![3.0]);
    assert_eq!(
        query.validate_document_embedding(wrong_dimension),
        Err(RetrievalError::EmbeddingDimensionMismatch {
            expected: 2,
            actual: 1
        })
    );
    let lexical = TaskQuery::new(TaskSignatureV2::new("query")).expect("lexical query");
    assert_eq!(
        lexical.validate_document_embedding(Embedding::new("fixture-v1", vec![1.0])),
        Err(RetrievalError::EmbeddingNotConfigured)
    );
}

#[test]
fn descriptor_n_plus_one_is_rejected_without_partial_value() {
    assert!(ditto_retrieval::EmbeddingDescriptor::new("d".repeat(256)).is_ok());
    assert!(matches!(
        ditto_retrieval::EmbeddingDescriptor::new("d".repeat(MAX_EMBEDDING_DESCRIPTOR_BYTES + 1)),
        Err(RetrievalError::EmbeddingDescriptorTooLong {
            actual,
            maximum: MAX_EMBEDDING_DESCRIPTOR_BYTES
        }) if actual == MAX_EMBEDDING_DESCRIPTOR_BYTES + 1
    ));
    assert!(ditto_retrieval::EmbeddingDescriptor::new("").is_err());
}

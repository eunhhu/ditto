use std::{
    collections::BTreeSet,
    sync::{Arc, Barrier},
    thread,
};

use chrono::{Duration, TimeZone, Utc};
use ditto_artifact_read::{ArtifactReadDeriver, capability_schema};
use ditto_capability::{
    CapabilityDeriver, CapabilityKind, CapabilityLifecycle, CapabilityManifest, EffectProfile,
    EffectSpec, ExecutionEpoch, InvocationCompiler, PlacementSpec, PolicySpec, RetrievalSpec,
    RuntimeSpec, RuntimeType, UntrustedToolCall, VerificationSpec,
};
use ditto_policy::{
    ApprovalRequirement, AuthorizationOutcome, CapabilityLease, InvocationAuthorizer, PolicyError,
    ResourceScope, StaticPolicy,
};
use serde_json::json;

fn manifest() -> CapabilityManifest {
    let effect = EffectProfile::read_content();
    CapabilityManifest {
        id: "artifact.read".into(),
        version: "0.1.0".into(),
        namespace: "artifact".into(),
        kind: CapabilityKind::Tool,
        lifecycle: CapabilityLifecycle::Active,
        summary: "Read a bounded range from a content-addressed artifact.".into(),
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
        policy: PolicySpec {
            approval: Some("never".into()),
            secret_handles: vec![],
        },
        verification: VerificationSpec {
            default: Some("content-hash".into()),
        },
    }
}

fn invocation(call_id: &str, length: u64) -> ditto_capability::CanonicalInvocation {
    let manifest = manifest();
    let schema = capability_schema();
    let deriver = ArtifactReadDeriver::default();
    let mut epoch = ExecutionEpoch::new(1);
    epoch
        .page_in_invocable(&manifest, &schema, deriver.revision().clone())
        .expect("page exact revision");
    InvocationCompiler::compile(
        &epoch,
        &manifest,
        &schema,
        UntrustedToolCall::new(
            call_id,
            "artifact.read",
            json!({
                "reference": format!("artifact:sha256:{}", "a".repeat(64)),
                "offset": 0,
                "length": length
            }),
        )
        .expect("call"),
        &deriver,
    )
    .expect("canonical invocation")
}

fn register_lease(
    authorizer: &InvocationAuthorizer,
    id: &str,
    calls: u32,
    approval: ApprovalRequirement,
) {
    let scope = invocation("scope-fixture", 1);
    authorizer
        .register_lease(
            CapabilityLease::new(
                id,
                Utc.with_ymd_and_hms(2030, 1, 1, 0, 0, 0)
                    .single()
                    .expect("time"),
                EffectProfile::read_content(),
                calls,
                BTreeSet::from(["artifact.read".into()]),
                scope
                    .resources()
                    .iter()
                    .cloned()
                    .map(ResourceScope::Exact)
                    .collect(),
                approval,
            )
            .expect("lease"),
        )
        .expect("register lease");
}

#[test]
fn static_artifact_policy_issues_only_a_matching_sealed_permit() {
    let now = Utc
        .with_ymd_and_hms(2029, 1, 1, 0, 0, 0)
        .single()
        .expect("time");
    let canonical = invocation("static", 1);
    let resource = canonical
        .resources()
        .iter()
        .next()
        .expect("resource")
        .clone();
    let authorizer = InvocationAuthorizer::new();
    let outcome = authorizer
        .authorize_static(
            &canonical,
            &StaticPolicy::artifact_read(resource).expect("policy"),
            now,
        )
        .expect("permit");
    let AuthorizationOutcome::Permitted(permit) = outcome else {
        panic!("static artifact policy cannot require approval");
    };
    permit.validate(&canonical, now).expect("matching permit");
    assert_eq!(
        permit.validate(&invocation("other", 1), now),
        Err(PolicyError::PermitInvocationMismatch)
    );
    assert_eq!(
        permit.validate(&canonical, now + Duration::minutes(5)),
        Err(PolicyError::PermitExpired)
    );
}

#[test]
fn failed_authorization_does_not_consume_and_successful_retry_consumes_once() {
    let now = Utc
        .with_ymd_and_hms(2029, 1, 1, 0, 0, 0)
        .single()
        .expect("time");
    let authorizer = InvocationAuthorizer::new();
    let invocation = invocation("retry", 1);
    authorizer
        .register_lease(
            CapabilityLease::new(
                "lease-empty-resource",
                now + Duration::hours(1),
                EffectProfile::read_content(),
                1,
                BTreeSet::from(["artifact.read".into()]),
                vec![],
                ApprovalRequirement::Never,
            )
            .expect("lease"),
        )
        .expect("register");
    assert_eq!(
        authorizer.authorize_with_lease(&invocation, "lease-empty-resource", now),
        Err(PolicyError::MissingResourceScope)
    );
    assert_eq!(
        authorizer
            .remaining_calls("lease-empty-resource")
            .expect("calls"),
        1
    );

    register_lease(&authorizer, "lease-good", 1, ApprovalRequirement::Never);
    let first = authorizer
        .authorize_with_lease(&invocation, "lease-good", now)
        .expect("first permit");
    let retry = authorizer
        .authorize_with_lease(&invocation, "lease-good", now)
        .expect("idempotent retry");
    assert_eq!(first, retry);
    assert_eq!(authorizer.remaining_calls("lease-good").expect("calls"), 0);
}

#[test]
fn invocation_id_digest_conflict_fails_closed() {
    let now = Utc
        .with_ymd_and_hms(2029, 1, 1, 0, 0, 0)
        .single()
        .expect("time");
    let manifest = manifest();
    let schema = capability_schema();
    let deriver = ArtifactReadDeriver::default();
    let mut epoch = ExecutionEpoch::new(1);
    epoch
        .page_in_invocable(&manifest, &schema, deriver.revision().clone())
        .expect("page exact revision");
    let compile = |length| {
        InvocationCompiler::compile(
            &epoch,
            &manifest,
            &schema,
            UntrustedToolCall::new(
                "conflict",
                "artifact.read",
                json!({
                    "reference": format!("artifact:sha256:{}", "a".repeat(64)),
                    "offset": 0,
                    "length": length
                }),
            )
            .expect("call"),
            &deriver,
        )
        .expect("canonical invocation")
    };
    let first = compile(1);
    let conflicting = compile(2);
    assert_eq!(first.invocation_id(), conflicting.invocation_id());
    assert_ne!(first.digest(), conflicting.digest());
    let authorizer = InvocationAuthorizer::new();
    let resource = first.resources().iter().next().expect("resource").clone();
    let policy = StaticPolicy::artifact_read(resource).expect("policy");
    authorizer
        .authorize_static(&first, &policy, now)
        .expect("first permit");
    assert_eq!(
        authorizer.authorize_static(&conflicting, &policy, now),
        Err(PolicyError::InvocationDigestConflict)
    );
}

#[test]
fn approval_required_issues_no_permit_and_consumes_nothing() {
    let now = Utc
        .with_ymd_and_hms(2029, 1, 1, 0, 0, 0)
        .single()
        .expect("time");
    let authorizer = InvocationAuthorizer::new();
    register_lease(
        &authorizer,
        "lease-approval",
        1,
        ApprovalRequirement::Always,
    );
    let invocation = invocation("approval", 1);
    let outcome = authorizer
        .authorize_with_lease(&invocation, "lease-approval", now)
        .expect("approval outcome");
    assert!(matches!(outcome, AuthorizationOutcome::ApprovalRequired(_)));
    assert_eq!(
        authorizer.remaining_calls("lease-approval").expect("calls"),
        1
    );
}

#[test]
fn concurrent_one_call_lease_issues_at_most_one_permit() {
    let now = Utc
        .with_ymd_and_hms(2029, 1, 1, 0, 0, 0)
        .single()
        .expect("time");
    let authorizer = InvocationAuthorizer::new();
    register_lease(&authorizer, "lease-race", 1, ApprovalRequirement::Never);
    let barrier = Arc::new(Barrier::new(3));
    let mut handles = Vec::new();
    for invocation in [invocation("race-a", 1), invocation("race-b", 1)] {
        let authorizer = authorizer.clone();
        let barrier = barrier.clone();
        handles.push(thread::spawn(move || {
            barrier.wait();
            authorizer.authorize_with_lease(&invocation, "lease-race", now)
        }));
    }
    barrier.wait();
    let results = handles
        .into_iter()
        .map(|handle| handle.join().expect("authorization thread"))
        .collect::<Vec<_>>();
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Ok(AuthorizationOutcome::Permitted(_))))
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(PolicyError::CallBudgetExhausted)))
            .count(),
        1
    );
    assert_eq!(authorizer.remaining_calls("lease-race").expect("calls"), 0);
}

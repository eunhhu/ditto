use std::{
    collections::BTreeSet,
    sync::{Arc, Barrier},
    thread,
};

use chrono::{DateTime, Duration, TimeZone, Utc};
use ditto_artifact_read::{ArtifactReadDeriver, capability_schema};
use ditto_capability::{
    CanonicalInvocation, CapabilityDeriver, CapabilityKind, CapabilityLifecycle,
    CapabilityManifest, EffectProfile, EffectSpec, InvocationCompiler, LiveExecutionEpoch,
    PlacementSpec, PolicySpec, RetrievalSpec, RuntimeSpec, RuntimeType, UntrustedToolCall,
    VerificationSpec,
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

struct Fixture {
    epoch: LiveExecutionEpoch,
    deriver: ArtifactReadDeriver,
}

impl Fixture {
    fn new() -> Self {
        let manifest = manifest();
        let schema = capability_schema();
        let deriver = ArtifactReadDeriver::default();
        let mut epoch = LiveExecutionEpoch::new(1);
        epoch
            .page_in_invocable(&manifest, &schema, deriver.revision().clone())
            .expect("page exact revision");
        Self { epoch, deriver }
    }

    fn invocation(&self, call_id: &str, length: u64) -> CanonicalInvocation {
        InvocationCompiler::compile(
            self.epoch
                .invocable_binding("artifact.read")
                .expect("live binding"),
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
            &self.deriver,
        )
        .expect("canonical invocation")
    }
}

fn time() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2029, 1, 1, 0, 0, 0)
        .single()
        .expect("time")
}

fn authorizer(fixture: &mut Fixture, expires_at: DateTime<Utc>) -> InvocationAuthorizer {
    let ticket = fixture
        .epoch
        .seal_for_authorization()
        .expect("epoch authorization ticket");
    InvocationAuthorizer::from_ticket(ticket, expires_at).expect("epoch authorizer")
}

fn register_lease(
    authorizer: &InvocationAuthorizer,
    scope: &CanonicalInvocation,
    id: &str,
    calls: u32,
    approval: ApprovalRequirement,
) {
    authorizer
        .register_lease(
            CapabilityLease::new(
                id,
                time() + Duration::hours(1),
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
fn static_artifact_policy_issues_only_a_matching_epoch_bounded_permit() {
    let now = time();
    let mut fixture = Fixture::new();
    let canonical = fixture.invocation("static", 1);
    let resource = canonical
        .resources()
        .iter()
        .next()
        .expect("resource")
        .clone();
    let epoch_expiry = now + Duration::minutes(1);
    let authorizer = authorizer(&mut fixture, epoch_expiry);
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
    assert_eq!(permit.epoch_id(), fixture.epoch.id());
    assert_eq!(permit.expires_at(), epoch_expiry);
    permit.validate(&canonical, now).expect("matching permit");
    assert_eq!(
        permit.validate(&fixture.invocation("other", 1), now),
        Err(PolicyError::PermitInvocationMismatch)
    );
    assert_eq!(
        permit.validate(&canonical, epoch_expiry),
        Err(PolicyError::PermitExpired)
    );
}

#[test]
fn failed_authorization_does_not_consume_and_successful_retry_consumes_once() {
    let now = time();
    let mut fixture = Fixture::new();
    let authorizer = authorizer(&mut fixture, now + Duration::hours(1));
    let invocation = fixture.invocation("retry", 1);
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

    register_lease(
        &authorizer,
        &invocation,
        "lease-good",
        1,
        ApprovalRequirement::Never,
    );
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
fn invocation_id_digest_conflict_fails_closed_inside_epoch() {
    let now = time();
    let mut fixture = Fixture::new();
    let first = fixture.invocation("conflict", 1);
    let conflicting = fixture.invocation("conflict", 2);
    assert_eq!(first.invocation_id(), conflicting.invocation_id());
    assert_ne!(first.digest(), conflicting.digest());
    let authorizer = authorizer(&mut fixture, now + Duration::hours(1));
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
    let now = time();
    let mut fixture = Fixture::new();
    let invocation = fixture.invocation("approval", 1);
    let authorizer = authorizer(&mut fixture, now + Duration::hours(1));
    register_lease(
        &authorizer,
        &invocation,
        "lease-approval",
        1,
        ApprovalRequirement::Always,
    );
    let outcome = authorizer
        .authorize_with_lease(&invocation, "lease-approval", now)
        .expect("approval outcome");
    let AuthorizationOutcome::ApprovalRequired(request) = outcome else {
        panic!("approval-required lease cannot issue a permit");
    };
    assert_eq!(request.epoch_id(), fixture.epoch.id());
    assert_eq!(
        authorizer.remaining_calls("lease-approval").expect("calls"),
        1
    );
}

#[test]
fn concurrent_one_call_lease_issues_at_most_one_permit() {
    let now = time();
    let mut fixture = Fixture::new();
    let first = fixture.invocation("race-a", 1);
    let second = fixture.invocation("race-b", 1);
    let authorizer = authorizer(&mut fixture, now + Duration::hours(1));
    register_lease(
        &authorizer,
        &first,
        "lease-race",
        1,
        ApprovalRequirement::Never,
    );
    let barrier = Arc::new(Barrier::new(3));
    let mut results = Vec::new();
    thread::scope(|scope| {
        let mut handles = Vec::new();
        for invocation in [&first, &second] {
            let authorizer = authorizer.clone();
            let barrier = barrier.clone();
            handles.push(scope.spawn(move || {
                barrier.wait();
                authorizer.authorize_with_lease(invocation, "lease-race", now)
            }));
        }
        barrier.wait();
        for handle in handles {
            results.push(handle.join().expect("authorization thread"));
        }
    });
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

#[test]
fn authorizer_rejects_other_or_expired_epochs() {
    let now = time();
    let mut first = Fixture::new();
    let second = Fixture::new();
    let first_invocation = first.invocation("first", 1);
    let second_invocation = second.invocation("second", 1);
    let resource = first_invocation
        .resources()
        .iter()
        .next()
        .expect("resource")
        .clone();
    let policy = StaticPolicy::artifact_read(resource).expect("policy");
    let authorizer = authorizer(&mut first, now + Duration::minutes(1));
    assert_eq!(
        authorizer.authorize_static(&second_invocation, &policy, now),
        Err(PolicyError::EpochMismatch)
    );
    assert_eq!(
        authorizer.authorize_static(&first_invocation, &policy, now + Duration::minutes(1)),
        Err(PolicyError::EpochExpired)
    );
}

#[test]
fn permit_issues_exactly_one_non_cloneable_execution_claim() {
    let now = time();
    let mut fixture = Fixture::new();
    let invocation = fixture.invocation("claim", 1);
    let resource = invocation
        .resources()
        .iter()
        .next()
        .expect("resource")
        .clone();
    let authorizer = authorizer(&mut fixture, now + Duration::minutes(1));
    let AuthorizationOutcome::Permitted(permit) = authorizer
        .authorize_static(
            &invocation,
            &StaticPolicy::artifact_read(resource).expect("policy"),
            now,
        )
        .expect("permit")
    else {
        panic!("static policy must permit");
    };
    let duplicate = permit.clone();
    let alternate_path = authorizer.clone();
    let claim = alternate_path
        .claim_execution(permit, &invocation, now)
        .expect("first claim");
    assert_eq!(claim.epoch_id(), fixture.epoch.id());
    assert_eq!(claim.invocation_digest(), invocation.digest());
    assert_eq!(
        authorizer.claim_execution(duplicate, &invocation, now),
        Err(PolicyError::PermitAlreadyClaimed)
    );
}

#[test]
fn dropping_the_authorizer_does_not_rearm_epoch_ticket_issuance() {
    let now = time();
    let mut fixture = Fixture::new();
    let authorizer = authorizer(&mut fixture, now + Duration::minutes(1));
    drop(authorizer);
    assert!(matches!(
        fixture.epoch.seal_for_authorization(),
        Err(ditto_capability::CapabilityRevisionError::EpochAlreadySealed)
    ));
}

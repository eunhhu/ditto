use chrono::{Duration, Utc};
use ditto_artifact_sort::{self as sort, SortError};
use ditto_capability::{
    CanonicalInvocation, CanonicalResource, CapabilityDeriver, InvocationCompiler,
    LiveExecutionEpoch, UntrustedToolCall,
};
use ditto_model::CancellationToken;
use ditto_policy::{
    ApprovalRequirement, AuthorizationOutcome, CapabilityLease, ExecutionClaim,
    InvocationAuthorizer, ResourceScope,
};
use serde_json::json;

fn authority(input: &[u8], unique: bool) -> (CanonicalInvocation, ExecutionClaim) {
    authority_at(input, unique, Utc::now())
}

fn authority_at(
    input: &[u8],
    unique: bool,
    now: chrono::DateTime<Utc>,
) -> (CanonicalInvocation, ExecutionClaim) {
    let deriver = sort::SortDeriver::default();
    let mut epoch = LiveExecutionEpoch::new(1);
    epoch
        .page_in_invocable(
            &sort::manifest(),
            &sort::schema(),
            deriver.revision().clone(),
        )
        .unwrap();
    let ticket = epoch.seal_for_authorization().unwrap();
    let call = UntrustedToolCall::new(
        "sort-call",
        sort::ID,
        json!({"reference":sort::input_reference(input),"unique":unique}),
    )
    .unwrap();
    let invocation =
        InvocationCompiler::compile(epoch.invocable_binding(sort::ID).unwrap(), call, &deriver)
            .unwrap();
    let expires = now + Duration::seconds(30);
    let authorizer = InvocationAuthorizer::from_ticket(ticket, expires).unwrap();
    authorizer
        .register_lease(
            CapabilityLease::new(
                "test-sort",
                expires,
                sort::effect(),
                1,
                [sort::ID.into()].into_iter().collect(),
                vec![ResourceScope::Exact(
                    CanonicalResource::artifact(sort::input_reference(input)).unwrap(),
                )],
                ApprovalRequirement::Never,
            )
            .unwrap(),
        )
        .unwrap();
    let AuthorizationOutcome::Permitted(permit) = authorizer
        .authorize_with_lease(&invocation, "test-sort", now)
        .unwrap()
    else {
        panic!()
    };
    let claim = authorizer
        .claim_execution(permit.clone(), &invocation, now)
        .unwrap();
    assert!(
        authorizer
            .claim_execution(permit, &invocation, now)
            .is_err()
    );
    assert_eq!(authorizer.remaining_calls("test-sort").unwrap(), 0);
    (invocation, claim)
}

#[tokio::test]
async fn real_sort_handles_empty_unicode_duplicates_and_lf_contract() {
    for (input, expected, unique) in [
        ("", "", false),
        ("\n", "\n", true),
        ("z\na\na", "a\na\nz\n", false),
        ("z\na\na\n", "a\nz\n", true),
        ("한글\nA\n\né\nA\r\n", "\nA\nA\r\né\n한글\n", false),
        (
            "; touch nope\n$(false)\n--help\n",
            "$(false)\n--help\n; touch nope\n",
            false,
        ),
    ] {
        let (invocation, claim) = authority(input.as_bytes(), unique);
        let output = sort::execute(
            invocation,
            claim,
            input.as_bytes(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(output.bytes(), expected.as_bytes());
    }
}

#[tokio::test]
async fn maximum_sized_input_is_processed_with_bounded_output() {
    let input = vec![b'z'; sort::MAX_INPUT_BYTES];
    let (invocation, claim) = authority(&input, false);
    let output = sort::execute(invocation, claim, &input, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(output.bytes().len(), sort::MAX_INPUT_BYTES + 1);
    assert_eq!(&output.bytes()[..sort::MAX_INPUT_BYTES], input);
    assert_eq!(output.input_lines(), 1);
}

#[test]
fn verifier_rejects_lost_added_unsorted_and_invalid_unique_results() {
    for output in [
        b"a\nz\n".as_slice(),
        b"a\na\nx\nz\n",
        b"z\na\na\n",
        b"a\na\nz",
        b"a\na\na\nz\n",
        b"a\na\nz\n\n",
    ] {
        assert_eq!(
            sort::verify_output(b"z\na\na", output.to_vec(), false).unwrap_err(),
            SortError::Verification
        );
    }
    for output in [b"a\na\nz\n".as_slice(), b"a\n", b"a\nx\nz\n"] {
        assert!(sort::verify_output(b"z\na\na", output.to_vec(), true).is_err());
    }
}

#[tokio::test]
async fn claim_epoch_input_and_cancellation_fail_closed() {
    let (expired, expired_claim) = authority_at(b"a", false, Utc::now() - Duration::seconds(60));
    assert_eq!(
        sort::execute(expired, expired_claim, b"a", CancellationToken::new())
            .await
            .unwrap_err(),
        SortError::Authority
    );
    let (first, _) = authority(b"a", false);
    let (_, other_claim) = authority(b"a", false);
    assert_eq!(
        sort::execute(first, other_claim, b"a", CancellationToken::new())
            .await
            .unwrap_err(),
        SortError::Authority
    );
    let (invocation, claim) = authority(b"a", false);
    assert_eq!(
        sort::execute(invocation, claim, b"different", CancellationToken::new())
            .await
            .unwrap_err(),
        SortError::Authority
    );
    let (invocation, claim) = authority(b"a", false);
    assert!(claim.validate(&invocation, claim.expires_at()).is_err());
    assert!(
        claim
            .validate(&invocation, claim.claimed_at() - Duration::milliseconds(1))
            .is_err()
    );
    let token = CancellationToken::new();
    token.cancel();
    assert_eq!(
        sort::execute(invocation, claim, b"a", token)
            .await
            .unwrap_err(),
        SortError::Cancelled
    );
}

#[test]
fn input_manifest_and_raw_authority_are_closed() {
    assert!(sort::validate_input(&vec![b'a'; sort::MAX_INPUT_BYTES + 1]).is_err());
    assert!(sort::validate_input(&vec![b'\n'; sort::MAX_LINES + 1]).is_err());
    assert!(sort::validate_input(b"a\0b").is_err());
    assert!(sort::validate_input(&[255]).is_err());
    let mut manifest = sort::manifest();
    manifest.runtime.command = Some("/bin/sh".into());
    assert!(sort::validate_manifest(&manifest).is_err());
    let deriver = sort::SortDeriver::default();
    let mut epoch = LiveExecutionEpoch::new(1);
    epoch
        .page_in_invocable(
            &sort::manifest(),
            &sort::schema(),
            deriver.revision().clone(),
        )
        .unwrap();
    for (field, value) in [
        ("program", json!("/bin/sh")),
        ("lease_id", json!("mine")),
        ("effect", json!({})),
        ("environment", json!({"A":"B"})),
        ("cwd", json!("/tmp")),
    ] {
        let mut arguments = json!({"reference":sort::input_reference(b"a"),"unique":false});
        arguments[field] = value;
        let call = UntrustedToolCall::new("bad", sort::ID, arguments).unwrap();
        assert!(
            InvocationCompiler::compile(epoch.invocable_binding(sort::ID).unwrap(), call, &deriver)
                .is_err()
        );
    }
}

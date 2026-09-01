use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};

use chrono::{DateTime, Duration, Utc};
use ditto_capability::{
    CanonicalInvocation, CanonicalPathRoot, CanonicalResource, EffectProfile, InvocationDigest,
    InvocationId,
};
use serde::Serialize;
use thiserror::Error;

const MAX_POLICY_ID_BYTES: usize = 256;
const STATIC_ARTIFACT_POLICY_ID: &str = "builtin.artifact-read.no-approval.v1";
const STATIC_ARTIFACT_PERMIT_SECONDS: i64 = 5 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalRequirement {
    Never,
    Always,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceScope {
    Exact(CanonicalResource),
    PathSubtree(CanonicalPathRoot),
}

impl ResourceScope {
    fn permits(&self, resource: &CanonicalResource) -> bool {
        match (self, resource) {
            (Self::Exact(expected), actual) => expected == actual,
            (Self::PathSubtree(root), CanonicalResource::Path(path)) => root.contains(path),
            (Self::PathSubtree(_), CanonicalResource::Artifact(_)) => false,
        }
    }
}

/// Trusted lease configuration stored only inside the policy authorizer.
/// Invocation inputs never carry or select this value.
#[derive(Debug, Clone)]
pub struct CapabilityLease {
    id: String,
    expires_at: DateTime<Utc>,
    effect_ceiling: EffectProfile,
    remaining_calls: u32,
    capability_ids: BTreeSet<String>,
    resource_scopes: Vec<ResourceScope>,
    approval: ApprovalRequirement,
}

impl CapabilityLease {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: impl Into<String>,
        expires_at: DateTime<Utc>,
        effect_ceiling: EffectProfile,
        remaining_calls: u32,
        capability_ids: BTreeSet<String>,
        resource_scopes: Vec<ResourceScope>,
        approval: ApprovalRequirement,
    ) -> Result<Self, PolicyError> {
        let id = validate_policy_id(id.into())?;
        if capability_ids.is_empty() {
            return Err(PolicyError::EmptyCapabilityScope);
        }
        if capability_ids.iter().any(|capability_id| {
            capability_id.is_empty()
                || capability_id.len() > MAX_POLICY_ID_BYTES
                || capability_id.trim() != capability_id
                || capability_id.chars().any(char::is_control)
        }) {
            return Err(PolicyError::InvalidConfiguration);
        }
        Ok(Self {
            id,
            expires_at,
            effect_ceiling,
            remaining_calls,
            capability_ids,
            resource_scopes,
            approval,
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub const fn remaining_calls(&self) -> u32 {
        self.remaining_calls
    }
}

/// Trusted static policy selected after the kernel establishes exact resource
/// scope. It is not deserializable model input.
#[derive(Debug, Clone)]
pub struct StaticPolicy {
    id: String,
    capability_id: String,
    effect_ceiling: EffectProfile,
    resources: BTreeSet<CanonicalResource>,
    permit_ttl: Duration,
}

impl StaticPolicy {
    /// Exact no-approval policy for a same-scope `artifact.read` resource.
    pub fn artifact_read(resource: CanonicalResource) -> Result<Self, PolicyError> {
        Self::artifact_read_scope(Some(resource))
    }

    /// Build the complete static rule from the kernel's source-verified
    /// resource scope. `None` is an explicit fail-closed scope, not ambient
    /// artifact authority.
    pub fn artifact_read_scope(resource: Option<CanonicalResource>) -> Result<Self, PolicyError> {
        if resource
            .as_ref()
            .is_some_and(|resource| resource.as_artifact().is_none())
        {
            return Err(PolicyError::InvalidConfiguration);
        }
        Ok(Self {
            id: STATIC_ARTIFACT_POLICY_ID.into(),
            capability_id: "artifact.read".into(),
            effect_ceiling: EffectProfile::read_content(),
            resources: resource.into_iter().collect(),
            permit_ttl: Duration::seconds(STATIC_ARTIFACT_PERMIT_SECONDS),
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuthorizationSource {
    StaticPolicy { policy_id: String },
    Lease { lease_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PermitId(String);

impl PermitId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Sealed, expiring authorization for exactly one invocation digest.
///
/// This type deliberately has private fields, no public unchecked constructor,
/// and no `Deserialize` implementation.
///
/// ```compile_fail
/// use ditto_policy::InvocationPermit;
/// let _ = InvocationPermit { permit_id: todo!() };
/// ```
///
/// ```compile_fail
/// use ditto_policy::InvocationPermit;
/// fn requires_deserialize<T: serde::de::DeserializeOwned>() {}
/// requires_deserialize::<InvocationPermit>();
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvocationPermit {
    permit_id: PermitId,
    invocation_digest: InvocationDigest,
    authorization_source: AuthorizationSource,
    granted_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}

impl InvocationPermit {
    pub fn permit_id(&self) -> &PermitId {
        &self.permit_id
    }

    pub const fn invocation_digest(&self) -> InvocationDigest {
        self.invocation_digest
    }

    pub fn authorization_source(&self) -> &AuthorizationSource {
        &self.authorization_source
    }

    pub const fn granted_at(&self) -> DateTime<Utc> {
        self.granted_at
    }

    pub const fn expires_at(&self) -> DateTime<Utc> {
        self.expires_at
    }

    pub fn validate(
        &self,
        invocation: &CanonicalInvocation,
        now: DateTime<Utc>,
    ) -> Result<(), PolicyError> {
        if self.invocation_digest != invocation.digest() {
            return Err(PolicyError::PermitInvocationMismatch);
        }
        if now < self.granted_at || now >= self.expires_at {
            return Err(PolicyError::PermitExpired);
        }
        Ok(())
    }
}

/// Sealed evidence that policy requires approval. This is not a permit and no
/// approval fulfillment path exists in Task 005.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRequest {
    request_id: String,
    invocation_digest: InvocationDigest,
    authorization_source: AuthorizationSource,
    requested_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}

impl ApprovalRequest {
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    pub const fn invocation_digest(&self) -> InvocationDigest {
        self.invocation_digest
    }

    pub fn authorization_source(&self) -> &AuthorizationSource {
        &self.authorization_source
    }

    pub const fn requested_at(&self) -> DateTime<Utc> {
        self.requested_at
    }

    pub const fn expires_at(&self) -> DateTime<Utc> {
        self.expires_at
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthorizationOutcome {
    Permitted(InvocationPermit),
    ApprovalRequired(ApprovalRequest),
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum PolicyError {
    #[error("policy authorization state mutex was poisoned")]
    StatePoisoned,
    #[error("policy configuration is invalid")]
    InvalidConfiguration,
    #[error("lease already exists: {lease_id}")]
    DuplicateLease { lease_id: String },
    #[error("lease is unknown: {lease_id}")]
    UnknownLease { lease_id: String },
    #[error("lease expired")]
    Expired,
    #[error("lease call budget exhausted")]
    CallBudgetExhausted,
    #[error("lease has no capability scope")]
    EmptyCapabilityScope,
    #[error("effect exceeds the authorization ceiling")]
    EffectDenied,
    #[error("capability is outside the authorization scope: {capability_id}")]
    CapabilityDenied { capability_id: String },
    #[error("invocation resources have no authorization scope")]
    MissingResourceScope,
    #[error("resource is outside the authorization scope: {resource}")]
    ResourceDenied { resource: String },
    #[error("invocation id is already bound to a different digest")]
    InvocationDigestConflict,
    #[error("permit does not match the canonical invocation")]
    PermitInvocationMismatch,
    #[error("permit is not currently valid")]
    PermitExpired,
}

/// Clone-shared, process-local atomic authorization ledger.
#[derive(Clone, Default)]
pub struct InvocationAuthorizer {
    state: Arc<Mutex<AuthorizationState>>,
}

#[derive(Default)]
struct AuthorizationState {
    bindings: BTreeMap<InvocationId, InvocationDigest>,
    decisions: BTreeMap<InvocationId, AuthorizationOutcome>,
    leases: BTreeMap<String, CapabilityLease>,
}

impl InvocationAuthorizer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_lease(&self, lease: CapabilityLease) -> Result<(), PolicyError> {
        let mut state = self.state.lock().map_err(|_| PolicyError::StatePoisoned)?;
        if state.leases.contains_key(lease.id()) {
            return Err(PolicyError::DuplicateLease {
                lease_id: lease.id().into(),
            });
        }
        state.leases.insert(lease.id.clone(), lease);
        Ok(())
    }

    pub fn remaining_calls(&self, lease_id: &str) -> Result<u32, PolicyError> {
        let state = self.state.lock().map_err(|_| PolicyError::StatePoisoned)?;
        state
            .leases
            .get(lease_id)
            .map(CapabilityLease::remaining_calls)
            .ok_or_else(|| PolicyError::UnknownLease {
                lease_id: lease_id.into(),
            })
    }

    pub fn authorize_static(
        &self,
        invocation: &CanonicalInvocation,
        policy: &StaticPolicy,
        now: DateTime<Utc>,
    ) -> Result<AuthorizationOutcome, PolicyError> {
        let mut state = self.state.lock().map_err(|_| PolicyError::StatePoisoned)?;
        bind_or_conflict(&mut state, invocation)?;
        if let Some(decision) = state.decisions.get(invocation.invocation_id()) {
            return Ok(decision.clone());
        }
        authorize_profile(
            invocation,
            &policy.capability_id,
            policy.effect_ceiling,
            policy.resources.iter().cloned().map(ResourceScope::Exact),
        )?;
        let expires_at = now
            .checked_add_signed(policy.permit_ttl)
            .ok_or(PolicyError::InvalidConfiguration)?;
        let outcome = AuthorizationOutcome::Permitted(permit(
            invocation,
            AuthorizationSource::StaticPolicy {
                policy_id: policy.id.clone(),
            },
            now,
            expires_at,
        ));
        state
            .decisions
            .insert(invocation.invocation_id().clone(), outcome.clone());
        Ok(outcome)
    }

    pub fn authorize_with_lease(
        &self,
        invocation: &CanonicalInvocation,
        lease_id: &str,
        now: DateTime<Utc>,
    ) -> Result<AuthorizationOutcome, PolicyError> {
        let mut state = self.state.lock().map_err(|_| PolicyError::StatePoisoned)?;
        bind_or_conflict(&mut state, invocation)?;
        if let Some(decision) = state.decisions.get(invocation.invocation_id()) {
            return Ok(decision.clone());
        }

        let (source, expires_at, approval) = {
            let lease = state
                .leases
                .get(lease_id)
                .ok_or_else(|| PolicyError::UnknownLease {
                    lease_id: lease_id.into(),
                })?;
            if now >= lease.expires_at {
                return Err(PolicyError::Expired);
            }
            if lease.remaining_calls == 0 {
                return Err(PolicyError::CallBudgetExhausted);
            }
            authorize_profile(
                invocation,
                &lease.capability_ids,
                lease.effect_ceiling,
                lease.resource_scopes.iter().cloned(),
            )?;
            (
                AuthorizationSource::Lease {
                    lease_id: lease.id.clone(),
                },
                lease.expires_at,
                lease.approval,
            )
        };

        if approval == ApprovalRequirement::Always {
            let outcome = AuthorizationOutcome::ApprovalRequired(approval_request(
                invocation, source, now, expires_at,
            ));
            state
                .decisions
                .insert(invocation.invocation_id().clone(), outcome.clone());
            return Ok(outcome);
        }

        let outcome = AuthorizationOutcome::Permitted(permit(invocation, source, now, expires_at));
        state
            .leases
            .get_mut(lease_id)
            .expect("the checked lease remains present under the authorization mutex")
            .remaining_calls -= 1;
        state
            .decisions
            .insert(invocation.invocation_id().clone(), outcome.clone());
        Ok(outcome)
    }
}

trait CapabilityScope {
    fn permits_capability(&self, capability_id: &str) -> bool;
}

impl CapabilityScope for str {
    fn permits_capability(&self, capability_id: &str) -> bool {
        self == capability_id
    }
}

impl CapabilityScope for String {
    fn permits_capability(&self, capability_id: &str) -> bool {
        self == capability_id
    }
}

impl CapabilityScope for BTreeSet<String> {
    fn permits_capability(&self, capability_id: &str) -> bool {
        self.contains(capability_id)
    }
}

fn authorize_profile(
    invocation: &CanonicalInvocation,
    capability_scope: &impl CapabilityScope,
    effect_ceiling: EffectProfile,
    resource_scopes: impl IntoIterator<Item = ResourceScope>,
) -> Result<(), PolicyError> {
    let capability_id = invocation.capability_revision().capability_id();
    if !capability_scope.permits_capability(capability_id) {
        return Err(PolicyError::CapabilityDenied {
            capability_id: capability_id.into(),
        });
    }
    if !effect_ceiling.permits(invocation.effect()) {
        return Err(PolicyError::EffectDenied);
    }
    let scopes = resource_scopes.into_iter().collect::<Vec<_>>();
    if !invocation.resources().is_empty() && scopes.is_empty() {
        return Err(PolicyError::MissingResourceScope);
    }
    for resource in invocation.resources() {
        if !scopes.iter().any(|scope| scope.permits(resource)) {
            return Err(PolicyError::ResourceDenied {
                resource: resource.to_string(),
            });
        }
    }
    Ok(())
}

fn bind_or_conflict(
    state: &mut AuthorizationState,
    invocation: &CanonicalInvocation,
) -> Result<(), PolicyError> {
    match state.bindings.get(invocation.invocation_id()) {
        Some(digest) if *digest != invocation.digest() => {
            Err(PolicyError::InvocationDigestConflict)
        }
        Some(_) => Ok(()),
        None => {
            state
                .bindings
                .insert(invocation.invocation_id().clone(), invocation.digest());
            Ok(())
        }
    }
}

fn permit(
    invocation: &CanonicalInvocation,
    authorization_source: AuthorizationSource,
    granted_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
) -> InvocationPermit {
    InvocationPermit {
        permit_id: PermitId(format!("permit_{}", invocation.digest())),
        invocation_digest: invocation.digest(),
        authorization_source,
        granted_at,
        expires_at,
    }
}

fn approval_request(
    invocation: &CanonicalInvocation,
    authorization_source: AuthorizationSource,
    requested_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
) -> ApprovalRequest {
    ApprovalRequest {
        request_id: format!("approval_{}", invocation.digest()),
        invocation_digest: invocation.digest(),
        authorization_source,
        requested_at,
        expires_at,
    }
}

fn validate_policy_id(value: String) -> Result<String, PolicyError> {
    if value.is_empty()
        || value.len() > MAX_POLICY_ID_BYTES
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(PolicyError::InvalidConfiguration);
    }
    Ok(value)
}

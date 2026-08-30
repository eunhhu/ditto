use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use ditto_capability::EffectProfile;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Canonical invocation produced after schema validation, argument normalization,
/// resource canonicalization, and capability-specific effect derivation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanonicalInvocation {
    pub lease_id: String,
    pub capability_id: String,
    pub effect: EffectProfile,
    #[serde(default)]
    pub device_id: Option<String>,
    #[serde(default)]
    pub program: Option<String>,
    #[serde(default)]
    pub resources: BTreeSet<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityLease {
    pub id: String,
    pub expires_at: DateTime<Utc>,
    pub effect_ceiling: EffectProfile,
    pub remaining_calls: u32,
    pub capability_ids: BTreeSet<String>,
    #[serde(default)]
    pub device_ids: BTreeSet<String>,
    #[serde(default)]
    pub programs: BTreeSet<String>,
    #[serde(default)]
    pub resources: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseGrant {
    pub lease_id: String,
    pub remaining_calls: u32,
}

#[derive(Debug, Error, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum PolicyError {
    #[error("invocation does not carry the matching opaque lease handle")]
    LeaseMismatch,
    #[error("lease expired")]
    Expired,
    #[error("lease call budget exhausted")]
    CallBudgetExhausted,
    #[error("lease has no capability scope")]
    EmptyCapabilityScope,
    #[error("effect exceeds the lease ceiling")]
    EffectDenied,
    #[error("capability is outside the lease scope: {capability_id}")]
    CapabilityDenied { capability_id: String },
    #[error("required invocation scope is missing: {field}")]
    MissingScope { field: String },
    #[error("device is outside the lease scope: {device_id}")]
    DeviceDenied { device_id: String },
    #[error("program is outside the lease scope: {program}")]
    ProgramDenied { program: String },
    #[error("resource is outside the lease scope: {resource}")]
    ResourceDenied { resource: String },
}

impl CapabilityLease {
    pub fn authorize(
        &mut self,
        invocation: &CanonicalInvocation,
        now: DateTime<Utc>,
    ) -> Result<LeaseGrant, PolicyError> {
        if invocation.lease_id != self.id {
            return Err(PolicyError::LeaseMismatch);
        }
        if now >= self.expires_at {
            return Err(PolicyError::Expired);
        }
        if self.remaining_calls == 0 {
            return Err(PolicyError::CallBudgetExhausted);
        }
        if self.capability_ids.is_empty() {
            return Err(PolicyError::EmptyCapabilityScope);
        }
        if !self.effect_ceiling.permits(invocation.effect) {
            return Err(PolicyError::EffectDenied);
        }
        if !allows_exact(&self.capability_ids, &invocation.capability_id) {
            return Err(PolicyError::CapabilityDenied {
                capability_id: invocation.capability_id.clone(),
            });
        }

        require_optional_scope(
            "device_id",
            &self.device_ids,
            invocation.device_id.as_deref(),
            |device_id| PolicyError::DeviceDenied {
                device_id: device_id.to_owned(),
            },
        )?;
        require_optional_scope(
            "program",
            &self.programs,
            invocation.program.as_deref(),
            |program| PolicyError::ProgramDenied {
                program: program.to_owned(),
            },
        )?;

        if !self.resources.is_empty() {
            if invocation.resources.is_empty() {
                return Err(PolicyError::MissingScope {
                    field: "resources".into(),
                });
            }
            for resource in &invocation.resources {
                if !self
                    .resources
                    .iter()
                    .any(|scope| resource_matches(scope, resource))
                {
                    return Err(PolicyError::ResourceDenied {
                        resource: resource.clone(),
                    });
                }
            }
        }

        self.remaining_calls -= 1;
        Ok(LeaseGrant {
            lease_id: self.id.clone(),
            remaining_calls: self.remaining_calls,
        })
    }
}

fn require_optional_scope(
    field: &str,
    allowed: &BTreeSet<String>,
    actual: Option<&str>,
    denied: impl FnOnce(&str) -> PolicyError,
) -> Result<(), PolicyError> {
    if allowed.is_empty() {
        return Ok(());
    }
    let actual = actual.ok_or_else(|| PolicyError::MissingScope {
        field: field.to_owned(),
    })?;
    if allows_exact(allowed, actual) {
        Ok(())
    } else {
        Err(denied(actual))
    }
}

fn allows_exact(scope: &BTreeSet<String>, value: &str) -> bool {
    scope.contains("*") || scope.contains(value)
}

fn resource_matches(scope: &str, resource: &str) -> bool {
    if scope == "*" {
        return true;
    }
    if let Some(base) = scope.strip_suffix("/**") {
        return resource == base
            || resource
                .strip_prefix(base)
                .is_some_and(|remainder| remainder.starts_with('/'));
    }
    resource == scope
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use chrono::{Duration, Utc};
    use ditto_capability::{DataAccess, EffectProfile, Mutation, Privilege};

    use super::{CanonicalInvocation, CapabilityLease, PolicyError};

    fn lease(now: chrono::DateTime<Utc>) -> CapabilityLease {
        CapabilityLease {
            id: "lease-1".into(),
            expires_at: now + Duration::minutes(15),
            effect_ceiling: EffectProfile {
                access: DataAccess::Content,
                mutation: Mutation::None,
                privilege: Privilege::Elevated,
                ..EffectProfile::default()
            },
            remaining_calls: 2,
            capability_ids: BTreeSet::from(["device.process.run".into()]),
            device_ids: BTreeSet::from(["home-pi".into()]),
            programs: BTreeSet::from(["git".into()]),
            resources: BTreeSet::from(["path:/srv/ditto/**".into()]),
        }
    }

    fn invocation() -> CanonicalInvocation {
        CanonicalInvocation {
            lease_id: "lease-1".into(),
            capability_id: "device.process.run".into(),
            effect: EffectProfile {
                access: DataAccess::Content,
                privilege: Privilege::Elevated,
                ..EffectProfile::default()
            },
            device_id: Some("home-pi".into()),
            program: Some("git".into()),
            resources: BTreeSet::from(["path:/srv/ditto/.git".into()]),
        }
    }

    #[test]
    fn privilege_does_not_authorize_irreversible_mutation() {
        let now = Utc::now();
        let mut invocation = invocation();
        invocation.effect.mutation = Mutation::Irreversible;
        let error = lease(now)
            .authorize(&invocation, now)
            .expect_err("deny orthogonal mutation");
        assert_eq!(error, PolicyError::EffectDenied);
    }

    #[test]
    fn missing_scoped_fields_cannot_bypass_a_lease() {
        let now = Utc::now();
        let mut invocation = invocation();
        invocation.device_id = None;
        let error = lease(now)
            .authorize(&invocation, now)
            .expect_err("missing device must be denied");
        assert_eq!(
            error,
            PolicyError::MissingScope {
                field: "device_id".into()
            }
        );
    }

    #[test]
    fn approved_call_consumes_only_after_all_checks_pass() {
        let now = Utc::now();
        let mut lease = lease(now);
        let grant = lease
            .authorize(&invocation(), now)
            .expect("authorize invocation");
        assert_eq!(grant.remaining_calls, 1);
        assert_eq!(lease.remaining_calls, 1);
    }

    #[test]
    fn path_prefix_does_not_match_a_sibling_path() {
        let now = Utc::now();
        let mut invocation = invocation();
        invocation.resources = BTreeSet::from(["path:/srv/ditto-secrets/key".into()]);
        let error = lease(now)
            .authorize(&invocation, now)
            .expect_err("deny sibling path");
        assert!(matches!(error, PolicyError::ResourceDenied { .. }));
    }
}

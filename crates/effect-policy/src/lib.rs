//! Effect firewall with bounded, opaque capability leases.

use ditto_capability_runtime::InvocationEnvelope;
use ditto_protocol::{EffectClass, new_id};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CapabilityLease {
    pub id: String,
    pub device_ids: Vec<String>,
    pub resource_prefixes: Vec<String>,
    pub allowed_programs: Vec<String>,
    pub effect_ceiling: EffectClass,
    pub max_calls: u32,
    pub calls_used: u32,
    pub expires_at_ms: i64,
}

impl CapabilityLease {
    pub fn bounded(
        device_ids: Vec<String>,
        resource_prefixes: Vec<String>,
        allowed_programs: Vec<String>,
        effect_ceiling: EffectClass,
        max_calls: u32,
        expires_at_ms: i64,
    ) -> Self {
        Self {
            id: new_id("lease"),
            device_ids,
            resource_prefixes,
            allowed_programs,
            effect_ceiling,
            max_calls,
            calls_used: 0,
            expires_at_ms,
        }
    }

    /// Consumes one lease call after every device, effect, program, and resource check passes.
    ///
    /// # Errors
    ///
    /// Returns [`PolicyError`] when any lease boundary is exceeded or mismatched.
    pub fn authorize(
        &mut self,
        envelope: &InvocationEnvelope,
        program: &str,
        now_ms: i64,
    ) -> Result<LeaseGrant, PolicyError> {
        if now_ms >= self.expires_at_ms {
            return Err(PolicyError::Expired);
        }
        if self.calls_used >= self.max_calls {
            return Err(PolicyError::CallBudgetExhausted);
        }
        if !self.device_ids.contains(&envelope.device_id) {
            return Err(PolicyError::DeviceDenied(envelope.device_id.clone()));
        }
        if !self.effect_ceiling.permits(envelope.effect_claim.class) {
            return Err(PolicyError::EffectDenied {
                ceiling: self.effect_ceiling,
                claimed: envelope.effect_claim.class,
            });
        }
        if !self
            .allowed_programs
            .iter()
            .any(|allowed| allowed == program)
        {
            return Err(PolicyError::ProgramDenied(program.to_owned()));
        }
        for resource in &envelope.effect_claim.resources {
            if !self
                .resource_prefixes
                .iter()
                .any(|prefix| resource_matches(prefix, resource))
            {
                return Err(PolicyError::ResourceDenied(resource.clone()));
            }
        }
        match &envelope.lease_id {
            Some(id) if id == &self.id => {}
            _ => return Err(PolicyError::LeaseMismatch),
        }

        self.calls_used += 1;
        Ok(LeaseGrant {
            lease_id: self.id.clone(),
            call_number: self.calls_used,
        })
    }
}

fn resource_matches(scope: &str, resource: &str) -> bool {
    if let Some(base) = scope.strip_suffix("/**") {
        return resource == base
            || resource
                .strip_prefix(base)
                .is_some_and(|remainder| remainder.starts_with('/'));
    }
    resource == scope
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeaseGrant {
    pub lease_id: String,
    pub call_number: u32,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum PolicyError {
    #[error("capability lease expired")]
    Expired,
    #[error("capability lease call budget exhausted")]
    CallBudgetExhausted,
    #[error("device denied by lease: {0}")]
    DeviceDenied(String),
    #[error("effect claim {claimed:?} exceeds lease ceiling {ceiling:?}")]
    EffectDenied {
        ceiling: EffectClass,
        claimed: EffectClass,
    },
    #[error("program denied by lease: {0}")]
    ProgramDenied(String),
    #[error("resource denied by lease: {0}")]
    ResourceDenied(String),
    #[error("invocation does not carry matching opaque lease handle")]
    LeaseMismatch,
}

#[cfg(test)]
mod tests {
    use ditto_capability_runtime::{EffectClaim, InvocationEnvelope};
    use serde_json::json;

    use super::*;

    #[test]
    fn privileged_claim_cannot_cross_read_lease() {
        let mut lease = CapabilityLease::bounded(
            vec!["local".to_owned()],
            vec!["device:local".to_owned()],
            vec!["systemctl".to_owned()],
            EffectClass::Read,
            1,
            10_000,
        );
        let mut envelope = InvocationEnvelope::new(
            "device.process.run",
            "local",
            json!({}),
            EffectClaim {
                class: EffectClass::Privileged,
                resources: vec!["device:local".to_owned()],
                expected_effect: "restart service".to_owned(),
            },
        );
        envelope.lease_id = Some(lease.id.clone());

        let error = lease.authorize(&envelope, "systemctl", 1).unwrap_err();
        assert!(matches!(error, PolicyError::EffectDenied { .. }));
    }

    #[test]
    fn approved_call_consumes_bounded_budget() {
        let mut lease = CapabilityLease::bounded(
            vec!["local".to_owned()],
            vec!["device:local".to_owned()],
            vec!["git".to_owned()],
            EffectClass::Read,
            1,
            10_000,
        );
        let mut envelope = InvocationEnvelope::new(
            "device.process.run",
            "local",
            json!({}),
            ditto_capability_runtime::EffectClaim {
                class: EffectClass::Read,
                resources: vec!["device:local".to_owned()],
                expected_effect: "inspect".to_owned(),
            },
        );
        envelope.lease_id = Some(lease.id.clone());
        lease.authorize(&envelope, "git", 1).unwrap();
        let error = lease.authorize(&envelope, "git", 2).unwrap_err();
        assert_eq!(error, PolicyError::CallBudgetExhausted);
    }

    #[test]
    fn path_prefix_does_not_match_sibling_path() {
        assert!(resource_matches("path:/srv/app/**", "path:/srv/app/logs/a"));
        assert!(!resource_matches(
            "path:/srv/app/**",
            "path:/srv/application/secrets"
        ));
    }
}

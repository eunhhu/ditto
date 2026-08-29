use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use ditto_capability::EffectClass;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvocationIntent {
    pub capability_id: String,
    pub effect: EffectClass,
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
    pub effect_ceiling: EffectClass,
    pub remaining_calls: u32,
    #[serde(default)]
    pub capability_ids: BTreeSet<String>,
    #[serde(default)]
    pub device_ids: BTreeSet<String>,
    #[serde(default)]
    pub programs: BTreeSet<String>,
    #[serde(default)]
    pub resources: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum LeaseDecision {
    Allowed { lease_id: String, remaining_calls: u32 },
    Denied { reason: String },
}

impl CapabilityLease {
    pub fn authorize(&mut self, intent: &InvocationIntent, now: DateTime<Utc>) -> LeaseDecision {
        let denial = if now >= self.expires_at {
            Some("lease expired")
        } else if self.remaining_calls == 0 {
            Some("lease call budget exhausted")
        } else if intent.effect.risk_rank() > self.effect_ceiling.risk_rank() {
            Some("effect exceeds lease ceiling")
        } else if !allows(&self.capability_ids, &intent.capability_id) {
            Some("capability is outside lease scope")
        } else if intent
            .device_id
            .as_ref()
            .is_some_and(|device| !allows(&self.device_ids, device))
        {
            Some("device is outside lease scope")
        } else if intent
            .program
            .as_ref()
            .is_some_and(|program| !allows(&self.programs, program))
        {
            Some("program is outside lease scope")
        } else if !intent
            .resources
            .iter()
            .all(|resource| allows(&self.resources, resource))
        {
            Some("resource is outside lease scope")
        } else {
            None
        };

        if let Some(reason) = denial {
            return LeaseDecision::Denied {
                reason: reason.into(),
            };
        }

        self.remaining_calls -= 1;
        LeaseDecision::Allowed {
            lease_id: self.id.clone(),
            remaining_calls: self.remaining_calls,
        }
    }
}

fn allows(scope: &BTreeSet<String>, value: &str) -> bool {
    scope.contains("*") || scope.contains(value)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use chrono::{Duration, Utc};
    use ditto_capability::EffectClass;

    use super::{CapabilityLease, InvocationIntent, LeaseDecision};

    #[test]
    fn lease_is_bounded_by_effect_and_resource() {
        let now = Utc::now();
        let mut lease = CapabilityLease {
            id: "lease-1".into(),
            expires_at: now + Duration::minutes(15),
            effect_ceiling: EffectClass::WriteReversible,
            remaining_calls: 2,
            capability_ids: BTreeSet::from(["device.process.run".into()]),
            device_ids: BTreeSet::from(["home-pi".into()]),
            programs: BTreeSet::from(["git".into()]),
            resources: BTreeSet::from(["workspace:ditto".into()]),
        };

        let allowed = lease.authorize(
            &InvocationIntent {
                capability_id: "device.process.run".into(),
                effect: EffectClass::WriteReversible,
                device_id: Some("home-pi".into()),
                program: Some("git".into()),
                resources: BTreeSet::from(["workspace:ditto".into()]),
            },
            now,
        );
        assert!(matches!(allowed, LeaseDecision::Allowed { .. }));

        let denied = lease.authorize(
            &InvocationIntent {
                capability_id: "device.process.run".into(),
                effect: EffectClass::Privileged,
                device_id: Some("home-pi".into()),
                program: Some("git".into()),
                resources: BTreeSet::from(["workspace:ditto".into()]),
            },
            now,
        );
        assert!(matches!(denied, LeaseDecision::Denied { .. }));
    }
}

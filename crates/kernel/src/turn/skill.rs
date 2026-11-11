//! `skill.read` (ADR 0046): the body of one of this profile's skills, by name.
//!
//! The catalogue hands the model a name and a trigger line; the body arrives only
//! when the model asks for it, which is the same progressive disclosure the
//! capability rule applies to everything else. Installing a skill grants no
//! effect, so this reads content and changes nothing - and replay recomputes it
//! from the same directory the live turn read.
use std::collections::BTreeSet;
use std::path::Path;

use ditto_capability::{
    CanonicalResource, CapabilityDeriver, CapabilityManifest, CapabilitySchema, DataAccess,
    DerivationBudget, DeriverError, DeriverRevision, EffectProfile, Externality, Mutation,
    Privilege, canonical_manifest_digest,
};
use ditto_profile::skill::{self, MAX_BODY_BYTES};

/// The largest body a read returns; the document format owns the number.
pub const MAX_BODY_BYTES_LIMIT: usize = MAX_BODY_BYTES;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const ID: &str = "skill.read";
pub(crate) const VERSION: &str = "0.1.0";
/// Characters a skill name may hold; the same rule the document format uses.
const MAX_NAME_CHARS: usize = 64;
const NAME_PATTERN: &str = "^[a-z0-9][a-z0-9_-]{0,63}$";

pub(crate) fn manifest() -> CapabilityManifest {
    toml::from_str(include_str!(
        "../../../../capabilities/core/skill-read/capability.toml"
    ))
    .expect("the bundled skill.read manifest parses")
}

/// The installed package must be the bundled one.
pub(crate) fn validate_manifest(installed: &CapabilityManifest) -> bool {
    canonical_manifest_digest(installed) == canonical_manifest_digest(&manifest())
}

/// A read of this profile's own document: content, no mutation, local.
pub(crate) fn effect() -> EffectProfile {
    EffectProfile {
        access: DataAccess::Content,
        mutation: Mutation::None,
        externality: Externality::Local,
        privilege: Privilege::User,
    }
}

pub(crate) fn schema() -> CapabilitySchema {
    CapabilitySchema {
        id: ID.into(),
        version: VERSION.into(),
        summary: "Read one of this profile's skills by name. The catalogue lists the names and what each is for; call this before following one.".into(),
        input_schema: json!({
            "type": "object", "additionalProperties": false, "required": ["name"],
            "properties": {
                "name": {
                    "type": "string",
                    "pattern": NAME_PATTERN,
                    "maxLength": MAX_NAME_CHARS
                }
            }
        }),
        output_schema: json!({"type": "object"}),
    }
}

pub(crate) fn deriver_revision() -> DeriverRevision {
    DeriverRevision::new("skill-read-v1").expect("static deriver revision is valid")
}

/// What one call asks for: the name of the skill to read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillAction {
    #[serde(rename = "name")]
    pub name: String,
}

impl SkillAction {
    fn from_arguments(arguments: &Value) -> Option<Self> {
        let name = arguments.get("name")?.as_str()?.to_owned();
        skill::validate_name(&name).ok()?;
        Some(Self { name })
    }
}

/// Characters the catalogue may add to a request. A profile is a person's
/// install, not a marketplace, so this is a high ceiling that keeps a runaway
/// directory from silently costing every turn.
pub(crate) const MAX_CATALOGUE_CHARS: usize = 4_000;

/// The catalogue as the model reads it: one line per skill, ordered by name,
/// so the bytes are stable while the installation does not change (ADR 0046).
///
/// Over the budget, descriptions are dropped first and every skill keeps its
/// name; if the names alone do not fit, the turn is refused rather than shown a
/// catalogue it would have to guess about. `Ok(None)` when the profile carries
/// no skill: an empty catalogue costs nothing.
pub(crate) fn catalogue_segment(
    headers: &[ditto_profile::skill::SkillHeader],
) -> Result<Option<String>, String> {
    if headers.is_empty() {
        return Ok(None);
    }
    const LEAD: &str = "Skills this profile carries. Read one with skill.read before following it:";
    let build = |descriptions: bool| {
        let mut segment = String::from(LEAD);
        for header in headers {
            if descriptions {
                segment.push_str(&format!("\n- {}: {}", header.name, header.description));
            } else {
                segment.push_str(&format!("\n- {}", header.name));
            }
        }
        segment
    };
    let full = build(true);
    if full.chars().count() <= MAX_CATALOGUE_CHARS {
        return Ok(Some(full));
    }
    let names = build(false);
    if names.chars().count() <= MAX_CATALOGUE_CHARS {
        return Ok(Some(names));
    }
    Err(format!(
        "the skill catalogue is {} characters as names alone, over the {MAX_CATALOGUE_CHARS} limit; remove skills",
        names.chars().count()
    ))
}

/// The arguments of a model call, typed, or `None` when they break the rule.
pub(crate) fn normalize_call(arguments: &serde_json::Value) -> Option<SkillAction> {
    SkillAction::from_arguments(arguments)
}

/// The deterministic deriver: one argument, one read, no resource.
#[derive(Debug, Clone)]
pub(crate) struct SkillDeriver(DeriverRevision);

impl Default for SkillDeriver {
    fn default() -> Self {
        Self(deriver_revision())
    }
}

impl CapabilityDeriver for SkillDeriver {
    fn capability_id(&self) -> &str {
        ID
    }

    fn revision(&self) -> &DeriverRevision {
        &self.0
    }

    fn normalize(
        &self,
        arguments: &Value,
        budget: &mut DerivationBudget,
    ) -> Result<Value, DeriverError> {
        budget.charge(1)?;
        SkillAction::from_arguments(arguments)
            .map(|action| json!(action))
            .ok_or_else(|| DeriverError::new("skill.read call is incomplete"))
    }

    fn derive_effect(
        &self,
        _normalized_arguments: &Value,
        budget: &mut DerivationBudget,
    ) -> Result<EffectProfile, DeriverError> {
        budget.charge(1)?;
        Ok(effect())
    }

    fn derive_resources(
        &self,
        _normalized_arguments: &Value,
        budget: &mut DerivationBudget,
    ) -> Result<BTreeSet<CanonicalResource>, DeriverError> {
        budget.charge(1)?;
        Ok(BTreeSet::new())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillRefusal {
    /// The name is valid but this profile carries no such skill.
    UnknownSkill,
    /// The arguments did not name a readable skill.
    InvalidArguments,
}

/// The tool's wire shape and the model's view of it are the same value: replay
/// rebuilds the conversation from the journal, so two spellings of one result
/// would make every recorded turn disagree with itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum SkillResult {
    Read { name: String, body: String },
    Refused { error: SkillRefusal },
}

impl SkillResult {
    /// Read one body from the profile's skills directory.
    ///
    /// A body at the size limit is refused rather than truncated: half a
    /// procedure looks like a whole one, and the model would follow it.
    pub(crate) fn read(root: &Path, action: &SkillAction) -> Self {
        match skill::read(root, &action.name) {
            Ok(read) if read.body.len() <= MAX_BODY_BYTES => Self::Read {
                name: read.header.name,
                body: read.body,
            },
            Ok(_) | Err(_) => Self::Refused {
                error: SkillRefusal::UnknownSkill,
            },
        }
    }

    pub(crate) fn model_value(&self) -> Value {
        serde_json::to_value(self).expect("a skill result is always serializable")
    }

    pub(crate) fn is_error(&self) -> bool {
        matches!(self, Self::Refused { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_read_returns_the_body_and_an_unknown_name_is_refused_the_same_way_twice() {
        let root = std::env::temp_dir().join(format!(
            "ditto-skill-read-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let directory = root.join("skills").join("photo-triage");
        std::fs::create_dir_all(&directory).expect("dir");
        std::fs::write(
            directory.join("SKILL.md"),
            "---\nname: photo-triage\ndescription: Sort photos.\n---\n# Steps\n",
        )
        .expect("write");

        let found = SkillResult::read(
            &root,
            &SkillAction {
                name: "photo-triage".into(),
            },
        );
        assert_eq!(
            found,
            SkillResult::Read {
                name: "photo-triage".into(),
                body: "# Steps\n".into()
            }
        );
        let missing = SkillResult::read(
            &root,
            &SkillAction {
                name: "ledger-close".into(),
            },
        );
        assert_eq!(
            missing,
            SkillResult::Refused {
                error: SkillRefusal::UnknownSkill
            }
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_catalogue_drops_descriptions_before_it_drops_a_name_and_refuses_last() {
        let header = |name: &str, description: &str| ditto_profile::skill::SkillHeader {
            name: name.to_owned(),
            description: description.to_owned(),
            path: std::path::PathBuf::new(),
        };
        assert_eq!(
            catalogue_segment(&[]).expect("no error"),
            None,
            "an empty catalogue costs nothing"
        );
        let small = catalogue_segment(&[header("photo-triage", "Sort photos by date.")])
            .expect("no error")
            .expect("a segment");
        assert!(small.contains("- photo-triage: Sort photos by date."));

        // 30 skills of 190-character descriptions are over the budget as text,
        // so every name survives and no description does.
        let many = (0..30)
            .map(|index| header(&format!("skill-{index:02}"), &"d".repeat(190)))
            .collect::<Vec<_>>();
        let names_only = catalogue_segment(&many)
            .expect("no error")
            .expect("a segment");
        assert!(
            !names_only.contains("ddd"),
            "descriptions are dropped first"
        );
        assert!(names_only.contains("- skill-29"), "every name survives");

        // 100 names of 64 characters are over the budget as names alone.
        let too_many = (0..100)
            .map(|index| header(&format!("skill-{index:03}{}", "n".repeat(53)), ""))
            .collect::<Vec<_>>();
        assert!(catalogue_segment(&too_many).is_err(), "the turn is refused");
    }

    #[test]
    fn one_name_is_a_call_and_a_path_is_not() {
        assert_eq!(
            normalize_call(&json!({"name": "photo-triage"})),
            Some(SkillAction {
                name: "photo-triage".into()
            })
        );
        for arguments in [
            json!({"name": "../journal"}),
            json!({"name": "Photo-Triage"}),
            json!({"name": ""}),
            json!({}),
        ] {
            assert_eq!(normalize_call(&arguments), None, "{arguments}");
        }
        assert_eq!(effect().mutation, Mutation::None);
    }

    #[test]
    fn the_manifest_is_the_bundled_contract_and_reads_nothing_else() {
        let manifest = manifest();
        assert_eq!(manifest.id, ID);
        assert_eq!(manifest.effects.maximum.mutation, Mutation::None);
        assert_eq!(manifest.effects.maximum.access, DataAccess::Content);
    }
}

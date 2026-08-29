//! Deterministic capability retrieval and execution-epoch paging.

use std::collections::{BTreeMap, BTreeSet};

use ditto_protocol::{CapabilityCard, EffectClass, new_id};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CapabilityManifest {
    pub id: String,
    pub version: String,
    pub namespace: String,
    pub kind: String,
    pub summary: String,
    pub runtime: RuntimeSpec,
    pub placement: PlacementSpec,
    pub retrieval: RetrievalSpec,
    pub effects: EffectsSpec,
    pub verification: VerificationSpec,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RuntimeSpec {
    pub runtime_type: String,
    pub command: String,
    pub lazy: bool,
    pub idle_ttl_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PlacementSpec {
    pub modes: Vec<String>,
    pub requires: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct RetrievalSpec {
    pub aliases: Vec<String>,
    pub intents: Vec<String>,
    pub negative_examples: Vec<String>,
    pub complements: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EffectsSpec {
    pub minimum: EffectClass,
    pub maximum: EffectClass,
    pub resources: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VerificationSpec {
    pub default: String,
}

impl CapabilityManifest {
    pub fn card(&self) -> CapabilityCard {
        CapabilityCard {
            id: self.id.clone(),
            summary: self.summary.clone(),
            namespace: self.namespace.clone(),
            maximum_effect: self.effects.maximum,
            placements: self.placement.modes.clone(),
        }
    }

    pub fn device_process_run() -> Self {
        Self {
            id: "device.process.run".to_owned(),
            version: "1.0.0".to_owned(),
            namespace: "device".to_owned(),
            kind: "tool".to_owned(),
            summary: "Run a structured process on a registered device.".to_owned(),
            runtime: RuntimeSpec {
                runtime_type: "process".to_owned(),
                command: "workers/device-runner".to_owned(),
                lazy: true,
                idle_ttl_ms: 30_000,
            },
            placement: PlacementSpec {
                modes: vec!["local".to_owned(), "ssh".to_owned()],
                requires: vec!["process".to_owned()],
            },
            retrieval: RetrievalSpec {
                aliases: vec![
                    "run command".to_owned(),
                    "start process".to_owned(),
                    "명령 실행".to_owned(),
                    "프로세스 실행".to_owned(),
                    "홈서버 로그".to_owned(),
                ],
                intents: vec![
                    "run a command on another computer".to_owned(),
                    "restart a service on the home server".to_owned(),
                    "inspect remote logs".to_owned(),
                    "다른 컴퓨터에서 명령을 실행".to_owned(),
                    "홈서버 서비스를 재시작".to_owned(),
                    "원격 로그를 확인".to_owned(),
                ],
                negative_examples: vec![
                    "send a message to another person".to_owned(),
                    "open a local file without executing anything".to_owned(),
                ],
                complements: Vec::new(),
            },
            effects: EffectsSpec {
                minimum: EffectClass::Read,
                maximum: EffectClass::Privileged,
                resources: vec!["device:{device_id}".to_owned(), "path:{cwd}/**".to_owned()],
            },
            verification: VerificationSpec {
                default: "exit-code-and-expected-output".to_owned(),
            },
        }
    }
}

#[derive(Clone, Debug)]
pub struct SearchContext {
    pub placement: String,
    pub effect_ceiling: EffectClass,
    pub limit: usize,
}

impl Default for SearchContext {
    fn default() -> Self {
        Self {
            placement: "local".to_owned(),
            effect_ceiling: EffectClass::Read,
            limit: 6,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct CapabilityIndex {
    manifests: BTreeMap<String, CapabilityManifest>,
}

impl CapabilityIndex {
    pub fn new(manifests: impl IntoIterator<Item = CapabilityManifest>) -> Self {
        Self {
            manifests: manifests
                .into_iter()
                .map(|manifest| (manifest.id.clone(), manifest))
                .collect(),
        }
    }

    pub fn len(&self) -> usize {
        self.manifests.len()
    }

    pub fn is_empty(&self) -> bool {
        self.manifests.is_empty()
    }

    pub fn get(&self, id: &str) -> Option<&CapabilityManifest> {
        self.manifests.get(id)
    }

    pub fn search(&self, query: &str, context: &SearchContext) -> Vec<CapabilityManifest> {
        let normalized_query = normalize(query);
        let query_terms = terms(&normalized_query);
        let mut ranked = self
            .manifests
            .values()
            .filter(|manifest| hard_filter(manifest, context))
            .filter_map(|manifest| {
                let score = score(manifest, &normalized_query, &query_terms);
                (score > 0).then_some((score, manifest))
            })
            .collect::<Vec<_>>();
        ranked.sort_by(|(left_score, left), (right_score, right)| {
            right_score
                .cmp(left_score)
                .then_with(|| left.id.cmp(&right.id))
        });

        let mut selected = Vec::new();
        let mut seen = BTreeSet::new();
        for (_, manifest) in ranked {
            if selected.len() >= context.limit {
                break;
            }
            if seen.insert(manifest.id.clone()) {
                selected.push(manifest.clone());
            }
            for complement in &manifest.retrieval.complements {
                if selected.len() >= context.limit {
                    break;
                }
                if let Some(complement) = self.manifests.get(complement)
                    && hard_filter(complement, context)
                    && seen.insert(complement.id.clone())
                {
                    selected.push(complement.clone());
                }
            }
        }
        selected
    }
}

#[derive(Clone, Debug)]
pub struct ExecutionEpoch {
    pub id: String,
    max_size: usize,
    ordered_ids: Vec<String>,
}

impl ExecutionEpoch {
    pub fn new(max_size: usize) -> Self {
        Self {
            id: new_id("epoch"),
            max_size,
            ordered_ids: Vec::new(),
        }
    }

    pub fn page_in(&mut self, manifests: &[CapabilityManifest]) -> Vec<String> {
        let mut added = Vec::new();
        for manifest in manifests {
            if self.ordered_ids.len() >= self.max_size {
                break;
            }
            if !self.ordered_ids.contains(&manifest.id) {
                self.ordered_ids.push(manifest.id.clone());
                added.push(manifest.id.clone());
            }
        }
        added
    }

    pub fn working_set(&self) -> &[String] {
        &self.ordered_ids
    }
}

fn hard_filter(manifest: &CapabilityManifest, context: &SearchContext) -> bool {
    manifest.placement.modes.contains(&context.placement)
        && context.effect_ceiling.permits(manifest.effects.minimum)
}

fn score(manifest: &CapabilityManifest, query: &str, query_terms: &BTreeSet<String>) -> i64 {
    if manifest
        .retrieval
        .negative_examples
        .iter()
        .any(|example| query.contains(&normalize(example)))
    {
        return 0;
    }

    let mut score: i64 = 0;
    let id = normalize(&manifest.id);
    if query == id {
        score += 1_000;
    } else if query.contains(&id) {
        score += 300;
    }

    for alias in &manifest.retrieval.aliases {
        let alias = normalize(alias);
        if query.contains(&alias) {
            score += 150;
        }
    }

    let searchable = format!(
        "{} {} {} {}",
        manifest.id,
        manifest.summary,
        manifest.retrieval.intents.join(" "),
        manifest.retrieval.aliases.join(" ")
    );
    let searchable_terms = terms(&normalize(&searchable));
    let overlap =
        i64::try_from(query_terms.intersection(&searchable_terms).count()).unwrap_or(i64::MAX / 10);
    score.saturating_add(overlap.saturating_mul(10))
}

fn normalize(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_alphanumeric() || character == '.' {
                character.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn terms(value: &str) -> BTreeSet<String> {
    value.split_whitespace().map(str::to_owned).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy(index: usize) -> CapabilityManifest {
        let mut manifest = CapabilityManifest::device_process_run();
        manifest.id = format!("dummy.capability.{index}");
        manifest.summary = format!("Unrelated synthetic capability number {index}");
        manifest.retrieval = RetrievalSpec::default();
        manifest
    }

    #[test]
    fn large_catalogue_pages_only_relevant_working_set() {
        let mut manifests = (0..1_000).map(dummy).collect::<Vec<_>>();
        manifests.push(CapabilityManifest::device_process_run());
        let index = CapabilityIndex::new(manifests);
        let selected = index.search("restart service on home server", &SearchContext::default());
        let mut epoch = ExecutionEpoch::new(6);
        epoch.page_in(&selected);

        assert_eq!(index.len(), 1_001);
        assert_eq!(epoch.working_set(), ["device.process.run"]);
    }

    #[test]
    fn epoch_order_is_append_only_and_deduplicated() {
        let manifest = CapabilityManifest::device_process_run();
        let mut epoch = ExecutionEpoch::new(6);
        epoch.page_in(std::slice::from_ref(&manifest));
        epoch.page_in(std::slice::from_ref(&manifest));
        assert_eq!(epoch.working_set(), ["device.process.run"]);
    }

    #[test]
    fn exact_korean_alias_skips_dense_retrieval() {
        let index = CapabilityIndex::new([CapabilityManifest::device_process_run()]);
        let selected = index.search("홈서버 로그를 확인해줘", &SearchContext::default());
        assert_eq!(selected[0].id, "device.process.run");
    }
}

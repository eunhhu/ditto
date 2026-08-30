use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use ulid::Ulid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EffectClass {
    Pure,
    Read,
    WriteReversible,
    WriteIrreversible,
    ExternalCommunication,
    Privileged,
    CredentialAccess,
}

impl EffectClass {
    pub const fn risk_rank(self) -> u8 {
        match self {
            Self::Pure => 0,
            Self::Read => 1,
            Self::WriteReversible => 2,
            Self::ExternalCommunication => 3,
            Self::WriteIrreversible => 4,
            Self::CredentialAccess => 5,
            Self::Privileged => 6,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CapabilityKind {
    Tool,
    Skill,
    Resource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeType {
    Builtin,
    Process,
    Wasi,
    Mcp,
    Remote,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityManifest {
    pub id: String,
    pub version: String,
    pub namespace: String,
    pub kind: CapabilityKind,
    pub summary: String,
    pub runtime: RuntimeSpec,
    #[serde(default)]
    pub placement: PlacementSpec,
    #[serde(default)]
    pub retrieval: RetrievalSpec,
    pub effects: EffectSpec,
    #[serde(default)]
    pub policy: PolicySpec,
    #[serde(default)]
    pub verification: VerificationSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeSpec {
    #[serde(rename = "type")]
    pub runtime_type: RuntimeType,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default = "default_true")]
    pub lazy: bool,
    #[serde(default = "default_idle_ttl_ms")]
    pub idle_ttl_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlacementSpec {
    #[serde(default)]
    pub modes: Vec<String>,
    #[serde(default)]
    pub requires: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RetrievalSpec {
    #[serde(default)]
    pub intents: Vec<String>,
    #[serde(default)]
    pub negative_examples: Vec<String>,
    #[serde(default)]
    pub complements: Vec<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EffectSpec {
    pub maximum: EffectClass,
    #[serde(default)]
    pub resources: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PolicySpec {
    #[serde(default)]
    pub approval: Option<String>,
    #[serde(default)]
    pub secret_handles: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VerificationSpec {
    #[serde(default)]
    pub default: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityCard {
    pub id: String,
    pub namespace: String,
    pub kind: CapabilityKind,
    pub summary: String,
    pub maximum_effect: EffectClass,
    pub placement_modes: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchContext {
    #[serde(default)]
    pub placement_mode: Option<String>,
    #[serde(default)]
    pub available_requirements: Option<Vec<String>>,
    #[serde(default)]
    pub allowed_effects: Option<Vec<EffectClass>>,
    #[serde(default)]
    pub allowed_capability_ids: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExecutionEpoch {
    pub id: String,
    max_working_set: usize,
    capabilities: Vec<CapabilityCard>,
}

impl ExecutionEpoch {
    pub fn new(max_working_set: usize) -> Self {
        Self {
            id: Ulid::new().to_string(),
            max_working_set,
            capabilities: Vec::new(),
        }
    }

    pub fn page_in(&mut self, cards: impl IntoIterator<Item = CapabilityCard>) -> usize {
        let initial_len = self.capabilities.len();
        for card in cards {
            if self.capabilities.len() >= self.max_working_set {
                break;
            }
            if !self
                .capabilities
                .iter()
                .any(|existing| existing.id == card.id)
            {
                self.capabilities.push(card);
            }
        }
        self.capabilities.len() - initial_len
    }

    pub fn capabilities(&self) -> &[CapabilityCard] {
        &self.capabilities
    }

    pub fn max_working_set(&self) -> usize {
        self.max_working_set
    }
}

impl<'de> Deserialize<'de> for ExecutionEpoch {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct EpochWire {
            id: String,
            max_working_set: usize,
            capabilities: Vec<CapabilityCard>,
        }

        let wire = EpochWire::deserialize(deserializer)?;
        if wire.capabilities.len() > wire.max_working_set {
            return Err(serde::de::Error::custom(
                "execution epoch exceeds its working-set limit",
            ));
        }
        let mut ids = HashSet::new();
        if wire
            .capabilities
            .iter()
            .any(|card| !ids.insert(card.id.as_str()))
        {
            return Err(serde::de::Error::custom(
                "execution epoch contains duplicate capabilities",
            ));
        }
        Ok(Self {
            id: wire.id,
            max_working_set: wire.max_working_set,
            capabilities: wire.capabilities,
        })
    }
}

impl From<&CapabilityManifest> for CapabilityCard {
    fn from(manifest: &CapabilityManifest) -> Self {
        Self {
            id: manifest.id.clone(),
            namespace: manifest.namespace.clone(),
            kind: manifest.kind,
            summary: manifest.summary.clone(),
            maximum_effect: manifest.effects.maximum,
            placement_modes: manifest.placement.modes.clone(),
        }
    }
}

#[derive(Debug, Error)]
pub enum CapabilityError {
    #[error("capability manifest I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse capability manifest {path}: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("duplicate capability id: {0}")]
    DuplicateId(String),
}

#[derive(Debug, Clone, Default)]
pub struct CapabilityCatalog {
    manifests: Vec<CapabilityManifest>,
    positions: HashMap<String, usize>,
}

impl CapabilityCatalog {
    pub fn load(root: impl AsRef<Path>) -> Result<Self, CapabilityError> {
        let root = root.as_ref();
        if !root.exists() {
            return Ok(Self::default());
        }

        let mut paths = Vec::new();
        collect_manifests(root, &mut paths)?;
        paths.sort();

        let mut catalog = Self::default();
        for path in paths {
            let content = fs::read_to_string(&path)?;
            let manifest = toml::from_str::<CapabilityManifest>(&content).map_err(|source| {
                CapabilityError::Parse {
                    path: path.clone(),
                    source,
                }
            })?;
            catalog.insert(manifest)?;
        }
        Ok(catalog)
    }

    pub fn insert(&mut self, manifest: CapabilityManifest) -> Result<(), CapabilityError> {
        if self.positions.contains_key(&manifest.id) {
            return Err(CapabilityError::DuplicateId(manifest.id));
        }
        let index = self.manifests.len();
        self.positions.insert(manifest.id.clone(), index);
        self.manifests.push(manifest);
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.manifests.len()
    }

    pub fn is_empty(&self) -> bool {
        self.manifests.is_empty()
    }

    pub fn get(&self, id: &str) -> Option<&CapabilityManifest> {
        self.positions
            .get(id)
            .and_then(|index| self.manifests.get(*index))
    }

    pub fn cards(&self) -> Vec<CapabilityCard> {
        self.manifests.iter().map(CapabilityCard::from).collect()
    }

    pub fn search(&self, query: &str, limit: usize) -> Vec<CapabilityCard> {
        self.search_with_context(query, &SearchContext::default(), limit)
    }

    pub fn search_with_context(
        &self,
        query: &str,
        context: &SearchContext,
        limit: usize,
    ) -> Vec<CapabilityCard> {
        if limit == 0 {
            return Vec::new();
        }
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return self
                .manifests
                .iter()
                .filter(|manifest| matches_context(manifest, context))
                .take(limit)
                .map(CapabilityCard::from)
                .collect();
        }

        let query_tokens = tokenize(&query);
        let mut scored = self
            .manifests
            .iter()
            .filter(|manifest| matches_context(manifest, context))
            .filter_map(|manifest| {
                let score = lexical_score(manifest, &query, &query_tokens);
                (score > 0.0).then_some((score, manifest))
            })
            .collect::<Vec<_>>();

        scored.sort_by(|(left_score, left), (right_score, right)| {
            right_score
                .total_cmp(left_score)
                .then_with(|| left.id.cmp(&right.id))
        });

        let mut selected = Vec::new();
        for (_, manifest) in scored {
            if selected.len() >= limit {
                break;
            }
            selected.push(manifest);
            for complement_id in &manifest.retrieval.complements {
                if selected.len() >= limit {
                    break;
                }
                let Some(complement) = self.get(complement_id) else {
                    continue;
                };
                if matches_context(complement, context)
                    && !selected
                        .iter()
                        .any(|candidate| candidate.id == complement.id)
                {
                    selected.push(complement);
                }
            }
        }

        selected.into_iter().map(CapabilityCard::from).collect()
    }
}

fn matches_context(manifest: &CapabilityManifest, context: &SearchContext) -> bool {
    if let Some(mode) = &context.placement_mode
        && !manifest.placement.modes.is_empty()
        && !manifest
            .placement
            .modes
            .iter()
            .any(|candidate| candidate == mode)
    {
        return false;
    }

    if let Some(available) = &context.available_requirements
        && !manifest
            .placement
            .requires
            .iter()
            .all(|requirement| available.contains(requirement))
    {
        return false;
    }

    if let Some(allowed) = &context.allowed_effects
        && !allowed.contains(&manifest.effects.maximum)
    {
        return false;
    }

    if let Some(allowed) = &context.allowed_capability_ids
        && !allowed.contains(&manifest.id)
    {
        return false;
    }

    true
}

fn collect_manifests(directory: &Path, output: &mut Vec<PathBuf>) -> Result<(), std::io::Error> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_manifests(&path, output)?;
        } else if path
            .file_name()
            .is_some_and(|name| name == "capability.toml")
        {
            output.push(path);
        }
    }
    Ok(())
}

fn lexical_score(
    manifest: &CapabilityManifest,
    normalized_query: &str,
    query_tokens: &HashSet<String>,
) -> f32 {
    let id = manifest.id.to_lowercase();
    let aliases = manifest.retrieval.aliases.join(" ").to_lowercase();
    let positive = format!(
        "{} {} {} {} {}",
        manifest.id,
        manifest.namespace,
        manifest.summary,
        aliases,
        manifest.retrieval.intents.join(" ")
    )
    .to_lowercase();
    let positive_tokens = tokenize(&positive);

    let mut score = 0.0;
    if id == normalized_query {
        score += 10.0;
    } else if id.contains(normalized_query) || aliases.contains(normalized_query) {
        score += 5.0;
    } else if positive.contains(normalized_query) {
        score += 2.5;
    }

    if !query_tokens.is_empty() {
        let overlap =
            query_tokens.intersection(&positive_tokens).count() as f32 / query_tokens.len() as f32;
        score += overlap * 4.0;
    }

    let negative_penalty = manifest
        .retrieval
        .negative_examples
        .iter()
        .map(|example| {
            let tokens = tokenize(example);
            if query_tokens.is_empty() {
                0.0
            } else {
                query_tokens.intersection(&tokens).count() as f32 / query_tokens.len() as f32
            }
        })
        .fold(0.0_f32, f32::max);

    score - negative_penalty * 3.0
}

fn tokenize(input: &str) -> HashSet<String> {
    input
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| token.len() > 1)
        .map(str::to_lowercase)
        .collect()
}

const fn default_true() -> bool {
    true
}

const fn default_idle_ttl_ms() -> u64 {
    30_000
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::{
        CapabilityCatalog, CapabilityKind, CapabilityManifest, EffectClass, EffectSpec,
        ExecutionEpoch, PlacementSpec, PolicySpec, RetrievalSpec, RuntimeSpec, RuntimeType,
        SearchContext, VerificationSpec,
    };

    const MANIFEST: &str = r#"
id = "device.process.run"
version = "0.1.0"
namespace = "device"
kind = "tool"
summary = "Run a structured process on a registered device."

[runtime]
type = "process"
command = "ditto-device-runner"

[placement]
modes = ["local", "ssh"]
requires = ["process"]

[retrieval]
intents = ["run a command on another computer", "restart a service"]
negative_examples = ["send a message to another person"]
aliases = ["remote command"]

[effects]
maximum = "privileged"
resources = ["device:{device_id}"]

[policy]
approval = "risk-based"

[verification]
default = "exit-code-and-expected-output"
"#;

    #[test]
    fn loads_and_searches_manifests() {
        let directory = tempdir().expect("temporary directory");
        let capability_dir = directory.path().join("device-process-run");
        fs::create_dir_all(&capability_dir).expect("create capability directory");
        fs::write(capability_dir.join("capability.toml"), MANIFEST).expect("write manifest");

        let catalog = CapabilityCatalog::load(directory.path()).expect("load catalog");
        assert_eq!(catalog.len(), 1);

        let result = catalog.search("remote command", 3);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "device.process.run");
    }

    #[test]
    fn filters_before_ranking_and_expands_complements() {
        let mut catalog = CapabilityCatalog::default();
        catalog
            .insert(manifest(
                "device.process.run",
                "restart service",
                &["ssh"],
                &["artifact.read"],
            ))
            .expect("insert process capability");
        catalog
            .insert(manifest(
                "artifact.read",
                "read process output artifact",
                &["local", "ssh"],
                &[],
            ))
            .expect("insert artifact capability");
        catalog
            .insert(manifest(
                "local.service.restart",
                "restart service locally",
                &["local"],
                &[],
            ))
            .expect("insert local capability");

        let results = catalog.search_with_context(
            "restart service",
            &SearchContext {
                placement_mode: Some("ssh".into()),
                ..SearchContext::default()
            },
            3,
        );
        let ids = results
            .iter()
            .map(|card| card.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, ["device.process.run", "artifact.read"]);
    }

    #[test]
    fn bounds_and_stabilizes_a_large_capability_working_set() {
        let mut catalog = CapabilityCatalog::default();
        for index in 0..1_000 {
            catalog
                .insert(manifest(
                    &format!("synthetic.noise.{index:04}"),
                    "unrelated synthetic operation",
                    &["local"],
                    &[],
                ))
                .expect("insert synthetic capability");
        }
        catalog
            .insert(manifest(
                "database.backup",
                "create database backup snapshot",
                &["local"],
                &[],
            ))
            .expect("insert relevant capability");

        let mut epoch = ExecutionEpoch::new(6);
        let first_page = catalog.search_with_context(
            "database backup",
            &SearchContext {
                placement_mode: Some("local".into()),
                allowed_effects: Some(vec![EffectClass::Read]),
                ..SearchContext::default()
            },
            6,
        );
        assert_eq!(first_page.len(), 1);
        assert_eq!(epoch.page_in(first_page), 1);
        let stable_id = epoch.capabilities()[0].id.clone();

        let noise = catalog.search("synthetic operation", 100);
        assert_eq!(epoch.page_in(noise), 5);
        assert_eq!(epoch.capabilities().len(), 6);
        assert_eq!(epoch.capabilities()[0].id, stable_id);
        assert_eq!(epoch.page_in(catalog.search("database backup", 6)), 0);
    }

    #[test]
    fn rejects_invalid_serialized_epochs() {
        let serialized = r#"{
            "id": "epoch-1",
            "max_working_set": 0,
            "capabilities": [{
                "id": "artifact.read",
                "namespace": "artifact",
                "kind": "tool",
                "summary": "read an artifact",
                "maximum_effect": "read",
                "placement_modes": ["local"]
            }]
        }"#;

        assert!(serde_json::from_str::<ExecutionEpoch>(serialized).is_err());
    }

    fn manifest(
        id: &str,
        summary: &str,
        placement_modes: &[&str],
        complements: &[&str],
    ) -> CapabilityManifest {
        CapabilityManifest {
            id: id.into(),
            version: "0.1.0".into(),
            namespace: id.split('.').next().unwrap_or("test").into(),
            kind: CapabilityKind::Tool,
            summary: summary.into(),
            runtime: RuntimeSpec {
                runtime_type: RuntimeType::Builtin,
                command: None,
                lazy: true,
                idle_ttl_ms: 30_000,
            },
            placement: PlacementSpec {
                modes: placement_modes.iter().map(ToString::to_string).collect(),
                requires: Vec::new(),
            },
            retrieval: RetrievalSpec {
                intents: Vec::new(),
                negative_examples: Vec::new(),
                complements: complements.iter().map(ToString::to_string).collect(),
                aliases: Vec::new(),
            },
            effects: EffectSpec {
                maximum: EffectClass::Read,
                resources: Vec::new(),
            },
            policy: PolicySpec::default(),
            verification: VerificationSpec::default(),
        }
    }
}

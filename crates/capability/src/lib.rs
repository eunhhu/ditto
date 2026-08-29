use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

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
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return self.cards().into_iter().take(limit).collect();
        }

        let query_tokens = tokenize(&query);
        let mut scored = self
            .manifests
            .iter()
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

        scored
            .into_iter()
            .take(limit)
            .map(|(_, manifest)| CapabilityCard::from(manifest))
            .collect()
    }
}

fn collect_manifests(directory: &Path, output: &mut Vec<PathBuf>) -> Result<(), std::io::Error> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_manifests(&path, output)?;
        } else if path.file_name().is_some_and(|name| name == "capability.toml") {
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
        let overlap = query_tokens.intersection(&positive_tokens).count() as f32
            / query_tokens.len() as f32;
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

    use super::CapabilityCatalog;

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
}

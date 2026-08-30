use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use ulid::Ulid;

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum DataAccess {
    #[default]
    None,
    Metadata,
    Content,
    Credentials,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum Mutation {
    #[default]
    None,
    Reversible,
    Irreversible,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum Externality {
    #[default]
    Local,
    Network,
    HumanCommunication,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum Privilege {
    #[default]
    User,
    Elevated,
}

/// Orthogonal effect dimensions. Privilege never implies mutation, communication,
/// or credential access.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EffectProfile {
    #[serde(default)]
    pub access: DataAccess,
    #[serde(default)]
    pub mutation: Mutation,
    #[serde(default)]
    pub externality: Externality,
    #[serde(default)]
    pub privilege: Privilege,
}

impl EffectProfile {
    pub const fn permits(self, claimed: Self) -> bool {
        self.access as u8 >= claimed.access as u8
            && self.mutation as u8 >= claimed.mutation as u8
            && self.externality as u8 >= claimed.externality as u8
            && self.privilege as u8 >= claimed.privilege as u8
    }

    pub const fn read_content() -> Self {
        Self {
            access: DataAccess::Content,
            mutation: Mutation::None,
            externality: Externality::Local,
            privilege: Privilege::User,
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
    #[serde(default)]
    pub minimum: EffectProfile,
    pub maximum: EffectProfile,
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
    pub minimum_effect: EffectProfile,
    pub maximum_effect: EffectProfile,
    pub placement_modes: Vec<String>,
}

impl From<&CapabilityManifest> for CapabilityCard {
    fn from(manifest: &CapabilityManifest) -> Self {
        Self {
            id: manifest.id.clone(),
            namespace: manifest.namespace.clone(),
            kind: manifest.kind,
            summary: manifest.summary.clone(),
            minimum_effect: manifest.effects.minimum,
            maximum_effect: manifest.effects.maximum,
            placement_modes: manifest.placement.modes.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    #[default]
    Catalogue,
    Runtime,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchContext {
    #[serde(default)]
    pub mode: SearchMode,
    #[serde(default)]
    pub available_placements: Option<Vec<String>>,
    #[serde(default)]
    pub preferred_placement: Option<String>,
    #[serde(default)]
    pub available_requirements: Option<Vec<String>>,
    #[serde(default)]
    pub effect_ceiling: Option<EffectProfile>,
    #[serde(default)]
    pub allowed_capability_ids: Option<Vec<String>>,
}

impl SearchContext {
    pub fn catalogue() -> Self {
        Self::default()
    }

    pub fn runtime(
        available_placements: Vec<String>,
        available_requirements: Vec<String>,
        effect_ceiling: EffectProfile,
    ) -> Self {
        Self {
            mode: SearchMode::Runtime,
            available_placements: Some(available_placements),
            preferred_placement: None,
            available_requirements: Some(available_requirements),
            effect_ceiling: Some(effect_ceiling),
            allowed_capability_ids: None,
        }
    }
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
        let mut existing = self
            .capabilities
            .iter()
            .map(|card| card.id.clone())
            .collect::<HashSet<_>>();
        for card in cards {
            if self.capabilities.len() >= self.max_working_set {
                break;
            }
            if existing.insert(card.id.clone()) {
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

    pub fn remaining_capacity(&self) -> usize {
        self.max_working_set.saturating_sub(self.capabilities.len())
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
        if wire.id.trim().is_empty() {
            return Err(serde::de::Error::custom("execution epoch id is empty"));
        }
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
    #[error("invalid capability {id}: {reason}")]
    InvalidManifest { id: String, reason: String },
    #[error("capability {id} references unknown complement {complement}")]
    UnknownComplement { id: String, complement: String },
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
        catalog.validate()?;
        Ok(catalog)
    }

    pub fn insert(&mut self, manifest: CapabilityManifest) -> Result<(), CapabilityError> {
        validate_manifest(&manifest)?;
        if self.positions.contains_key(&manifest.id) {
            return Err(CapabilityError::DuplicateId(manifest.id));
        }
        let index = self.manifests.len();
        self.positions.insert(manifest.id.clone(), index);
        self.manifests.push(manifest);
        Ok(())
    }

    pub fn validate(&self) -> Result<(), CapabilityError> {
        for manifest in &self.manifests {
            for complement in &manifest.retrieval.complements {
                if self.get(complement).is_none() {
                    return Err(CapabilityError::UnknownComplement {
                        id: manifest.id.clone(),
                        complement: complement.clone(),
                    });
                }
            }
        }
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
        self.search_with_context(query, &SearchContext::catalogue(), limit)
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
        let query = normalize(query);
        let query_tokens = tokenize(&query);
        let mut ranked = self
            .manifests
            .iter()
            .filter(|manifest| matches_context(manifest, context))
            .filter_map(|manifest| {
                let score = lexical_score(manifest, &query, &query_tokens, context);
                (query.is_empty() || score > 0.0).then_some((score, manifest))
            })
            .collect::<Vec<_>>();

        ranked.sort_by(|(left_score, left), (right_score, right)| {
            right_score
                .total_cmp(left_score)
                .then_with(|| left.id.cmp(&right.id))
        });

        let mut selected = Vec::new();
        let mut seen_ids = HashSet::new();
        for (_, manifest) in ranked {
            if selected.len() >= limit {
                break;
            }
            if seen_ids.insert(manifest.id.as_str()) {
                selected.push(manifest);
            }
            for complement_id in &manifest.retrieval.complements {
                if selected.len() >= limit {
                    break;
                }
                let Some(complement) = self.get(complement_id) else {
                    continue;
                };
                if matches_context(complement, context) && seen_ids.insert(complement.id.as_str()) {
                    selected.push(complement);
                }
            }
        }

        selected.into_iter().map(CapabilityCard::from).collect()
    }
}

fn matches_context(manifest: &CapabilityManifest, context: &SearchContext) -> bool {
    if let Some(allowed) = &context.allowed_capability_ids
        && !allowed.iter().any(|id| id == &manifest.id || id == "*")
    {
        return false;
    }

    if context.mode == SearchMode::Catalogue {
        return true;
    }

    let Some(placements) = &context.available_placements else {
        return false;
    };
    if manifest.placement.modes.is_empty()
        || !manifest
            .placement
            .modes
            .iter()
            .any(|mode| placements.contains(mode))
    {
        return false;
    }

    match &context.available_requirements {
        Some(available) => {
            if !manifest
                .placement
                .requires
                .iter()
                .all(|requirement| available.contains(requirement))
            {
                return false;
            }
        }
        None if !manifest.placement.requires.is_empty() => return false,
        None => {}
    }

    let Some(effect_ceiling) = context.effect_ceiling else {
        return false;
    };
    effect_ceiling.permits(manifest.effects.minimum)
}

fn lexical_score(
    manifest: &CapabilityManifest,
    normalized_query: &str,
    query_tokens: &HashSet<String>,
    context: &SearchContext,
) -> f32 {
    if manifest.retrieval.negative_examples.iter().any(|example| {
        let normalized = normalize(example);
        !normalized.is_empty() && normalized_query.contains(&normalized)
    }) {
        return 0.0;
    }

    let id = normalize(&manifest.id);
    let aliases = normalize(&manifest.retrieval.aliases.join(" "));
    let positive = normalize(&format!(
        "{} {} {} {} {}",
        manifest.id,
        manifest.namespace,
        manifest.summary,
        aliases,
        manifest.retrieval.intents.join(" ")
    ));
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
            let tokens = tokenize(&normalize(example));
            if query_tokens.is_empty() {
                0.0
            } else {
                query_tokens.intersection(&tokens).count() as f32 / query_tokens.len() as f32
            }
        })
        .fold(0.0_f32, f32::max);
    score -= negative_penalty * 3.0;

    if context
        .preferred_placement
        .as_ref()
        .is_some_and(|preferred| manifest.placement.modes.contains(preferred))
    {
        score += 0.25;
    }

    score
}

fn validate_manifest(manifest: &CapabilityManifest) -> Result<(), CapabilityError> {
    macro_rules! invalid {
        ($reason:expr) => {
            CapabilityError::InvalidManifest {
                id: manifest.id.clone(),
                reason: $reason.into(),
            }
        };
    }
    if !valid_capability_id(&manifest.id) {
        return Err(invalid!("id must contain lowercase dotted segments"));
    }
    if manifest.namespace != manifest.id.split('.').next().unwrap_or_default() {
        return Err(invalid!("namespace must match the first id segment"));
    }
    if !valid_semver(&manifest.version) {
        return Err(invalid!("version must be a semantic x.y.z version"));
    }
    if manifest.summary.trim().is_empty() {
        return Err(invalid!("summary is empty"));
    }
    if manifest.placement.modes.is_empty() {
        return Err(invalid!("at least one placement mode is required"));
    }
    if manifest.runtime.runtime_type == RuntimeType::Process
        && manifest
            .runtime
            .command
            .as_ref()
            .is_none_or(|command| command.trim().is_empty())
    {
        return Err(invalid!("process runtime requires a command"));
    }
    if !manifest.effects.maximum.permits(manifest.effects.minimum) {
        return Err(invalid!("minimum effect exceeds maximum effect"));
    }
    for (label, values) in [
        ("alias", &manifest.retrieval.aliases),
        ("intent", &manifest.retrieval.intents),
        ("complement", &manifest.retrieval.complements),
    ] {
        let mut seen = HashSet::new();
        if values.iter().any(|value| !seen.insert(value)) {
            return Err(invalid!(format!("duplicate {label}")));
        }
    }
    if manifest
        .retrieval
        .complements
        .iter()
        .any(|complement| complement == &manifest.id)
    {
        return Err(invalid!("capability cannot complement itself"));
    }
    Ok(())
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

fn valid_capability_id(id: &str) -> bool {
    let mut segments = id.split('.');
    let mut count = 0;
    for segment in &mut segments {
        count += 1;
        let mut chars = segment.chars();
        let Some(first) = chars.next() else {
            return false;
        };
        if !first.is_ascii_lowercase() {
            return false;
        }
        if !chars.all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
        }) {
            return false;
        }
    }
    count >= 2
}

fn valid_semver(version: &str) -> bool {
    let core = version.split_once('-').map_or(version, |(core, _)| core);
    let parts = core.split('.').collect::<Vec<_>>();
    parts.len() == 3
        && parts.iter().all(|part| {
            !part.is_empty() && part.chars().all(|character| character.is_ascii_digit())
        })
}

fn normalize(input: &str) -> String {
    input
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

fn tokenize(input: &str) -> HashSet<String> {
    input
        .split_whitespace()
        .filter(|token| token.chars().count() > 1)
        .map(str::to_owned)
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
        CapabilityCatalog, CapabilityError, CapabilityKind, CapabilityManifest, DataAccess,
        EffectProfile, EffectSpec, ExecutionEpoch, Externality, Mutation, PlacementSpec,
        PolicySpec, Privilege, RetrievalSpec, RuntimeSpec, RuntimeType, SearchContext, SearchMode,
        VerificationSpec,
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

[effects.minimum]
access = "metadata"
mutation = "none"
externality = "local"
privilege = "user"

[effects.maximum]
access = "credentials"
mutation = "irreversible"
externality = "network"
privilege = "elevated"

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
    fn runtime_search_is_fail_closed_and_uses_minimum_effect() {
        let mut catalog = CapabilityCatalog::default();
        catalog
            .insert(manifest(
                "device.process.run",
                "restart service",
                &["local", "ssh"],
                &["process"],
                &[],
                EffectProfile {
                    access: DataAccess::Metadata,
                    ..EffectProfile::default()
                },
                EffectProfile {
                    access: DataAccess::Credentials,
                    mutation: Mutation::Irreversible,
                    externality: Externality::Network,
                    privilege: Privilege::Elevated,
                },
            ))
            .expect("insert capability");

        let missing_runtime_state = SearchContext {
            mode: SearchMode::Runtime,
            ..SearchContext::default()
        };
        assert!(
            catalog
                .search_with_context("restart service", &missing_runtime_state, 3)
                .is_empty()
        );

        let context = SearchContext::runtime(
            vec!["local".into()],
            vec!["process".into()],
            EffectProfile {
                access: DataAccess::Metadata,
                ..EffectProfile::default()
            },
        );
        let result = catalog.search_with_context("restart service", &context, 3);
        assert_eq!(result[0].id, "device.process.run");
    }

    #[test]
    fn independently_places_and_deduplicates_complements() {
        let mut catalog = CapabilityCatalog::default();
        catalog
            .insert(manifest(
                "device.process.run",
                "inspect remote logs",
                &["ssh"],
                &["process"],
                &["artifact.read"],
                EffectProfile {
                    access: DataAccess::Metadata,
                    externality: Externality::Network,
                    ..EffectProfile::default()
                },
                EffectProfile {
                    access: DataAccess::Content,
                    externality: Externality::Network,
                    ..EffectProfile::default()
                },
            ))
            .expect("insert process capability");
        catalog
            .insert(manifest(
                "artifact.read",
                "read remote process output",
                &["local"],
                &[],
                &[],
                EffectProfile::read_content(),
                EffectProfile::read_content(),
            ))
            .expect("insert artifact capability");
        catalog.validate().expect("validate references");

        let mut context = SearchContext::runtime(
            vec!["ssh".into(), "local".into()],
            vec!["process".into()],
            EffectProfile {
                access: DataAccess::Content,
                externality: Externality::Network,
                ..EffectProfile::default()
            },
        );
        context.preferred_placement = Some("ssh".into());
        let results = catalog.search_with_context("inspect remote logs", &context, 3);
        let ids = results
            .iter()
            .map(|card| card.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, ["device.process.run", "artifact.read"]);
    }

    #[test]
    fn rejects_unknown_complements() {
        let mut catalog = CapabilityCatalog::default();
        catalog
            .insert(manifest(
                "device.process.run",
                "run process",
                &["local"],
                &["process"],
                &["device.process.wait"],
                EffectProfile::default(),
                EffectProfile::default(),
            ))
            .expect("insert capability");
        assert!(matches!(
            catalog.validate(),
            Err(CapabilityError::UnknownComplement { .. })
        ));
    }

    #[test]
    fn bounds_and_stabilizes_a_large_capability_working_set() {
        let mut catalog = CapabilityCatalog::default();
        for index in 0..1_000 {
            catalog
                .insert(manifest(
                    &format!("synthetic.noise{index:04}.run"),
                    "unrelated synthetic operation",
                    &["local"],
                    &[],
                    &[],
                    EffectProfile::default(),
                    EffectProfile::default(),
                ))
                .expect("insert synthetic capability");
        }
        catalog
            .insert(manifest(
                "database.backup",
                "create database backup snapshot",
                &["local"],
                &[],
                &[],
                EffectProfile::default(),
                EffectProfile {
                    mutation: Mutation::Reversible,
                    ..EffectProfile::default()
                },
            ))
            .expect("insert relevant capability");

        let mut epoch = ExecutionEpoch::new(6);
        let first_page = catalog.search("database backup", 6);
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
    fn orthogonal_effects_do_not_inherit_from_privilege() {
        let elevated_read = EffectProfile {
            access: DataAccess::Content,
            privilege: Privilege::Elevated,
            ..EffectProfile::default()
        };
        let irreversible_write = EffectProfile {
            mutation: Mutation::Irreversible,
            ..EffectProfile::default()
        };
        assert!(!elevated_read.permits(irreversible_write));
    }

    fn manifest(
        id: &str,
        summary: &str,
        placement_modes: &[&str],
        requirements: &[&str],
        complements: &[&str],
        minimum: EffectProfile,
        maximum: EffectProfile,
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
                requires: requirements.iter().map(ToString::to_string).collect(),
            },
            retrieval: RetrievalSpec {
                intents: Vec::new(),
                negative_examples: Vec::new(),
                complements: complements.iter().map(ToString::to_string).collect(),
                aliases: Vec::new(),
            },
            effects: EffectSpec {
                minimum,
                maximum,
                resources: Vec::new(),
            },
            policy: PolicySpec::default(),
            verification: VerificationSpec::default(),
        }
    }
}

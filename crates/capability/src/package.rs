//! Compact discovery metadata; full manifests are verified only on page-in.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use crate::{
    CapabilityCard, CapabilityError, CapabilityKind, CapabilityLifecycle, CapabilityManifest,
    EffectProfile, PlacementSpec, RetrievalSpec, capability_candidate_bytes, validate_manifest,
};

mod filesystem;

pub const PACKAGE_HEADER_FILENAME: &str = "capability.header.json";
pub const MAX_PACKAGE_HEADER_BYTES: usize = 64 * 1024;
pub const MAX_PACKAGE_MANIFEST_BYTES: usize = 1024 * 1024;
pub const MAX_PACKAGE_STARTUP_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_RETAINED_HEADER_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_PACKAGE_COUNT: usize = 16_384;
pub const MAX_DISCOVERY_ENTRIES: usize = 65_536;
pub const MAX_DISCOVERY_DEPTH: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeaderEffects {
    pub minimum: EffectProfile,
    pub maximum: EffectProfile,
}

/// Discovery data never substitutes for a validated manifest or live binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityHeader {
    pub header_version: u16,
    pub manifest_sha256: String,
    pub id: String,
    pub version: String,
    pub namespace: String,
    pub kind: CapabilityKind,
    pub lifecycle: CapabilityLifecycle,
    pub summary: String,
    pub placement: PlacementSpec,
    pub retrieval: RetrievalSpec,
    pub effects: HeaderEffects,
    /// Preserves the existing V2 full-candidate accounting, without retaining
    /// runtime commands, resource templates, policy, or verification strings.
    pub candidate_bytes: usize,
}

impl CapabilityHeader {
    fn retained_bytes(&self) -> usize {
        let mut bytes = std::mem::size_of::<Self>();
        for value in [
            &self.manifest_sha256,
            &self.id,
            &self.version,
            &self.namespace,
            &self.summary,
        ] {
            bytes = bytes.saturating_add(value.capacity());
        }
        for values in [
            &self.placement.modes,
            &self.placement.requires,
            &self.retrieval.aliases,
            &self.retrieval.intents,
            &self.retrieval.negative_examples,
            &self.retrieval.complements,
        ] {
            bytes = bytes.saturating_add(
                values
                    .capacity()
                    .saturating_mul(std::mem::size_of::<String>()),
            );
            for value in values {
                bytes = bytes.saturating_add(value.capacity());
            }
        }
        bytes
    }
    pub fn from_manifest_bytes(bytes: &[u8]) -> Result<Self, CapabilityError> {
        ensure_limit("manifest bytes", bytes.len(), MAX_PACKAGE_MANIFEST_BYTES)?;
        let manifest = parse_manifest(bytes)?;
        let header = Self::project(&manifest, digest(bytes));
        header.validate()?;
        Ok(header)
    }

    pub fn to_json(&self) -> Result<String, CapabilityError> {
        self.validate()?;
        let value = serde_json::to_string_pretty(self)
            .map_err(|_| invalid("header serialization failed"))?
            + "\n";
        ensure_limit("header bytes", value.len(), MAX_PACKAGE_HEADER_BYTES)?;
        Ok(value)
    }

    pub(super) fn project(manifest: &CapabilityManifest, manifest_sha256: String) -> Self {
        Self {
            header_version: 1,
            manifest_sha256,
            id: manifest.id.clone(),
            version: manifest.version.clone(),
            namespace: manifest.namespace.clone(),
            kind: manifest.kind,
            lifecycle: manifest.lifecycle,
            summary: manifest.summary.clone(),
            placement: manifest.placement.clone(),
            retrieval: manifest.retrieval.clone(),
            effects: HeaderEffects {
                minimum: manifest.effects.minimum,
                maximum: manifest.effects.maximum,
            },
            candidate_bytes: capability_candidate_bytes(manifest),
        }
    }

    fn validate(&self) -> Result<(), CapabilityError> {
        if self.header_version != 1
            || self.manifest_sha256.len() != 64
            || !self
                .manifest_sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || !crate::valid_capability_id(&self.id)
            || self.namespace != self.id.split('.').next().unwrap_or_default()
            || !crate::valid_semver(&self.version)
            || self.summary.trim().is_empty()
            || self.placement.modes.is_empty()
            || !self.effects.maximum.permits(self.effects.minimum)
        {
            return Err(invalid("invalid package header"));
        }
        for values in [
            &self.retrieval.aliases,
            &self.retrieval.intents,
            &self.retrieval.complements,
        ] {
            let mut seen = std::collections::HashSet::new();
            if values.iter().any(|value| !seen.insert(value)) {
                return Err(invalid("duplicate package header retrieval value"));
            }
        }
        if self
            .retrieval
            .complements
            .iter()
            .any(|id| id == &self.id || !crate::valid_capability_id(id))
        {
            return Err(invalid("invalid package header complement"));
        }
        let mut minimum =
            self.id.len() + self.version.len() + self.namespace.len() + self.summary.len();
        for value in self
            .placement
            .modes
            .iter()
            .chain(&self.placement.requires)
            .chain(&self.retrieval.aliases)
            .chain(&self.retrieval.intents)
            .chain(&self.retrieval.negative_examples)
            .chain(&self.retrieval.complements)
        {
            minimum = minimum.saturating_add(value.len());
        }
        if self.candidate_bytes < minimum {
            return Err(invalid("header undercounts candidate bytes"));
        }
        ensure_limit(
            "candidate bytes",
            self.candidate_bytes,
            MAX_PACKAGE_MANIFEST_BYTES,
        )?;
        Ok(())
    }
}

impl From<&CapabilityHeader> for CapabilityCard {
    fn from(header: &CapabilityHeader) -> Self {
        Self {
            id: header.id.clone(),
            namespace: header.namespace.clone(),
            kind: header.kind,
            summary: header.summary.clone(),
            minimum_effect: header.effects.minimum,
            maximum_effect: header.effects.maximum,
            placement_modes: header.placement.modes.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub(super) enum ManifestSource {
    Inline(Arc<CapabilityManifest>),
    File(PathBuf),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CapabilityLoadMetrics {
    pub headers_read: u64,
    pub legacy_manifests_read: u64,
    pub manifests_paged: u64,
    pub startup_bytes: u64,
    pub paged_bytes: u64,
    pub retained_header_bytes: u64,
}

#[derive(Debug, Default)]
pub(super) struct LoadCounters {
    pub startup: CapabilityLoadMetrics,
    pages: AtomicU64,
    bytes: AtomicU64,
}

impl LoadCounters {
    pub fn snapshot(&self) -> CapabilityLoadMetrics {
        CapabilityLoadMetrics {
            manifests_paged: self.pages.load(Ordering::Relaxed),
            paged_bytes: self.bytes.load(Ordering::Relaxed),
            ..self.startup
        }
    }
}

pub(super) fn load(root: &Path) -> Result<crate::CapabilityCatalog, CapabilityError> {
    let paths = filesystem::discover(root)?;
    let mut catalog = crate::CapabilityCatalog::default();
    let mut counters = LoadCounters::default();
    let mut startup_bytes = 0;
    let mut retained_bytes = 0;
    for directory in paths {
        let header_path = directory.join(PACKAGE_HEADER_FILENAME);
        let manifest_path = directory.join("capability.toml");
        let remaining = MAX_PACKAGE_STARTUP_BYTES - startup_bytes;
        let (header, read_bytes) =
            match filesystem::read_optional(&header_path, MAX_PACKAGE_HEADER_BYTES.min(remaining))?
            {
                Some(bytes) => {
                    let header: CapabilityHeader = serde_json::from_slice(&bytes)
                        .map_err(|_| invalid("malformed package header"))?;
                    header.validate()?;
                    counters.startup.headers_read += 1;
                    (header, bytes.len())
                }
                None => {
                    let bytes = filesystem::read_required(
                        &manifest_path,
                        MAX_PACKAGE_MANIFEST_BYTES.min(remaining),
                    )?;
                    let header = CapabilityHeader::from_manifest_bytes(&bytes)?;
                    counters.startup.legacy_manifests_read += 1;
                    (header, bytes.len())
                }
            };
        charge(
            &mut startup_bytes,
            read_bytes,
            MAX_PACKAGE_STARTUP_BYTES,
            "startup bytes",
        )?;
        let header_bytes = header.retained_bytes();
        charge(
            &mut retained_bytes,
            header_bytes
                .saturating_add(manifest_path.capacity())
                .saturating_add(std::mem::size_of::<ManifestSource>())
                .saturating_add(std::mem::size_of::<(String, usize)>())
                .saturating_add(header.id.len()),
            MAX_RETAINED_HEADER_BYTES,
            "retained header bytes",
        )?;
        catalog.insert_header(header, ManifestSource::File(manifest_path))?;
    }
    catalog.validate()?;
    counters.startup.startup_bytes = startup_bytes as u64;
    counters.startup.retained_header_bytes = retained_bytes as u64;
    catalog.load_counters = Arc::new(counters);
    Ok(catalog)
}

pub(super) fn page(
    header: &CapabilityHeader,
    source: &ManifestSource,
    counters: &LoadCounters,
) -> Result<CapabilityManifest, CapabilityError> {
    match source {
        ManifestSource::Inline(manifest) => Ok((**manifest).clone()),
        ManifestSource::File(path) => {
            let bytes = filesystem::read_required(path, MAX_PACKAGE_MANIFEST_BYTES)?;
            for (counter, amount) in [(&counters.pages, 1), (&counters.bytes, bytes.len() as u64)] {
                let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                    Some(v.saturating_add(amount))
                });
            }
            let manifest_digest = digest(&bytes);
            if manifest_digest != header.manifest_sha256 {
                return Err(invalid("package manifest digest mismatch"));
            }
            let manifest = parse_manifest(&bytes)?;
            if CapabilityHeader::project(&manifest, manifest_digest) != *header {
                return Err(invalid("package header projection mismatch"));
            }
            Ok(manifest)
        }
    }
}

/// Generate a header from one bounded, no-follow manifest read. The caller
/// chooses where to write the returned JSON; loading never mutates packages.
pub fn generate_package_header(path: impl AsRef<Path>) -> Result<String, CapabilityError> {
    CapabilityHeader::from_manifest_bytes(&filesystem::read_required(
        path.as_ref(),
        MAX_PACKAGE_MANIFEST_BYTES,
    )?)?
    .to_json()
}

fn parse_manifest(bytes: &[u8]) -> Result<CapabilityManifest, CapabilityError> {
    let text = std::str::from_utf8(bytes).map_err(|_| invalid("manifest is not UTF-8"))?;
    let manifest = toml::from_str(text).map_err(|_| invalid("malformed package manifest"))?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub(super) fn invalid(reason: &'static str) -> CapabilityError {
    CapabilityError::PackageInvalid(reason)
}
pub(super) fn ensure_limit(
    kind: &'static str,
    actual: usize,
    maximum: usize,
) -> Result<(), CapabilityError> {
    if actual > maximum {
        Err(CapabilityError::PackageLimit { kind, maximum })
    } else {
        Ok(())
    }
}
pub(super) fn charge(
    total: &mut usize,
    amount: usize,
    maximum: usize,
    kind: &'static str,
) -> Result<(), CapabilityError> {
    let next = total
        .checked_add(amount)
        .ok_or(CapabilityError::PackageLimit { kind, maximum })?;
    ensure_limit(kind, next, maximum)?;
    *total = next;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_aggregate_envelopes_accept_n_and_leave_counters_unchanged_at_n_plus_one() {
        for (kind, maximum) in [
            ("startup bytes", MAX_PACKAGE_STARTUP_BYTES),
            ("retained header bytes", MAX_RETAINED_HEADER_BYTES),
            ("package count", MAX_PACKAGE_COUNT),
            ("discovery entries", MAX_DISCOVERY_ENTRIES),
        ] {
            let mut used = maximum - 1;
            charge(&mut used, 1, maximum, kind).unwrap();
            assert_eq!(used, maximum);
            assert!(matches!(
                charge(&mut used, 1, maximum, kind),
                Err(CapabilityError::PackageLimit { .. })
            ));
            assert_eq!(used, maximum);
            assert!(charge(&mut used, usize::MAX, maximum, kind).is_err());
            assert_eq!(used, maximum);
        }
    }
}

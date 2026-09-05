use ditto_capability::{
    CapabilityCatalog, CapabilityError, CapabilityHeader, CapabilityManifest, MAX_DISCOVERY_DEPTH,
    MAX_PACKAGE_HEADER_BYTES, MAX_PACKAGE_MANIFEST_BYTES, PACKAGE_HEADER_FILENAME, SearchContext,
};
use ditto_retrieval::{CapabilityRootLimit, ExecutionEpochLimit, TaskQuery, TaskSignatureV2};
use std::{
    fs,
    path::{Path, PathBuf},
};

const MANIFEST: &str = include_str!("../../../capabilities/core/artifact-read/capability.toml");

fn root() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = fs::canonicalize(dir.path()).unwrap();
    (dir, path)
}

fn package(root: &Path, name: &str, body: &str, with_header: bool) -> PathBuf {
    let path = root.join(name);
    fs::create_dir_all(&path).unwrap();
    fs::write(path.join("capability.toml"), body).unwrap();
    if with_header {
        fs::write(
            path.join(PACKAGE_HEADER_FILENAME),
            CapabilityHeader::from_manifest_bytes(body.as_bytes())
                .unwrap()
                .to_json()
                .unwrap(),
        )
        .unwrap();
    }
    path
}

fn cards(catalog: &CapabilityCatalog) -> serde_json::Value {
    let query = TaskQuery::new(TaskSignatureV2::new("read artifact")).unwrap();
    serde_json::json!({
        "legacy": catalog.search("read artifact", 5),
        "v2": catalog.search_task_query(&query, &SearchContext::catalogue(),
            CapabilityRootLimit::new(5).unwrap(), ExecutionEpochLimit::new(5).unwrap(), None).unwrap()
    })
}

#[test]
fn startup_and_search_are_cold_and_selected_bodies_are_not_retained() {
    let (_dir, root) = root();
    let body = MANIFEST.replace("content-hash", &"x".repeat(400_000));
    package(&root, "read", &body, true);
    let catalog = CapabilityCatalog::load(&root).unwrap();
    let mut inline = CapabilityCatalog::default();
    inline
        .insert(toml::from_str::<CapabilityManifest>(&body).unwrap())
        .unwrap();
    let before = catalog.load_metrics();
    assert_eq!(before.headers_read, 1);
    assert_eq!(before.legacy_manifests_read, 0);
    assert_eq!(before.manifests_paged, 0);
    assert!(before.startup_bytes < 4096);
    assert!(before.retained_header_bytes < 4096);
    assert_eq!(cards(&catalog), cards(&inline));
    assert_eq!(catalog.load_metrics(), before);
    assert_eq!(
        catalog
            .page_manifest("artifact.read")
            .unwrap()
            .unwrap()
            .verification
            .default
            .unwrap()
            .len(),
        400_000
    );
    assert_eq!(catalog.load_metrics().manifests_paged, 1);
    assert_eq!(
        catalog.load_metrics().retained_header_bytes,
        before.retained_header_bytes
    );
    assert_eq!(catalog.load_metrics().paged_bytes, body.len() as u64);
    fs::remove_file(root.join("read/capability.toml")).unwrap();
    assert!(
        catalog.page_manifest("artifact.read").is_err(),
        "no stale cache fallback"
    );
}

#[test]
fn legacy_and_header_packages_preserve_search_and_charge_the_same_candidate_work() {
    let (_a, a) = root();
    let (_b, b) = root();
    package(&a, "read", MANIFEST, true);
    package(&b, "read", MANIFEST, false);
    let header = CapabilityCatalog::load(&a).unwrap();
    let legacy = CapabilityCatalog::load(&b).unwrap();
    assert_eq!(cards(&header), cards(&legacy));
    assert_eq!(
        header.header("artifact.read").unwrap(),
        legacy.header("artifact.read").unwrap()
    );
    assert_eq!(legacy.load_metrics().legacy_manifests_read, 1);
    assert_eq!(legacy.load_metrics().headers_read, 0);
    assert_eq!(legacy.load_metrics().startup_bytes, MANIFEST.len() as u64);
    legacy.page_manifest("artifact.read").unwrap().unwrap();
    assert_eq!(legacy.load_metrics().manifests_paged, 1);
    assert_eq!(legacy.load_metrics().legacy_manifests_read, 1);
}

#[test]
fn inactive_packages_do_not_parse_bodies_resolve_complements_or_page() {
    for state in ["quarantined", "retired"] {
        let (_dir, root) = root();
        let body = format!("lifecycle = \"{state}\"\n{MANIFEST}")
            .replace("complements = []", "complements = [\"missing.tool\"]");
        let path = package(&root, "read", &body, true);
        fs::write(path.join("capability.toml"), "not valid TOML").unwrap();
        let catalog = CapabilityCatalog::load(&root).unwrap();
        assert!(catalog.cards().is_empty());
        assert!(catalog.search("read artifact", 5).is_empty());
        assert!(catalog.page_manifest("artifact.read").unwrap().is_none());
        assert_eq!(catalog.load_metrics().legacy_manifests_read, 0);
        assert_eq!(catalog.load_metrics().manifests_paged, 0);
    }
}

#[test]
fn active_unknown_complements_and_duplicate_ids_fail_catalogue_load() {
    let (_dir, root) = root();
    let body = MANIFEST.replace("complements = []", "complements = [\"missing.tool\"]");
    package(&root, "read", &body, true);
    assert!(matches!(
        CapabilityCatalog::load(&root),
        Err(CapabilityError::UnknownComplement { .. })
    ));
    package(&root, "read", MANIFEST, true);
    package(&root, "duplicate", MANIFEST, true);
    assert!(matches!(
        CapabilityCatalog::load(&root),
        Err(CapabilityError::DuplicateId(_))
    ));
}

#[test]
fn body_drift_and_header_contradictions_fail_page_in() {
    let (_dir, root) = root();
    let path = package(&root, "read", MANIFEST, true);
    let catalog = CapabilityCatalog::load(&root).unwrap();
    fs::write(
        path.join("capability.toml"),
        format!("{MANIFEST}\n# changed"),
    )
    .unwrap();
    assert!(matches!(
        catalog.page_manifest("artifact.read"),
        Err(CapabilityError::PackageInvalid(
            "package manifest digest mismatch"
        ))
    ));
    fs::write(path.join("capability.toml"), MANIFEST).unwrap();
    let mut header = CapabilityHeader::from_manifest_bytes(MANIFEST.as_bytes()).unwrap();
    header.summary = "other".into();
    fs::write(
        path.join(PACKAGE_HEADER_FILENAME),
        header.to_json().unwrap(),
    )
    .unwrap();
    let catalog = CapabilityCatalog::load(&root).unwrap();
    assert!(matches!(
        catalog.page_manifest("artifact.read"),
        Err(CapabilityError::PackageInvalid(
            "package header projection mismatch"
        ))
    ));
}

#[test]
fn exact_header_and_manifest_byte_limits_reject_n_plus_one() {
    let (_dir, root) = root();
    let path = package(&root, "read", MANIFEST, true);
    let mut header = fs::read(path.join(PACKAGE_HEADER_FILENAME)).unwrap();
    header.resize(MAX_PACKAGE_HEADER_BYTES, b' ');
    fs::write(path.join(PACKAGE_HEADER_FILENAME), &header).unwrap();
    assert!(CapabilityCatalog::load(&root).is_ok());
    header.push(b' ');
    fs::write(path.join(PACKAGE_HEADER_FILENAME), header).unwrap();
    assert!(matches!(
        CapabilityCatalog::load(&root),
        Err(CapabilityError::PackageLimit { .. })
    ));
    let mut body = MANIFEST.as_bytes().to_vec();
    body.resize(MAX_PACKAGE_MANIFEST_BYTES, b' ');
    fs::write(
        path.join(PACKAGE_HEADER_FILENAME),
        CapabilityHeader::from_manifest_bytes(&body)
            .unwrap()
            .to_json()
            .unwrap(),
    )
    .unwrap();
    fs::write(path.join("capability.toml"), &body).unwrap();
    let catalog = CapabilityCatalog::load(&root).unwrap();
    assert!(catalog.page_manifest("artifact.read").unwrap().is_some());
    body.push(b' ');
    fs::write(path.join("capability.toml"), &body).unwrap();
    assert!(matches!(
        catalog.page_manifest("artifact.read"),
        Err(CapabilityError::PackageLimit { .. })
    ));
    assert!(CapabilityHeader::from_manifest_bytes(&body).is_err());
}

#[test]
fn discovery_depth_is_checked_at_n_and_before_n_plus_one() {
    let (_dir, root) = root();
    let relative: PathBuf = (0..MAX_DISCOVERY_DEPTH).map(|_| "nested").collect();
    let path = package(&root, relative.to_str().unwrap(), MANIFEST, true);
    assert!(CapabilityCatalog::load(&root).is_ok());
    fs::create_dir(path.join("one-more")).unwrap();
    assert!(matches!(
        CapabilityCatalog::load(&root),
        Err(CapabilityError::PackageLimit {
            kind: "discovery depth",
            ..
        })
    ));
}

#[cfg(unix)]
#[test]
fn root_descendant_and_metadata_symlinks_are_rejected_including_later_replacements() {
    use std::os::unix::fs::symlink;
    let (_dir, root) = root();
    let source = package(&root, "packages/read", MANIFEST, true);
    symlink(root.join("packages"), root.join("root-link")).unwrap();
    assert!(CapabilityCatalog::load(root.join("root-link")).is_err());
    symlink(&source, root.join("packages/alias")).unwrap();
    assert!(CapabilityCatalog::load(root.join("packages")).is_err());
    fs::remove_file(root.join("packages/alias")).unwrap();
    let catalog = CapabilityCatalog::load(root.join("packages")).unwrap();
    let outside = root.join("body");
    fs::rename(source.join("capability.toml"), &outside).unwrap();
    symlink(&outside, source.join("capability.toml")).unwrap();
    assert!(catalog.page_manifest("artifact.read").is_err());
    assert!(CapabilityCatalog::load(root.join("packages")).is_err());
    fs::remove_file(source.join("capability.toml")).unwrap();
    fs::rename(outside, source.join("capability.toml")).unwrap();
    fs::rename(&source, root.join("moved")).unwrap();
    symlink(root.join("moved"), &source).unwrap();
    assert!(catalog.page_manifest("artifact.read").is_err());
}

#[test]
fn large_header_catalogue_never_reads_unused_bodies_and_keeps_order() {
    let (_dir, root) = root();
    for i in 0..1_000 {
        let body = MANIFEST.replace("artifact.read", &format!("artifact.read{i:04}"));
        let path = package(&root, &format!("p{i:04}"), &body, true);
        // Headers provide discovery even when unused bodies are unavailable.
        if i != 500 {
            fs::remove_file(path.join("capability.toml")).unwrap();
        }
    }
    let catalog = CapabilityCatalog::load(&root).unwrap();
    assert_eq!(catalog.len(), 1_000);
    assert_eq!(catalog.load_metrics().headers_read, 1_000);
    assert_eq!(catalog.load_metrics().legacy_manifests_read, 0);
    assert_eq!(
        catalog.search("artifact.read0500", 1)[0].id,
        "artifact.read0500"
    );
    catalog.page_manifest("artifact.read0500").unwrap().unwrap();
    assert_eq!(catalog.load_metrics().manifests_paged, 1);
}

#[test]
fn bundled_headers_match_the_generator() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../capabilities/core")
        .canonicalize()
        .unwrap();
    for name in ["artifact-read", "device-process-run"] {
        let path = root.join(name);
        let expected =
            ditto_capability::generate_package_header(path.join("capability.toml")).unwrap();
        assert_eq!(
            fs::read_to_string(path.join(PACKAGE_HEADER_FILENAME)).unwrap(),
            expected
        );
    }
}

#[cfg(unix)]
#[test]
fn non_regular_metadata_is_rejected_without_blocking() {
    use std::os::unix::ffi::OsStrExt;
    let (_dir, root) = root();
    let path = package(&root, "read", MANIFEST, true);
    let catalog = CapabilityCatalog::load(&root).unwrap();
    let body_path = path.join("capability.toml");
    fs::remove_file(&body_path).unwrap();
    let c_path = std::ffi::CString::new(body_path.as_os_str().as_bytes()).unwrap();
    // SAFETY: the NUL-terminated path refers to this test's private fixture.
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
    assert!(catalog.page_manifest("artifact.read").is_err());
    assert!(CapabilityCatalog::load(&root).is_err());
}

#[test]
fn invalid_header_version_digest_accounting_and_unknown_fields_are_rejected() {
    let (_dir, root) = root();
    let path = package(&root, "read", MANIFEST, true);
    let baseline: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(path.join(PACKAGE_HEADER_FILENAME)).unwrap())
            .unwrap();
    for (field, value) in [
        ("header_version", serde_json::json!(2)),
        ("manifest_sha256", serde_json::json!("invalid")),
        ("candidate_bytes", serde_json::json!(0)),
        ("unknown_field", serde_json::json!(true)),
    ] {
        let mut header = baseline.clone();
        header[field] = value;
        fs::write(
            path.join(PACKAGE_HEADER_FILENAME),
            serde_json::to_vec(&header).unwrap(),
        )
        .unwrap();
        assert!(CapabilityCatalog::load(&root).is_err(), "{field}");
    }
}

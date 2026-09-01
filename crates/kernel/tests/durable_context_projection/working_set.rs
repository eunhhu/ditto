// These items are included at the root of `durable_context_projection.rs` so
// they can reuse the admission fixture's canonical node/source constructors.

#[derive(Clone)]
struct WorkingSetManifestSpec {
    id: String,
    summary: String,
    intents: Vec<String>,
    aliases: Vec<String>,
    negative_examples: Vec<String>,
    complements: Vec<String>,
    placement_modes: Vec<String>,
    placement_requirements: Vec<String>,
}

impl WorkingSetManifestSpec {
    fn new(id: impl Into<String>, summary: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            summary: summary.into(),
            intents: Vec::new(),
            aliases: Vec::new(),
            negative_examples: Vec::new(),
            complements: Vec::new(),
            placement_modes: vec!["local".into()],
            placement_requirements: Vec::new(),
        }
    }
}

fn working_set_toml_array(values: &[String]) -> String {
    values
        .iter()
        .map(|value| format!("{value:?}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn working_set_manifest(spec: &WorkingSetManifestSpec) -> String {
    let id = &spec.id;
    let summary = &spec.summary;
    let intents = working_set_toml_array(&spec.intents);
    let aliases = working_set_toml_array(&spec.aliases);
    let negative_examples = working_set_toml_array(&spec.negative_examples);
    let complements = working_set_toml_array(&spec.complements);
    let placement_modes = working_set_toml_array(&spec.placement_modes);
    let placement_requirements = working_set_toml_array(&spec.placement_requirements);
    format!(
        r#"id = {id:?}
version = "0.1.0"
namespace = "fixture"
kind = "tool"
summary = {summary:?}

[runtime]
type = "builtin"
lazy = true
idle_ttl_ms = 30000

[placement]
modes = [{placement_modes}]
requires = [{placement_requirements}]

[retrieval]
intents = [{intents}]
negative_examples = [{negative_examples}]
aliases = [{aliases}]
complements = [{complements}]

[effects]
resources = []

[effects.minimum]
access = "metadata"
mutation = "none"
externality = "local"
privilege = "user"

[effects.maximum]
access = "metadata"
mutation = "none"
externality = "local"
privilege = "user"

[policy]
approval = "never"
secret_handles = []

[verification]
default = "fixture"
"#
    )
}

fn write_working_set_manifest(root: &std::path::Path, index: usize, spec: &WorkingSetManifestSpec) {
    let directory = root.join(format!("manifest-{index:05}"));
    std::fs::create_dir_all(&directory).expect("create capability fixture directory");
    std::fs::write(
        directory.join("capability.toml"),
        working_set_manifest(spec),
    )
    .expect("write capability fixture manifest");
}

struct WorkingSetFixture {
    root: TempDir,
    data_dir: std::path::PathBuf,
    capabilities_dir: std::path::PathBuf,
    config: KernelConfig,
    kernel: DittoKernel,
    store: EventStore,
}

impl WorkingSetFixture {
    fn new(
        manifests: &[WorkingSetManifestSpec],
        provider: Option<Arc<dyn ditto_retrieval::EmbeddingProvider>>,
    ) -> Self {
        let root = tempfile::tempdir().expect("working-set fixture root");
        let data_dir = root.path().join("data");
        let capabilities_dir = root.path().join("capabilities");
        std::fs::create_dir_all(&capabilities_dir).expect("create capability fixture root");
        for (index, manifest) in manifests.iter().enumerate() {
            write_working_set_manifest(&capabilities_dir, index, manifest);
        }
        Self::open(root, data_dir, capabilities_dir, provider)
    }

    fn with_bulk_capabilities(count: usize, summary: &str) -> Self {
        let root = tempfile::tempdir().expect("bulk working-set fixture root");
        let data_dir = root.path().join("data");
        let capabilities_dir = root.path().join("capabilities");
        std::fs::create_dir_all(&capabilities_dir).expect("create bulk capability root");
        for index in 0..count {
            let manifest =
                WorkingSetManifestSpec::new(format!("fixture.cap-{index:05}"), summary.to_owned());
            write_working_set_manifest(&capabilities_dir, index, &manifest);
        }
        Self::open(root, data_dir, capabilities_dir, None)
    }

    fn open(
        root: TempDir,
        data_dir: std::path::PathBuf,
        capabilities_dir: std::path::PathBuf,
        provider: Option<Arc<dyn ditto_retrieval::EmbeddingProvider>>,
    ) -> Self {
        let config = KernelConfig::new(&data_dir, &capabilities_dir);
        let kernel = match provider {
            Some(provider) => DittoKernel::open_with_embedding_provider(config.clone(), provider)
                .expect("open embedded working-set kernel"),
            None => DittoKernel::open(config.clone()).expect("open lexical working-set kernel"),
        };
        let store = EventStore::open(data_dir.join("state.db")).expect("open working-set store");
        Self {
            root,
            data_dir,
            capabilities_dir,
            config,
            kernel,
            store,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkingSetProviderMode {
    Stable,
    FailQuery,
    FailContextDocument,
    FailCapabilityDocument,
    MismatchContextDescriptor,
    MismatchCapabilityDimension,
    RankReversal,
    BlockFirstDocument,
}

type WorkingSetProviderRelease = Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>;
type BlockingWorkingSetProvider = (
    WorkingSetProvider,
    std::sync::mpsc::Receiver<()>,
    WorkingSetProviderRelease,
);

#[derive(Clone)]
struct WorkingSetProvider {
    mode: WorkingSetProviderMode,
    calls: Arc<std::sync::Mutex<Vec<(ditto_retrieval::EmbeddingPurpose, String)>>>,
    block_started: Option<std::sync::mpsc::SyncSender<()>>,
    block_release: Option<WorkingSetProviderRelease>,
    block_once: Arc<std::sync::atomic::AtomicBool>,
}

impl WorkingSetProvider {
    fn new(mode: WorkingSetProviderMode) -> Self {
        Self {
            mode,
            calls: Arc::new(std::sync::Mutex::new(Vec::new())),
            block_started: None,
            block_release: None,
            block_once: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        }
    }

    fn blocking() -> BlockingWorkingSetProvider {
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
        let release = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        (
            Self {
                block_started: Some(started_tx),
                block_release: Some(Arc::clone(&release)),
                ..Self::new(WorkingSetProviderMode::BlockFirstDocument)
            },
            started_rx,
            release,
        )
    }

    fn calls(&self) -> Vec<(ditto_retrieval::EmbeddingPurpose, String)> {
        self.calls.lock().expect("provider call log").clone()
    }

    fn call_count(&self, purpose: ditto_retrieval::EmbeddingPurpose) -> usize {
        self.calls()
            .iter()
            .filter(|(actual, _)| *actual == purpose)
            .count()
    }
}

impl ditto_retrieval::EmbeddingProvider for WorkingSetProvider {
    fn embed(
        &self,
        purpose: ditto_retrieval::EmbeddingPurpose,
        text: &str,
    ) -> Result<ditto_retrieval::Embedding, ditto_retrieval::EmbeddingProviderError> {
        self.calls
            .lock()
            .expect("record provider call")
            .push((purpose, text.to_owned()));

        if purpose == ditto_retrieval::EmbeddingPurpose::Query
            && self.mode == WorkingSetProviderMode::FailQuery
        {
            return Err(ditto_retrieval::EmbeddingProviderError::failure(
                "query embedding failed",
            ));
        }

        let is_context_document = text.contains("\nkind=");
        let is_capability_document = text.contains("\nnamespace=");
        if purpose == ditto_retrieval::EmbeddingPurpose::Document {
            if self.mode == WorkingSetProviderMode::BlockFirstDocument
                && self
                    .block_once
                    .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                self.block_started
                    .as_ref()
                    .expect("blocking provider start sender")
                    .send(())
                    .expect("announce blocked document call");
                let (released, wake) = self
                    .block_release
                    .as_deref()
                    .expect("blocking provider release state");
                let guard = released.lock().expect("blocking provider release lock");
                drop(
                    wake.wait_while(guard, |released| !*released)
                        .expect("wait for blocked provider release"),
                );
            }
            if self.mode == WorkingSetProviderMode::FailContextDocument && is_context_document {
                return Err(ditto_retrieval::EmbeddingProviderError::failure(
                    "context document embedding failed",
                ));
            }
            if self.mode == WorkingSetProviderMode::FailCapabilityDocument && is_capability_document
            {
                return Err(ditto_retrieval::EmbeddingProviderError::failure(
                    "capability document embedding failed",
                ));
            }
            if self.mode == WorkingSetProviderMode::MismatchContextDescriptor && is_context_document
            {
                return Ok(ditto_retrieval::Embedding::new(
                    "wrong-descriptor",
                    vec![1.0, 0.0],
                ));
            }
            if self.mode == WorkingSetProviderMode::MismatchCapabilityDimension
                && is_capability_document
            {
                return Ok(ditto_retrieval::Embedding::new(
                    "fixture-v1",
                    vec![1.0, 0.0, 0.0],
                ));
            }
        }

        let vector = if purpose == ditto_retrieval::EmbeddingPurpose::Query
            || (self.mode == WorkingSetProviderMode::RankReversal
                && text.contains("id=embedding-first"))
        {
            vec![1.0, 0.0]
        } else if self.mode == WorkingSetProviderMode::RankReversal {
            vec![0.0, 1.0]
        } else {
            vec![0.8, 0.2]
        };
        Ok(ditto_retrieval::Embedding::new("fixture-v1", vector))
    }
}

fn working_set_source(
    store: &EventStore,
    session_id: &str,
    task_id: Option<&str>,
    label: &str,
) -> EventRecord {
    store
        .append(NewEvent {
            session_id: Some(session_id.to_owned()),
            task_id: task_id.map(str::to_owned),
            actor: EventActor::User,
            kind: format!("fixture.working-set-source.{label}"),
            payload: json!({"label": label}),
            causation_id: None,
            correlation_id: Some(task_id.unwrap_or(session_id).to_owned()),
            span_id: None,
        })
        .expect("append working-set source")
}

fn working_set_admit_task(
    kernel: &DittoKernel,
    source: &EventRecord,
    task_id: &str,
    id: &str,
    summary: &str,
) -> EventRecord {
    kernel
        .admit_context_node(TrustedContextNodeDraft::task(
            SESSION_A,
            task_id,
            task_user_node(id, &source.event_id, summary),
        ))
        .expect("admit working-set task node")
}

fn working_set_request(request: &str) -> ditto_kernel::WorkingSetRequest {
    ditto_kernel::WorkingSetRequest {
        scope: ditto_retrieval::RetrievalScope::task(
            ditto_retrieval::SessionId::new(SESSION_A).expect("session scope"),
            ditto_retrieval::TaskId::new(TASK_A).expect("task scope"),
        ),
        signature: ditto_retrieval::TaskSignatureV2::new(request),
        context_token_budget: None,
        context_result_limit: 32,
        capability_root_limit: 32,
        execution_epoch_limit: 64,
        capability_search: ditto_capability::SearchContext::catalogue(),
    }
}

fn working_set_node_ids(working_set: &ditto_kernel::WorkingSet) -> Vec<String> {
    working_set
        .compiled_context()
        .nodes
        .iter()
        .map(|node| node.id.clone())
        .collect()
}

fn working_set_capability_ids(working_set: &ditto_kernel::WorkingSet) -> Vec<String> {
    working_set
        .execution_epoch()
        .capabilities()
        .iter()
        .map(|card| card.id.clone())
        .collect()
}

#[derive(Debug, PartialEq)]
struct StableWorkingSetContent {
    mode: String,
    descriptor: Option<String>,
    checkpoint: ditto_context_projection::ProjectionCheckpoint,
    compiled_context: serde_json::Value,
    context_capsule: serde_json::Value,
    capabilities: Vec<(String, String, String)>,
}

fn stable_working_set_content(working_set: &ditto_kernel::WorkingSet) -> StableWorkingSetContent {
    StableWorkingSetContent {
        mode: working_set.retrieval().mode().as_str().to_owned(),
        descriptor: working_set
            .retrieval()
            .embedding_descriptor()
            .map(|descriptor| descriptor.as_str().to_owned()),
        checkpoint: working_set.projection_checkpoint().clone(),
        compiled_context: serde_json::to_value(working_set.compiled_context())
            .expect("serialize stable compiled context"),
        context_capsule: serde_json::to_value(working_set.context_capsule())
            .expect("serialize stable context capsule"),
        capabilities: working_set
            .execution_epoch()
            .capabilities()
            .iter()
            .map(|card| {
                (
                    card.id.clone(),
                    card.namespace.clone(),
                    card.summary.clone(),
                )
            })
            .collect(),
    }
}

fn release_blocked_provider(release: &WorkingSetProviderRelease) {
    let (released, wake) = release.as_ref();
    *released.lock().expect("provider release lock") = true;
    wake.notify_all();
}

#[test]
fn lexical_working_set_rebuilds_context_and_pages_capabilities_without_model_io() {
    let mut root_capability =
        WorkingSetManifestSpec::new("fixture.lookup", "Lookup durable context memory");
    root_capability.intents = vec!["lookup durable context memory".into()];
    root_capability.aliases = vec!["memory lookup".into()];
    root_capability.complements = vec!["fixture.helper".into()];
    let helper_capability =
        WorkingSetManifestSpec::new("fixture.helper", "Helper loaded only as a complement");
    let fixture = WorkingSetFixture::new(&[root_capability, helper_capability], None);
    let source = working_set_source(&fixture.store, SESSION_A, None, "lexical");

    fixture
        .kernel
        .admit_context_node(TrustedContextNodeDraft::session(
            SESSION_A,
            node(
                "session-memory",
                ContextScope::Session,
                ContextOrigin::User,
                EpistemicStatus::Asserted,
                vec![source.event_id.clone()],
                Vec::new(),
                "durable context memory",
            ),
        ))
        .expect("admit session memory");
    working_set_admit_task(
        &fixture.kernel,
        &source,
        TASK_A,
        "task-memory",
        "task context memory",
    );
    working_set_admit_task(
        &fixture.kernel,
        &source,
        TASK_A,
        "resource-exact",
        "opaque exact identity",
    );
    working_set_admit_task(
        &fixture.kernel,
        &source,
        TASK_B,
        "wrong-task-memory",
        "durable context memory in another task",
    );
    working_set_admit_task(
        &fixture.kernel,
        &source,
        TASK_A,
        "superseded-memory",
        "durable context memory old",
    );
    fixture
        .kernel
        .admit_context_node(TrustedContextNodeDraft::task(
            SESSION_A,
            TASK_A,
            node(
                "replacement-memory",
                ContextScope::Task,
                ContextOrigin::User,
                EpistemicStatus::Asserted,
                vec![source.event_id.clone()],
                vec!["superseded-memory".into()],
                "durable context memory replacement",
            ),
        ))
        .expect("admit replacement memory");
    fixture
        .kernel
        .admit_context_node(TrustedContextNodeDraft::task(
            SESSION_A,
            TASK_A,
            node(
                "disputed-memory",
                ContextScope::Task,
                ContextOrigin::User,
                EpistemicStatus::Disputed,
                vec![source.event_id.clone()],
                Vec::new(),
                "durable context memory disputed",
            ),
        ))
        .expect("admit disputed memory");
    let mut expired = task_user_node(
        "expired-memory",
        &source.event_id,
        "durable context memory expired",
    );
    expired.valid_until = Some(Utc::now() - Duration::hours(1));
    fixture
        .kernel
        .admit_context_node(TrustedContextNodeDraft::task(SESSION_A, TASK_A, expired))
        .expect("admit expired memory");

    let mut request = working_set_request("lookup durable context memory replacement");
    request.signature.resources = vec!["fixture.lookup".into(), "resource-exact".into()];
    let event_count = fixture
        .kernel
        .event_count()
        .expect("pre-retrieval event count");
    let initial = fixture
        .kernel
        .retrieve_working_set(request.clone())
        .expect("initial lexical working set");
    assert_eq!(initial.retrieval().mode().as_str(), "lexical_only");
    assert!(initial.retrieval().embedding_descriptor().is_none());
    let initial_ids = working_set_node_ids(&initial);
    for expected in [
        "session-memory",
        "task-memory",
        "resource-exact",
        "replacement-memory",
    ] {
        assert!(
            initial_ids.iter().any(|id| id == expected),
            "missing {expected}"
        );
    }
    for excluded in [
        "wrong-task-memory",
        "superseded-memory",
        "disputed-memory",
        "expired-memory",
    ] {
        assert!(
            initial_ids.iter().all(|id| id != excluded),
            "unexpected {excluded}"
        );
    }
    assert_eq!(
        initial_ids
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        initial_ids.len()
    );
    assert_eq!(
        working_set_capability_ids(&initial),
        vec!["fixture.lookup", "fixture.helper"]
    );
    assert_eq!(
        fixture.kernel.event_count().expect("post-retrieval count"),
        event_count
    );

    let incremental = working_set_admit_task(
        &fixture.kernel,
        &source,
        TASK_A,
        "incremental-memory",
        "incremental durable context memory",
    );
    let after_incremental_count = fixture
        .kernel
        .event_count()
        .expect("post-incremental event count");
    let final_live = fixture
        .kernel
        .retrieve_working_set(request.clone())
        .expect("incremental lexical working set");
    assert!(
        working_set_node_ids(&final_live)
            .iter()
            .any(|id| id == "incremental-memory")
    );
    assert_eq!(
        final_live.projection_checkpoint().through_seq,
        incremental.seq
    );
    let stable_live = stable_working_set_content(&final_live);
    assert_eq!(
        fixture
            .kernel
            .event_count()
            .expect("post-incremental retrieval count"),
        after_incremental_count
    );

    let WorkingSetFixture {
        root,
        data_dir,
        capabilities_dir: _,
        config,
        kernel,
        store,
    } = fixture;
    drop((kernel, store));
    let reopened = DittoKernel::open(config.clone()).expect("reopen lexical kernel");
    let reopened_result = reopened
        .retrieve_working_set(request.clone())
        .expect("reopened lexical working set");
    assert_eq!(stable_working_set_content(&reopened_result), stable_live);
    assert_ne!(
        reopened_result.execution_epoch().id,
        final_live.execution_epoch().id,
        "epoch ULIDs are issuance identities"
    );
    assert_eq!(
        reopened.event_count().expect("reopened event count"),
        after_incremental_count
    );
    drop(reopened);

    let projection_path = data_dir.join("context-projection.db");
    std::fs::remove_file(&projection_path).expect("delete derived projection database");
    for suffix in ["-wal", "-shm"] {
        let sidecar = std::path::PathBuf::from(format!("{}{suffix}", projection_path.display()));
        if sidecar.exists() {
            std::fs::remove_file(sidecar).expect("delete derived projection sidecar");
        }
    }
    let rebuilt = DittoKernel::open(config).expect("reopen after projection deletion");
    let rebuilt_result = rebuilt
        .retrieve_working_set(request)
        .expect("cache-rebuilt lexical working set");
    assert_eq!(stable_working_set_content(&rebuilt_result), stable_live);
    assert_eq!(
        rebuilt.event_count().expect("rebuilt event count"),
        after_incremental_count
    );
    drop((rebuilt, root));
}

#[test]
fn one_query_embedding_is_shared_without_bypassing_domain_filters() {
    let mut allowed = WorkingSetManifestSpec::new("fixture.allowed", "Deploy service safely");
    allowed.intents = vec!["deploy service".into()];
    let mut wrong_placement =
        WorkingSetManifestSpec::new("fixture.wrong-placement", "Deploy service remotely");
    wrong_placement.intents = vec!["deploy service".into()];
    wrong_placement.placement_modes = vec!["ssh".into()];
    let mut negative =
        WorkingSetManifestSpec::new("fixture.negative", "Deploy service negative example");
    negative.intents = vec!["deploy service".into()];
    negative.negative_examples = vec!["deploy service".into()];
    let provider = WorkingSetProvider::new(WorkingSetProviderMode::Stable);
    let fixture = WorkingSetFixture::new(
        &[allowed, wrong_placement, negative],
        Some(Arc::new(provider.clone())),
    );
    let source = working_set_source(&fixture.store, SESSION_A, None, "embedded-domain");
    working_set_admit_task(
        &fixture.kernel,
        &source,
        TASK_A,
        "context-eligible",
        "deploy service safely",
    );
    working_set_admit_task(
        &fixture.kernel,
        &source,
        TASK_B,
        "context-wrong-task",
        "deploy service safely",
    );
    let mut disputed = task_user_node(
        "context-disputed",
        &source.event_id,
        "deploy service safely",
    );
    disputed.epistemic = EpistemicStatus::Disputed;
    fixture
        .kernel
        .admit_context_node(TrustedContextNodeDraft::task(SESSION_A, TASK_A, disputed))
        .expect("admit disputed embedded candidate");
    let mut expired = task_user_node("context-expired", &source.event_id, "deploy service safely");
    expired.valid_until = Some(Utc::now() - Duration::hours(2));
    fixture
        .kernel
        .admit_context_node(TrustedContextNodeDraft::task(SESSION_A, TASK_A, expired))
        .expect("admit expired embedded candidate");
    working_set_admit_task(
        &fixture.kernel,
        &source,
        TASK_A,
        "context-old",
        "deploy service old",
    );
    fixture
        .kernel
        .admit_context_node(TrustedContextNodeDraft::task(
            SESSION_A,
            TASK_A,
            node(
                "context-replacement",
                ContextScope::Task,
                ContextOrigin::User,
                EpistemicStatus::Asserted,
                vec![source.event_id],
                vec!["context-old".into()],
                "deploy service replacement",
            ),
        ))
        .expect("admit embedded replacement");

    let mut request = working_set_request("deploy service");
    request.capability_search = ditto_capability::SearchContext::runtime(
        vec!["local".into()],
        Vec::new(),
        ditto_capability::EffectProfile::read_content(),
    );
    let before = fixture.kernel.event_count().expect("embedded pre-count");
    let result = fixture
        .kernel
        .retrieve_working_set(request)
        .expect("embedded working set");
    assert_eq!(result.retrieval().mode().as_str(), "embedded");
    assert_eq!(
        result
            .retrieval()
            .embedding_descriptor()
            .expect("embedding descriptor")
            .as_str(),
        "fixture-v1"
    );
    assert_eq!(
        provider.call_count(ditto_retrieval::EmbeddingPurpose::Query),
        1
    );
    let calls = provider.calls();
    let documents = calls
        .iter()
        .filter(|(purpose, _)| *purpose == ditto_retrieval::EmbeddingPurpose::Document)
        .map(|(_, text)| text.as_str())
        .collect::<Vec<_>>();
    assert!(
        documents
            .iter()
            .any(|text| text.contains("id=context-eligible"))
    );
    assert!(
        documents
            .iter()
            .any(|text| text.contains("id=context-replacement"))
    );
    assert!(
        documents
            .iter()
            .any(|text| text.contains("id=fixture.allowed"))
    );
    for denied in [
        "context-wrong-task",
        "context-disputed",
        "context-expired",
        "context-old",
        "fixture.wrong-placement",
        "fixture.negative",
    ] {
        assert!(
            documents.iter().all(|text| !text.contains(denied)),
            "hard-filtered document reached provider: {denied}"
        );
    }
    let ids = working_set_node_ids(&result);
    assert_eq!(ids, vec!["context-eligible", "context-replacement"]);
    assert_eq!(working_set_capability_ids(&result), vec!["fixture.allowed"]);
    assert_eq!(
        fixture.kernel.event_count().expect("embedded post-count"),
        before
    );
}

#[test]
fn shared_gate_orders_successful_admission_and_projection_snapshot_visibility() {
    let (provider, started, release) = WorkingSetProvider::blocking();
    let fixture = WorkingSetFixture::new(&[], Some(Arc::new(provider)));
    let source = working_set_source(&fixture.store, SESSION_A, None, "shared-gate");
    let initial = working_set_admit_task(
        &fixture.kernel,
        &source,
        TASK_A,
        "gate-initial",
        "gate memory initial",
    );
    let retrieval_kernel = fixture.kernel.clone();
    let retrieval = thread::spawn(move || {
        retrieval_kernel.retrieve_working_set(working_set_request("gate memory"))
    });
    started
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("retrieval reached post-snapshot document embedding");

    let admission_kernel = fixture.kernel.clone();
    let admission_source = source.clone();
    let (admitted_tx, admitted_rx) = std::sync::mpsc::sync_channel(1);
    let admission = thread::spawn(move || {
        admitted_tx
            .send(working_set_admit_task(
                &admission_kernel,
                &admission_source,
                TASK_A,
                "gate-later",
                "gate memory later",
            ))
            .expect("send admission result");
    });
    let later = match admitted_rx.recv_timeout(std::time::Duration::from_secs(5)) {
        Ok(event) => event,
        Err(error) => {
            release_blocked_provider(&release);
            let _ = retrieval.join();
            let _ = admission.join();
            panic!("admission gate was held through document embedding: {error}");
        }
    };
    release_blocked_provider(&release);
    admission.join().expect("join concurrent admission");
    let earlier = retrieval
        .join()
        .expect("join earlier retrieval")
        .expect("earlier detached working set");
    assert_eq!(working_set_node_ids(&earlier), vec!["gate-initial"]);
    assert_eq!(earlier.projection_checkpoint().through_seq, initial.seq);
    assert!(earlier.projection_checkpoint().through_seq < later.seq);

    let after = fixture
        .kernel
        .retrieve_working_set(working_set_request("gate memory"))
        .expect("post-admission working set");
    assert_eq!(
        working_set_node_ids(&after),
        vec!["gate-initial", "gate-later"]
    );
    assert_eq!(after.projection_checkpoint().through_seq, later.seq);
    assert_eq!(
        fixture
            .kernel
            .event_count()
            .expect("shared-gate event count"),
        u64::try_from(later.seq).expect("positive event sequence")
    );
}

#[test]
fn legacy_search_clamp_and_execution_epoch_behavior_remain_unchanged() {
    let fixture = WorkingSetFixture::with_bulk_capabilities(150, "Legacy searchable capability");
    let zero_clamped = fixture.kernel.search_capabilities("legacy", 0);
    assert_eq!(
        zero_clamped.len(),
        1,
        "kernel legacy wrapper still clamps zero"
    );
    let over_clamped = fixture.kernel.search_capabilities("legacy", usize::MAX);
    assert_eq!(
        over_clamped.len(),
        100,
        "kernel legacy wrapper still clamps at 100"
    );

    let catalogue = ditto_capability::CapabilityCatalog::load(&fixture.capabilities_dir)
        .expect("load legacy catalogue directly");
    assert!(catalogue.search("legacy", 0).is_empty());
    assert_eq!(catalogue.search("LEGACY !!!", 2).len(), 2);
    assert_eq!(
        catalogue
            .search_with_context("legacy", &ditto_capability::SearchContext::catalogue(), 3)
            .len(),
        3
    );

    let empty_epoch = fixture.kernel.build_execution_epoch(
        "legacy",
        &ditto_capability::SearchContext::catalogue(),
        0,
    );
    assert_eq!(empty_epoch.max_working_set(), 0);
    assert!(empty_epoch.capabilities().is_empty());
    let mut built = fixture.kernel.build_execution_epoch(
        "legacy",
        &ditto_capability::SearchContext::catalogue(),
        2,
    );
    assert_eq!(built.max_working_set(), 2);
    assert_eq!(built.capabilities().len(), 2);
    assert_eq!(
        fixture.kernel.page_execution_epoch(
            &mut built,
            "legacy",
            &ditto_capability::SearchContext::catalogue()
        ),
        0
    );

    let cards = fixture.kernel.search_capabilities("legacy", 3);
    let mut direct = ditto_capability::ExecutionEpoch::new(2);
    assert_eq!(
        direct.page_in([
            cards[0].clone(),
            cards[0].clone(),
            cards[1].clone(),
            cards[2].clone(),
        ]),
        2
    );
    assert_eq!(direct.capabilities().len(), 2);
    assert_eq!(direct.remaining_capacity(), 0);
    assert_eq!(direct.page_in(cards), 0);
    assert_eq!(fixture.kernel.event_count().expect("legacy event count"), 0);
}

fn record_working_set_context_event(
    store: &EventStore,
    source: &EventRecord,
    node_id: String,
) -> EventRecord {
    let context_node = task_user_node(node_id, &source.event_id, "prefilter candidate");
    store
        .append(NewEvent {
            session_id: Some(SESSION_A.to_owned()),
            task_id: Some(TASK_A.to_owned()),
            actor: EventActor::System,
            kind: event_kind::CONTEXT_NODE_RECORDED.to_owned(),
            payload: serde_json::to_value(ContextNodeRecordedPayloadV1::new(context_node))
                .expect("serialize prefilter context event"),
            causation_id: Some(source.event_id.clone()),
            correlation_id: Some(TASK_A.to_owned()),
            span_id: None,
        })
        .expect("append prefilter context event")
}

fn assert_limit_error_before_provider(
    kernel: &DittoKernel,
    provider: &WorkingSetProvider,
    request: ditto_kernel::WorkingSetRequest,
    expected: &str,
) {
    let error = kernel
        .retrieve_working_set(request)
        .expect_err("invalid raw limit must reject the whole working set");
    match expected {
        "context" => assert!(matches!(
            error,
            ditto_kernel::WorkingSetError::ContextResultLimit(
                ditto_retrieval::RetrievalError::ResultLimitOutOfRange { .. }
            )
        )),
        "capability" => assert!(matches!(
            error,
            ditto_kernel::WorkingSetError::CapabilityRootLimit(
                ditto_retrieval::RetrievalError::ResultLimitOutOfRange { .. }
            )
        )),
        "epoch" => assert!(matches!(
            error,
            ditto_kernel::WorkingSetError::ExecutionEpochLimit(
                ditto_retrieval::RetrievalError::ResultLimitOutOfRange { .. }
            )
        )),
        other => panic!("unknown raw-limit assertion {other}"),
    }
    assert!(provider.calls().is_empty());
}

#[test]
fn prefilter_scan_and_zero_n_plus_one_limits_return_no_partial_working_set() {
    let provider = WorkingSetProvider::new(WorkingSetProviderMode::Stable);
    let limits_fixture = WorkingSetFixture::new(&[], Some(Arc::new(provider.clone())));
    let base = working_set_request("bounded retrieval");
    for (field, value, expected) in [
        ("context", 0, "context"),
        ("context", 257, "context"),
        ("capability", 0, "capability"),
        ("capability", 257, "capability"),
        ("epoch", 0, "epoch"),
        ("epoch", 513, "epoch"),
    ] {
        let mut request = base.clone();
        match field {
            "context" => request.context_result_limit = value,
            "capability" => request.capability_root_limit = value,
            "epoch" => request.execution_epoch_limit = value,
            _ => unreachable!(),
        }
        assert_limit_error_before_provider(&limits_fixture.kernel, &provider, request, expected);
    }
    let mut all_invalid = base.clone();
    all_invalid.context_result_limit = 0;
    all_invalid.capability_root_limit = 0;
    all_invalid.execution_epoch_limit = 0;
    assert_limit_error_before_provider(
        &limits_fixture.kernel,
        &provider,
        all_invalid,
        "context",
    );
    let count_before_valid = limits_fixture
        .kernel
        .event_count()
        .expect("valid limit pre-count");
    let mut minimum = base.clone();
    minimum.context_result_limit = 1;
    minimum.capability_root_limit = 1;
    minimum.execution_epoch_limit = 1;
    minimum.context_token_budget = Some(0);
    let minimum_result = limits_fixture
        .kernel
        .retrieve_working_set(minimum)
        .expect("minimum V2 limits and zero token budget are valid");
    assert!(minimum_result.compiled_context().nodes.is_empty());
    let mut maximum = base;
    maximum.context_result_limit = 256;
    maximum.capability_root_limit = 256;
    maximum.execution_epoch_limit = 512;
    limits_fixture
        .kernel
        .retrieve_working_set(maximum)
        .expect("maximum V2 limits are valid");
    assert_eq!(
        provider.call_count(ditto_retrieval::EmbeddingPurpose::Query),
        2
    );
    assert_eq!(
        limits_fixture
            .kernel
            .event_count()
            .expect("valid limit post-count"),
        count_before_valid
    );

    let context_fixture = WorkingSetFixture::new(&[], None);
    let source = working_set_source(&context_fixture.store, SESSION_A, None, "context-prefilter");
    let mut last = None;
    for index in 0..10_000 {
        last = Some(record_working_set_context_event(
            &context_fixture.store,
            &source,
            format!("prefilter-{index:05}"),
        ));
    }
    let canonical_count = context_fixture
        .kernel
        .event_count()
        .expect("canonical N event count");
    let no_match_request = working_set_request("lexically absent request");
    let at_limit = context_fixture
        .kernel
        .retrieve_working_set(no_match_request.clone())
        .expect("10,000 canonical prefilter rows are accepted");
    assert!(at_limit.compiled_context().nodes.is_empty());
    assert_eq!(
        context_fixture
            .kernel
            .event_count()
            .expect("canonical N post-retrieval count"),
        canonical_count
    );

    let fake_node = task_user_node(
        "cache-only-prefilter-10000",
        &source.event_id,
        "cache-only overflow must not deny retrieval",
    );
    let last = last.expect("last canonical prefilter event");
    let connection = Connection::open(context_fixture.data_dir.join("context-projection.db"))
        .expect("open prefilter projection cache");
    connection
        .execute(
            "INSERT INTO projected_nodes (session_id, task_id, node_id, event_seq, event_id, node_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                SESSION_A,
                TASK_A,
                fake_node.id,
                last.seq + 10_000,
                "cache-only-prefilter-event",
                serde_json::to_string(&fake_node).expect("serialize cache-only prefilter row"),
            ],
        )
        .expect("insert cache-only 10,001st row");
    drop(connection);
    let repaired = context_fixture
        .kernel
        .retrieve_working_set(no_match_request.clone())
        .expect("cache-only 10,001st row rebuilds away instead of denying retrieval");
    assert!(repaired.compiled_context().nodes.is_empty());
    assert_eq!(
        context_fixture
            .kernel
            .event_count()
            .expect("cache-only overflow source count"),
        canonical_count
    );
    let connection = Connection::open(context_fixture.data_dir.join("context-projection.db"))
        .expect("verify prefilter cache repair");
    let fake_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM projected_nodes WHERE event_id = 'cache-only-prefilter-event'",
            [],
            |row| row.get(0),
        )
        .expect("count repaired fake row");
    assert_eq!(fake_count, 0);
    drop(connection);

    record_working_set_context_event(&context_fixture.store, &source, "prefilter-10000".into());
    let canonical_overflow_count = context_fixture
        .kernel
        .event_count()
        .expect("canonical N+1 event count");
    let context_error = context_fixture
        .kernel
        .retrieve_working_set(no_match_request)
        .expect_err("canonical 10,001st row must reject the whole working set");
    assert!(matches!(
        context_error,
        ditto_kernel::WorkingSetError::ContextProjection(ContextProjectionError::Retrieval(
            ditto_retrieval::RetrievalError::CandidateCountExceeded {
                actual: 10_001,
                maximum: 10_000,
            }
        ))
    ));
    assert_eq!(
        context_fixture
            .kernel
            .event_count()
            .expect("canonical N+1 post-error count"),
        canonical_overflow_count
    );

    let capability_fixture =
        WorkingSetFixture::with_bulk_capabilities(10_001, "catalogue prefilter candidate");
    let capability_error = capability_fixture
        .kernel
        .retrieve_working_set(working_set_request("lexically absent request"))
        .expect_err("10,001st installed manifest must reject the whole working set");
    assert!(matches!(
        capability_error,
        ditto_kernel::WorkingSetError::CapabilitySearch(
            ditto_capability::CapabilitySearchError::Retrieval(
                ditto_retrieval::RetrievalError::CandidateCountExceeded {
                    actual: 10_001,
                    maximum: 10_000,
                }
            )
        )
    ));
    assert_eq!(
        capability_fixture
            .kernel
            .event_count()
            .expect("capability overflow event count"),
        0
    );
}

#[test]
fn invalid_scope_and_search_context_are_rejected_before_provider_io() {
    let provider = WorkingSetProvider::new(WorkingSetProviderMode::Stable);
    let fixture = WorkingSetFixture::new(&[], Some(Arc::new(provider.clone())));

    assert!(matches!(
        ditto_retrieval::SessionId::new("bad\nsession"),
        Err(ditto_retrieval::RetrievalError::IdentifierForbiddenCharacter {
            field: "session_id"
        })
    ));
    assert!(provider.calls().is_empty());

    let mut request = working_set_request("invalid placement context");
    request.capability_search.available_placements = Some(vec!["ssh".into()]);
    request.capability_search.preferred_placement = Some("local".into());
    let error = fixture
        .kernel
        .retrieve_working_set(request)
        .expect_err("unavailable preferred placement");
    assert!(matches!(
        error,
        ditto_kernel::WorkingSetError::CapabilitySearch(
            ditto_capability::CapabilitySearchError::InvalidSearchContext(
                ditto_capability::SearchContextError::PreferredPlacementUnavailable { .. }
            )
        )
    ));
    assert!(
        provider.calls().is_empty(),
        "invalid scope/context must precede the shared query provider call"
    );
}

#[test]
fn repeated_working_sets_reuse_startup_verification_and_advance_by_delta() {
    let fixture = WorkingSetFixture::new(&[], None);
    let startup = fixture
        .kernel
        .retrieval_verification_metrics()
        .expect("startup verification metrics");
    assert_eq!(startup.full_replays, 1);

    let request = working_set_request("steady retrieval");
    fixture
        .kernel
        .retrieve_working_set(request.clone())
        .expect("first steady snapshot");
    fixture
        .kernel
        .retrieve_working_set(request.clone())
        .expect("second steady snapshot");
    let steady = fixture
        .kernel
        .retrieval_verification_metrics()
        .expect("steady verification metrics");
    assert_eq!(steady.full_replays, 1);
    assert_eq!(steady.fast_snapshots, 2);

    working_set_source(&fixture.store, SESSION_A, None, "delta-metrics");
    fixture
        .kernel
        .retrieve_working_set(request)
        .expect("delta working set");
    let delta = fixture
        .kernel
        .retrieval_verification_metrics()
        .expect("delta verification metrics");
    assert_eq!(delta.full_replays, 1);
    assert_eq!(delta.delta_synchronizations, 1);
    assert_eq!(delta.fast_snapshots, 3);
}

fn embedded_failure_fixture(
    mode: WorkingSetProviderMode,
    context_candidate: bool,
    capability_candidate: bool,
) -> (WorkingSetFixture, WorkingSetProvider) {
    let manifests = if capability_candidate {
        let mut manifest =
            WorkingSetManifestSpec::new("fixture.failure", "provider failure candidate");
        manifest.intents = vec!["provider failure".into()];
        vec![manifest]
    } else {
        Vec::new()
    };
    let provider = WorkingSetProvider::new(mode);
    let fixture = WorkingSetFixture::new(&manifests, Some(Arc::new(provider.clone())));
    if context_candidate {
        let source = working_set_source(&fixture.store, SESSION_A, None, "provider-failure");
        working_set_admit_task(
            &fixture.kernel,
            &source,
            TASK_A,
            "provider-failure-context",
            "provider failure candidate",
        );
    }
    (fixture, provider)
}

#[test]
fn configured_embedding_failure_returns_no_partial_working_set() {
    let (query_fixture, query_provider) =
        embedded_failure_fixture(WorkingSetProviderMode::FailQuery, false, false);
    let query_before = query_fixture
        .kernel
        .event_count()
        .expect("query failure pre-count");
    let query_error = query_fixture
        .kernel
        .retrieve_working_set(working_set_request("provider failure"))
        .expect_err("query provider failure must not fall back");
    assert!(matches!(
        query_error,
        ditto_kernel::WorkingSetError::Query(
            ditto_retrieval::RetrievalError::ProviderFailure { .. }
        )
    ));
    assert_eq!(
        query_provider.call_count(ditto_retrieval::EmbeddingPurpose::Query),
        1
    );
    assert_eq!(
        query_fixture
            .kernel
            .event_count()
            .expect("query failure post-count"),
        query_before
    );

    for mode in [
        WorkingSetProviderMode::FailContextDocument,
        WorkingSetProviderMode::MismatchContextDescriptor,
    ] {
        let (fixture, provider) = embedded_failure_fixture(mode, true, false);
        let before = fixture
            .kernel
            .event_count()
            .expect("context failure pre-count");
        let error = fixture
            .kernel
            .retrieve_working_set(working_set_request("provider failure"))
            .expect_err("context provider failure must reject the whole working set");
        assert!(matches!(
            error,
            ditto_kernel::WorkingSetError::ContextRanking(
                ditto_context::ContextQueryRankingError::Retrieval(
                    ditto_retrieval::RetrievalError::ProviderFailure { .. }
                        | ditto_retrieval::RetrievalError::EmbeddingDescriptorMismatch { .. }
                )
            )
        ));
        assert_eq!(
            provider.call_count(ditto_retrieval::EmbeddingPurpose::Query),
            1
        );
        assert_eq!(
            fixture
                .kernel
                .event_count()
                .expect("context failure post-count"),
            before
        );
    }

    for mode in [
        WorkingSetProviderMode::FailCapabilityDocument,
        WorkingSetProviderMode::MismatchCapabilityDimension,
    ] {
        let (fixture, provider) = embedded_failure_fixture(mode, false, true);
        let before = fixture
            .kernel
            .event_count()
            .expect("capability failure pre-count");
        let error = fixture
            .kernel
            .retrieve_working_set(working_set_request("provider failure"))
            .expect_err("capability provider failure must reject the whole working set");
        assert!(matches!(
            error,
            ditto_kernel::WorkingSetError::CapabilitySearch(
                ditto_capability::CapabilitySearchError::Retrieval(
                    ditto_retrieval::RetrievalError::ProviderFailure { .. }
                        | ditto_retrieval::RetrievalError::EmbeddingDimensionMismatch { .. }
                )
            )
        ));
        assert_eq!(
            provider.call_count(ditto_retrieval::EmbeddingPurpose::Query),
            1
        );
        assert_eq!(
            fixture
                .kernel
                .event_count()
                .expect("capability failure post-count"),
            before
        );
    }
}

#[test]
fn token_budget_reversal_preserves_opaque_rank_order_and_receipt_lexical_scores() {
    let provider = WorkingSetProvider::new(WorkingSetProviderMode::RankReversal);
    let fixture = WorkingSetFixture::new(&[], Some(Arc::new(provider.clone())));
    let source = working_set_source(&fixture.store, SESSION_A, None, "rank-reversal");
    working_set_admit_task(
        &fixture.kernel,
        &source,
        TASK_A,
        "lexical-first",
        "alpha beta gamma delta",
    );
    working_set_admit_task(&fixture.kernel, &source, TASK_A, "embedding-first", "alpha");
    let before = fixture
        .kernel
        .event_count()
        .expect("rank reversal pre-count");
    let mut request = working_set_request("alpha beta gamma delta");
    request.context_result_limit = 2;
    let full = fixture
        .kernel
        .retrieve_working_set(request.clone())
        .expect("full embedded ranking");
    assert_eq!(
        working_set_node_ids(&full),
        vec!["embedding-first", "lexical-first"]
    );
    let embedding_receipt = full
        .compiled_context()
        .receipt
        .included
        .iter()
        .find(|entry| entry.node_id == "embedding-first")
        .expect("embedding-first receipt");
    let lexical_receipt = full
        .compiled_context()
        .receipt
        .included
        .iter()
        .find(|entry| entry.node_id == "lexical-first")
        .expect("lexical-first receipt");
    assert!(
        embedding_receipt.score < lexical_receipt.score,
        "receipt scores remain lexical even when embeddings reverse order"
    );
    let embedding_score = embedding_receipt.score;
    let embedding_cost = embedding_receipt.token_cost;

    request.context_token_budget = Some(embedding_cost);
    let budgeted = fixture
        .kernel
        .retrieve_working_set(request)
        .expect("budgeted embedded ranking");
    assert_eq!(working_set_node_ids(&budgeted), vec!["embedding-first"]);
    let selected = &budgeted.compiled_context().receipt.included[0];
    assert_eq!(selected.score.to_bits(), embedding_score.to_bits());
    assert_eq!(selected.token_cost, embedding_cost);
    assert!(
        budgeted
            .compiled_context()
            .receipt
            .excluded
            .iter()
            .any(|entry| {
                entry.node_id == "lexical-first"
                    && entry.reason == ditto_context::ContextExclusionReason::TokenBudget
            })
    );
    assert_eq!(
        provider.call_count(ditto_retrieval::EmbeddingPurpose::Query),
        2
    );
    assert_eq!(
        fixture
            .kernel
            .event_count()
            .expect("rank reversal post-count"),
        before
    );
}

#[test]
fn cache_row_deletion_alteration_and_cache_only_rows_rebuild_or_reject_without_authority() {
    let fixture = WorkingSetFixture::new(&[], None);
    let source = working_set_source(&fixture.store, SESSION_A, None, "cache-authority");
    working_set_admit_task(
        &fixture.kernel,
        &source,
        TASK_A,
        "canonical-row",
        "canonical cache authority summary",
    );
    working_set_admit_task(
        &fixture.kernel,
        &source,
        TASK_A,
        "canonical-target",
        "canonical cache authority target",
    );
    let request = working_set_request("canonical cache authority");
    let baseline = fixture
        .kernel
        .retrieve_working_set(request.clone())
        .expect("baseline source-authenticated snapshot");
    let baseline_content = stable_working_set_content(&baseline);
    let source_count = fixture
        .kernel
        .event_count()
        .expect("cache authority source count");
    let projection_path = fixture.data_dir.join("context-projection.db");

    let connection = Connection::open(&projection_path).expect("open deletion cache connection");
    assert_eq!(
        connection
            .execute(
                "DELETE FROM projected_nodes WHERE session_id = ?1 AND node_id = ?2",
                rusqlite::params![SESSION_A, "canonical-row"],
            )
            .expect("delete canonical cache row"),
        1
    );
    drop(connection);
    let after_delete = fixture
        .kernel
        .retrieve_working_set(request.clone())
        .expect("retrieval repairs deleted cache row");
    assert_eq!(stable_working_set_content(&after_delete), baseline_content);
    assert_eq!(
        fixture
            .kernel
            .event_count()
            .expect("deleted-row source count"),
        source_count
    );

    let mut altered = task_user_node(
        "canonical-row",
        &source.event_id,
        "cache-only altered summary",
    );
    altered.scope = ContextScope::Task;
    let connection = Connection::open(&projection_path).expect("open alteration cache connection");
    assert_eq!(
        connection
            .execute(
                "UPDATE projected_nodes SET node_json = ?1 WHERE session_id = ?2 AND node_id = ?3",
                rusqlite::params![
                    serde_json::to_string(&altered).expect("serialize altered cache row"),
                    SESSION_A,
                    "canonical-row",
                ],
            )
            .expect("alter canonical cache row"),
        1
    );
    drop(connection);
    let after_alteration = fixture
        .kernel
        .retrieve_working_set(request.clone())
        .expect("retrieval repairs altered cache row");
    assert_eq!(
        stable_working_set_content(&after_alteration),
        baseline_content
    );
    assert!(
        after_alteration
            .compiled_context()
            .nodes
            .iter()
            .any(|node| node.id == "canonical-row"
                && node.summary == "canonical cache authority summary")
    );
    assert_eq!(
        fixture
            .kernel
            .event_count()
            .expect("altered-row source count"),
        source_count
    );

    let fake_node = task_user_node(
        "cache-only-row",
        &source.event_id,
        "cache-only authority must disappear",
    );
    let checkpoint = after_alteration.projection_checkpoint().clone();
    let connection = Connection::open(&projection_path).expect("open cache-only connection");
    connection
        .execute(
            "INSERT INTO projected_nodes (session_id, task_id, node_id, event_seq, event_id, node_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                SESSION_A,
                TASK_A,
                fake_node.id,
                checkpoint.through_seq + 10_000,
                "cache-only-event",
                serde_json::to_string(&fake_node).expect("serialize cache-only row"),
            ],
        )
        .expect("insert cache-only identity row");
    connection
        .execute(
            "INSERT INTO supersession_edges (session_id, task_key, superseding_node_id, superseded_node_id, event_seq) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                SESSION_A,
                TASK_A,
                "cache-only-row",
                "canonical-target",
                checkpoint.through_seq + 10_000,
            ],
        )
        .expect("insert cache-only supersession edge");
    drop(connection);
    let after_cache_only = fixture
        .kernel
        .retrieve_working_set(request.clone())
        .expect("retrieval rebuilds cache-only row and edge away");
    assert_eq!(
        stable_working_set_content(&after_cache_only),
        baseline_content
    );
    assert!(
        working_set_node_ids(&after_cache_only)
            .iter()
            .all(|id| id != "cache-only-row")
    );
    assert!(
        working_set_node_ids(&after_cache_only)
            .iter()
            .any(|id| id == "canonical-target")
    );
    let connection = Connection::open(&projection_path).expect("verify cache-only repair");
    let fake_rows: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM projected_nodes WHERE event_id = 'cache-only-event'",
            [],
            |row| row.get(0),
        )
        .expect("count cache-only rows after repair");
    let fake_edges: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM supersession_edges WHERE superseding_node_id = 'cache-only-row'",
            [],
            |row| row.get(0),
        )
        .expect("count cache-only edges after repair");
    assert_eq!((fake_rows, fake_edges), (0, 0));

    let connection = Connection::open(&projection_path).expect("reopen cache-only connection");
    connection
        .execute(
            "INSERT INTO projected_nodes (session_id, task_id, node_id, event_seq, event_id, node_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                SESSION_A,
                TASK_A,
                fake_node.id,
                checkpoint.through_seq + 10_000,
                "cache-only-event-second",
                serde_json::to_string(&fake_node).expect("serialize repeated cache-only row"),
            ],
        )
        .expect("reinsert cache-only identity row");
    drop(connection);
    let before_rejected_admission = fixture
        .kernel
        .event_count()
        .expect("cache-only supersession pre-count");
    let error = fixture
        .kernel
        .admit_context_node(TrustedContextNodeDraft::task(
            SESSION_A,
            TASK_A,
            node(
                "cache-only-superseder",
                ContextScope::Task,
                ContextOrigin::User,
                EpistemicStatus::Asserted,
                vec![source.event_id],
                vec!["cache-only-row".into()],
                "cache-only target cannot authorize supersession",
            ),
        ))
        .expect_err("cache-only supersession target must not authorize admission");
    assert!(matches!(
        error,
        KernelError::ContextProjection(ContextProjectionError::MissingSupersededNode {
            ref superseded_id,
            ..
        }) if superseded_id == "cache-only-row"
    ));
    assert_eq!(
        fixture
            .kernel
            .event_count()
            .expect("cache-only supersession post-count"),
        before_rejected_admission
    );
    assert_eq!(before_rejected_admission, source_count);
}

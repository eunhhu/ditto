use std::{
    collections::{BTreeSet, VecDeque},
    fs,
    sync::{Arc, Mutex},
};

use async_stream::stream;
use chrono::{Duration as ChronoDuration, Utc};
use ditto_artifact_read::{
    ARTIFACT_READ_ID, ArtifactReadDeriver, MAX_READ_BYTES, capability_schema,
};
use ditto_artifact_store::ArtifactStore;
use ditto_capability::{CapabilityDeriver, CapabilityRevision};
use ditto_context::{
    ContextCandidate, ContextCapsule, ContextLens, ContextNode, ContextNodeKind, ContextOrigin,
    ContextScope, EpistemicStatus,
};
use ditto_event_store::EventStore;
use ditto_kernel::turn::TurnFailureEvidence;
use ditto_kernel::{
    ArtifactReadTurnReplay, ArtifactReadTurnStatus, ArtifactWriteContext, DittoKernel,
    ExecutionOutputPayload, KernelConfig, KernelError, ModelOutputPayload, ReadOnlyTurnControl,
    TurnFailureCode, TurnRunError, replay_artifact_read_turn,
};
use ditto_model::{
    CancellationToken, ContentPart, ConversationItem, DriverDescriptor, DriverId, FailureKind,
    FinishReason, ModelDriver, ModelEvent, ModelEventStream, ModelFeature, ModelRequest,
    ModelStreamEvent, ParallelToolCalls, ProviderCallId, ProviderWarning, ReasoningItemId,
    RequestCapabilities, ToolChoice, ToolChoiceKind,
};
use ditto_protocol::{
    EventActor, EventQuery, EventRecord, NewEvent, SubmitInputCommand, event_kind,
};
use serde_json::{Value, json};
use tempfile::TempDir;

#[derive(Clone)]
struct ScriptedDriver {
    descriptor: DriverDescriptor,
    scripts: Arc<Mutex<VecDeque<Vec<ModelEvent>>>>,
    requests: Arc<Mutex<Vec<ModelRequest>>>,
}

impl ScriptedDriver {
    fn new(scripts: Vec<Vec<ModelEvent>>) -> Self {
        Self {
            descriptor: DriverDescriptor {
                id: DriverId::new("task003-scripted").expect("driver id"),
                request_capabilities: RequestCapabilities {
                    tool_choices: [ToolChoiceKind::Required, ToolChoiceKind::Auto]
                        .into_iter()
                        .collect(),
                    parallel_tool_calls: [ParallelToolCalls::Forbid].into_iter().collect(),
                    ..RequestCapabilities::default()
                },
                emitted_features: [ModelFeature::Text, ModelFeature::ToolCalls]
                    .into_iter()
                    .collect(),
            },
            scripts: Arc::new(Mutex::new(scripts.into())),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn requests(&self) -> Vec<ModelRequest> {
        self.requests.lock().expect("request lock").clone()
    }
}

impl ModelDriver for ScriptedDriver {
    fn descriptor(&self) -> &DriverDescriptor {
        &self.descriptor
    }

    fn stream(&self, request: ModelRequest, cancellation: CancellationToken) -> ModelEventStream {
        self.requests.lock().expect("request lock").push(request);
        let script = self
            .scripts
            .lock()
            .expect("script lock")
            .pop_front()
            .unwrap_or_default();
        ModelEventStream::new(stream! {
            for event in script {
                tokio::task::yield_now().await;
                if cancellation.is_cancelled() {
                    yield ModelEvent::Failed {
                        failure: ditto_model::ModelFailure::new(
                            FailureKind::Cancelled,
                            "script observed cancellation",
                        ),
                    };
                    return;
                }
                yield event;
            }
        })
    }
}

#[derive(Clone, Copy)]
enum OutputBudgetCase {
    EventExact,
    EventOverflow,
    RequestExact,
    RequestOverflow,
}

struct OutputBudgetDriver {
    descriptor: DriverDescriptor,
    case: OutputBudgetCase,
}

struct DelayedDriver {
    descriptor: DriverDescriptor,
    delay: std::time::Duration,
}

impl DelayedDriver {
    fn new(delay: std::time::Duration) -> Self {
        Self {
            descriptor: ScriptedDriver::new(Vec::new()).descriptor,
            delay,
        }
    }
}

impl ModelDriver for DelayedDriver {
    fn descriptor(&self) -> &DriverDescriptor {
        &self.descriptor
    }

    fn stream(&self, _request: ModelRequest, _cancellation: CancellationToken) -> ModelEventStream {
        let delay = self.delay;
        ModelEventStream::new(stream! {
            tokio::time::sleep(delay).await;
            yield ModelEvent::TextDelta { text: "late".into() };
        })
    }
}

impl OutputBudgetDriver {
    fn new(case: OutputBudgetCase) -> Self {
        Self {
            descriptor: ScriptedDriver::new(Vec::new()).descriptor,
            case,
        }
    }
}

impl ModelDriver for OutputBudgetDriver {
    fn descriptor(&self) -> &DriverDescriptor {
        &self.descriptor
    }

    fn stream(&self, request: ModelRequest, _cancellation: CancellationToken) -> ModelEventStream {
        let turn_id = request
            .control
            .cancellation_id
            .as_ref()
            .expect("turn cancellation id")
            .as_str()
            .to_owned();
        let request_id = request.request_id.clone();
        let encoded_len = |sequence, event: &ModelEvent| {
            serde_json::to_vec(&ModelOutputPayload {
                event_version: ditto_kernel::turn::TURN_PAYLOAD_VERSION,
                turn_id: turn_id.clone(),
                request_index: 0,
                request_id: request_id.clone(),
                admitted_at: chrono::DateTime::from_timestamp_millis(1_800_000_000_123)
                    .expect("fixed admitted timestamp"),
                stream_event: ModelStreamEvent::new(sequence, event.clone()),
            })
            .expect("encode model output fixture")
            .len()
        };
        let warning_at_size = |sequence: u64, desired_size: usize| {
            let mut event = ModelEvent::ProviderWarning {
                warning: ProviderWarning {
                    code: None,
                    message: "x".into(),
                },
            };
            let base_size = encoded_len(sequence, &event);
            let ModelEvent::ProviderWarning { warning } = &mut event else {
                unreachable!("warning fixture")
            };
            warning.message = "x".repeat(
                desired_size
                    .checked_sub(base_size)
                    .expect("desired output size exceeds envelope")
                    + 1,
            );
            assert_eq!(encoded_len(sequence, &event), desired_size);
            event
        };

        let events = match self.case {
            OutputBudgetCase::EventExact | OutputBudgetCase::EventOverflow => {
                let overflow = matches!(self.case, OutputBudgetCase::EventOverflow) as usize;
                vec![
                    warning_at_size(
                        0,
                        ditto_kernel::turn::MAX_MODEL_OUTPUT_EVENT_BYTES + overflow,
                    ),
                    ModelEvent::Completed {
                        finish_reason: FinishReason::EndTurn,
                        continuation: None,
                    },
                ]
            }
            OutputBudgetCase::RequestExact | OutputBudgetCase::RequestOverflow => {
                const WARNING_COUNT: usize = 14;
                const FIRST_WARNING_SIZE: usize = 300_000;
                let completed = ModelEvent::Completed {
                    finish_reason: FinishReason::EndTurn,
                    continuation: None,
                };
                let completed_size = encoded_len(WARNING_COUNT as u64, &completed);
                let overflow = matches!(self.case, OutputBudgetCase::RequestOverflow) as usize;
                let mut events = (0..WARNING_COUNT - 1)
                    .map(|sequence| warning_at_size(sequence as u64, FIRST_WARNING_SIZE))
                    .collect::<Vec<_>>();
                let used = (WARNING_COUNT - 1) * FIRST_WARNING_SIZE;
                let last_size =
                    ditto_kernel::turn::MAX_MODEL_OUTPUT_BYTES_PER_REQUEST - completed_size - used
                        + overflow;
                events.push(warning_at_size((WARNING_COUNT - 1) as u64, last_size));
                events.push(completed);
                events
            }
        };
        ModelEventStream::new(stream! {
            for event in events {
                yield event;
            }
        })
    }
}

struct Fixture {
    _directory: TempDir,
    config: KernelConfig,
    kernel: DittoKernel,
}

struct AcceptanceRelativeCandidates {
    kernel: DittoKernel,
    source_event_id: String,
}

impl IntoIterator for AcceptanceRelativeCandidates {
    type Item = ContextCandidate;
    type IntoIter = std::vec::IntoIter<ContextCandidate>;

    fn into_iter(self) -> Self::IntoIter {
        let accepted = all_task_events(&self.kernel, "task-1")
            .into_iter()
            .rev()
            .find(|event| {
                event.kind == event_kind::INPUT_RECEIVED
                    && event
                        .correlation_id
                        .as_deref()
                        .is_some_and(|correlation| correlation.starts_with("turn_"))
            })
            .expect("accepted turn input");
        let node = ContextNode {
            id: "submillisecond-context".into(),
            kind: ContextNodeKind::Resource,
            summary: "read the artifact".into(),
            origin: ContextOrigin::User,
            epistemic: EpistemicStatus::Asserted,
            scope: ContextScope::Task,
            lens: ContextLens::Task,
            confidence: 1.0,
            source_event_ids: vec![self.source_event_id],
            supersedes: Vec::new(),
            valid_from: None,
            valid_until: Some(accepted.recorded_at + ChronoDuration::microseconds(500)),
        };
        vec![ContextCandidate::user_pinned(node)].into_iter()
    }
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("temporary directory");
        let capabilities =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../capabilities");
        let config = KernelConfig::new(directory.path().join("data"), capabilities);
        let kernel = DittoKernel::open(config.clone()).expect("open kernel");
        Self {
            _directory: directory,
            config,
            kernel,
        }
    }

    fn store(&self, bytes: &[u8], session: &str, task: Option<&str>) -> String {
        self.kernel
            .store_artifact(
                bytes,
                ArtifactWriteContext {
                    session_id: Some(session.into()),
                    task_id: task.map(str::to_owned),
                    mime: Some("application/octet-stream".into()),
                    purpose: Some("turn test".into()),
                    ..ArtifactWriteContext::default()
                },
            )
            .expect("store artifact")
            .metadata
            .reference
            .to_string()
    }

    fn events_for_task(&self, task: &str) -> Vec<EventRecord> {
        all_task_events(&self.kernel, task)
    }

    fn events_for_session(&self, session: &str) -> Vec<EventRecord> {
        all_session_events(&self.kernel, session)
    }
}

fn all_task_events(kernel: &DittoKernel, task: &str) -> Vec<EventRecord> {
    let high_water = kernel.latest_event_seq().expect("high water");
    let mut after_seq = None;
    let mut events = Vec::new();
    loop {
        let page = kernel
            .list_events_through(
                &EventQuery {
                    after_seq,
                    limit: Some(1_000),
                    task_id: Some(task.into()),
                    ..EventQuery::default()
                },
                high_water,
            )
            .expect("list task events");
        if page.is_empty() {
            break;
        }
        after_seq = page.last().map(|event| event.seq);
        let done = page.len() < 1_000 || after_seq.is_some_and(|seq| seq >= high_water);
        events.extend(page);
        if done {
            break;
        }
    }
    events
}

fn all_session_events(kernel: &DittoKernel, session: &str) -> Vec<EventRecord> {
    let high_water = kernel.latest_event_seq().expect("high water");
    let mut after_seq = None;
    let mut events = Vec::new();
    loop {
        let page = kernel
            .list_events_through(
                &EventQuery {
                    after_seq,
                    limit: Some(1_000),
                    session_id: Some(session.into()),
                    ..EventQuery::default()
                },
                high_water,
            )
            .expect("list session events");
        if page.is_empty() {
            break;
        }
        after_seq = page.last().map(|event| event.seq);
        let done = page.len() < 1_000 || after_seq.is_some_and(|seq| seq >= high_water);
        events.extend(page);
        if done {
            break;
        }
    }
    events
}

fn turn_events(events: &[EventRecord], turn_id: &str) -> Vec<EventRecord> {
    events
        .iter()
        .filter(|event| event.correlation_id.as_deref() == Some(turn_id))
        .cloned()
        .collect()
}

fn terminal_recorded_before_deadline(events: &mut [EventRecord], deadline: chrono::DateTime<Utc>) {
    events
        .iter_mut()
        .find(|event| event.kind == event_kind::TURN_FAILED)
        .expect("turn failure")
        .recorded_at = deadline - ChronoDuration::milliseconds(2);
}

fn attach_deadline_evidence(event: &mut EventRecord, deadline: chrono::DateTime<Utc>) {
    event.payload["failure"]["evidence"] = json!({
        "type": "deadline",
        "deadline": deadline.timestamp_millis(),
    });
}

fn refresh_context_token_accounting(event: &mut EventRecord) {
    let capsule: ContextCapsule = serde_json::from_value(event.payload["capsule"].clone())
        .expect("decode mutated context capsule");
    let item_costs = capsule
        .nodes
        .iter()
        .map(ditto_context::ContextCapsuleItem::token_cost)
        .collect::<Vec<_>>();
    for (index, cost) in item_costs.iter().enumerate() {
        event.payload["compiled"]["receipt"]["included"][index]["token_cost"] = json!(cost);
    }
    let total = item_costs.iter().copied().sum::<u32>();
    event.payload["compiled"]["receipt"]["total_token_cost"] = json!(total);
    let soft_budget = event.payload["compiled"]["receipt"]["token_budget"]
        .as_u64()
        .expect("soft budget");
    event.payload["compiled"]["receipt"]["over_soft_budget"] =
        json!(u64::from(total) > soft_budget);
}

fn call_id(value: &str) -> ProviderCallId {
    ProviderCallId::new(value).expect("call id")
}

fn artifact_arguments(reference: &str, offset: i64, length: usize) -> Value {
    json!({"reference": reference, "offset": offset, "length": length})
}

fn tool_request_script(
    id: &str,
    capability_id: &str,
    arguments: Value,
    finish_reason: FinishReason,
) -> Vec<ModelEvent> {
    let id = call_id(id);
    vec![
        ModelEvent::ToolCallStarted {
            call_id: id.clone(),
            capability_id: capability_id.into(),
        },
        ModelEvent::ToolCallArgumentDelta {
            call_id: id.clone(),
            delta: serde_json::to_string(&arguments).expect("arguments JSON"),
        },
        ModelEvent::ToolCallReady {
            call_id: id,
            capability_id: capability_id.into(),
            arguments,
        },
        ModelEvent::Completed {
            finish_reason,
            continuation: None,
        },
    ]
}

fn final_script(text_parts: &[&str]) -> Vec<ModelEvent> {
    let mut events = text_parts
        .iter()
        .map(|text| ModelEvent::TextDelta {
            text: (*text).into(),
        })
        .collect::<Vec<_>>();
    events.push(ModelEvent::Completed {
        finish_reason: FinishReason::EndTurn,
        continuation: None,
    });
    events
}

fn command(session: &str, task: &str) -> SubmitInputCommand {
    SubmitInputCommand {
        text: "read the artifact".into(),
        session_id: Some(session.into()),
        task_id: Some(task.into()),
    }
}

async fn run_success(
    fixture: &Fixture,
    reference: &str,
    final_parts: &[&str],
) -> (ditto_kernel::ArtifactReadTurnOutcome, ScriptedDriver) {
    let arguments = artifact_arguments(reference, 1, 4);
    let driver = ScriptedDriver::new(vec![
        tool_request_script(
            "call-1",
            ARTIFACT_READ_ID,
            arguments,
            FinishReason::ToolCalls,
        ),
        final_script(final_parts),
    ]);
    let outcome = fixture
        .kernel
        .run_artifact_read_turn(
            command("session-1", "task-1"),
            Vec::new(),
            &driver,
            CancellationToken::new(),
            ReadOnlyTurnControl::default(),
        )
        .await
        .expect("successful turn");
    (outcome, driver)
}

#[tokio::test]
async fn successful_two_request_continuation_persists_exact_epoch_schema_history_and_replays() {
    let fixture = Fixture::new();
    let loaded = fixture.kernel.capability_load_metrics();
    assert_eq!(loaded.headers_read, 2);
    assert_eq!(loaded.legacy_manifests_read, 0);
    assert_eq!(loaded.manifests_paged, 0);
    let reference = fixture.store(b"abcdef", "session-1", Some("task-1"));
    let (outcome, driver) = run_success(&fixture, &reference, &["read ", "complete"]).await;
    assert_eq!(fixture.kernel.capability_load_metrics().manifests_paged, 1);
    assert_eq!(
        fixture
            .kernel
            .capability_load_metrics()
            .retained_header_bytes,
        loaded.retained_header_bytes
    );

    assert_eq!(outcome.response, "read complete");
    assert_eq!(outcome.status, ArtifactReadTurnStatus::Unverified);
    assert_eq!(outcome.request_count, 2);
    assert_eq!(outcome.tool_call_count, 1);

    let requests = driver.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].execution_epoch_id,
        requests[1].execution_epoch_id
    );
    assert_eq!(requests[0].tools, requests[1].tools);
    assert_eq!(requests[0].tools, vec![capability_schema()]);
    assert_eq!(
        requests[0].stable_system_prefix,
        requests[1].stable_system_prefix
    );
    assert_eq!(requests[0].generation.tool_use.choice, ToolChoice::Required);
    assert_eq!(requests[1].generation.tool_use.choice, ToolChoice::Auto);
    assert!(requests.iter().all(|request| {
        request.generation.tool_use.parallel_calls == ParallelToolCalls::Forbid
    }));
    assert_eq!(requests[0].control.deadline, requests[1].control.deadline);
    assert!(requests[0].control.deadline.is_some());
    assert_eq!(requests[1].turn.conversation.len(), 3);
    let ConversationItem::ToolResult {
        call_id: result_call_id,
        content,
        is_error,
    } = &requests[1].turn.conversation[2]
    else {
        panic!("third continuation item must be the tool result")
    };
    assert_eq!(result_call_id.as_str(), "call-1");
    assert!(!is_error);
    let [ContentPart::Structured { value }] = content.as_slice() else {
        panic!("tool result must be one structured projection")
    };
    assert_eq!(value["reference"], reference);
    assert_eq!(value["offset"], 1);
    assert_eq!(value["requested_bytes"], 4);
    assert_eq!(value["returned_bytes"], 4);
    assert_eq!(value["data"], "YmNkZQ==");
    assert_eq!(value["is_error"], false);

    let events = fixture.events_for_session("session-1");
    assert!(
        !events
            .iter()
            .any(|event| event.kind == event_kind::TASK_COMPLETED)
    );
    let replay = replay_artifact_read_turn(&events, &outcome.turn_id)
        .expect("replay complete turn from task snapshot");
    assert_eq!(
        replay.terminal,
        ArtifactReadTurnReplay::Finished {
            outcome: outcome.clone()
        }
    );
    assert_eq!(replay.requests.len(), 2);
    assert_eq!(replay.outputs.len(), 7);
    assert_eq!(replay.calls.len(), 1);
    assert!(replay.calls[0].output.is_some());
    let selected = replay.capabilities.as_ref().expect("selected capability");
    let expected_revision = CapabilityRevision::from_contract(
        &selected.manifest,
        &selected.schemas[0],
        ArtifactReadDeriver::default().revision().clone(),
    )
    .expect("exact artifact revision");
    assert_eq!(selected.epoch.invocation_revisions(), [expected_revision]);
    assert_eq!(replay.sequence_span.first_seq, events[1].seq);
    assert_eq!(
        replay.sequence_span.last_seq,
        events.last().expect("turn terminal").seq
    );

    drop(fixture.kernel.clone());
    let reopened = DittoKernel::open(fixture.config.clone()).expect("reopen kernel");
    let reopened_events = all_session_events(&reopened, "session-1");
    assert_eq!(
        replay_artifact_read_turn(&reopened_events, &outcome.turn_id)
            .expect("replay after reopen")
            .terminal,
        ArtifactReadTurnReplay::Finished { outcome }
    );
}

#[tokio::test]
async fn changed_selected_package_fails_before_model_and_replays_without_package_io() {
    let directory = tempfile::tempdir().unwrap();
    let capabilities = directory.path().join("capabilities");
    std::fs::create_dir(&capabilities).unwrap();
    let body = include_str!("../../../capabilities/core/artifact-read/capability.toml");
    std::fs::write(capabilities.join("capability.toml"), body).unwrap();
    std::fs::write(
        capabilities.join(ditto_capability::PACKAGE_HEADER_FILENAME),
        ditto_capability::CapabilityHeader::from_manifest_bytes(body.as_bytes())
            .unwrap()
            .to_json()
            .unwrap(),
    )
    .unwrap();
    let config = KernelConfig::new(directory.path().join("data"), &capabilities);
    let kernel = DittoKernel::open(config).unwrap();
    std::fs::write(
        capabilities.join("capability.toml"),
        format!("{body}\n# changed"),
    )
    .unwrap();
    let driver = ScriptedDriver::new(Vec::new());
    let error = kernel
        .run_artifact_read_turn(
            command("session-1", "task-1"),
            Vec::new(),
            &driver,
            CancellationToken::new(),
            ReadOnlyTurnControl::default(),
        )
        .await
        .unwrap_err();
    let TurnRunError::Failed(failure) = error else {
        panic!("expected durable package failure")
    };
    assert_eq!(failure.code, TurnFailureCode::CapabilityContract);
    assert_eq!(
        failure.message,
        "installed artifact.read package could not be verified"
    );
    assert!(driver.requests().is_empty());
    let snapshot = all_session_events(&kernel, "session-1");
    assert!(!snapshot.iter().any(|event| matches!(
        event.kind.as_str(),
        event_kind::MODEL_REQUESTED | event_kind::EXECUTION_STARTED | event_kind::TASK_COMPLETED
    )));
    std::fs::remove_dir_all(capabilities).unwrap();
    assert_eq!(
        replay_artifact_read_turn(&snapshot, &failure.turn_id)
            .unwrap()
            .terminal,
        ArtifactReadTurnReplay::Failed {
            failure: (*failure).clone()
        }
    );
    let mut forged = snapshot;
    forged.last_mut().unwrap().payload["failure"]["message"] = json!("some other package failure");
    assert!(replay_artifact_read_turn(&forged, &failure.turn_id).is_err());
}

#[tokio::test]
async fn adjacent_text_chunks_coalesce_before_the_continuation_request_and_replay() {
    let fixture = Fixture::new();
    let reference = fixture.store(b"abcdef", "session-1", Some("task-1"));
    let mut first = tool_request_script(
        "call-1",
        ARTIFACT_READ_ID,
        artifact_arguments(&reference, 0, 1),
        FinishReason::ToolCalls,
    );
    first.insert(0, ModelEvent::TextDelta { text: "pre".into() });
    first.insert(
        1,
        ModelEvent::TextDelta {
            text: "face".into(),
        },
    );
    let driver = ScriptedDriver::new(vec![first, final_script(&["done"])]);
    let outcome = fixture
        .kernel
        .run_artifact_read_turn(
            command("session-1", "task-1"),
            Vec::new(),
            &driver,
            CancellationToken::new(),
            ReadOnlyTurnControl::default(),
        )
        .await
        .expect("chunked text turn");
    let requests = driver.requests();
    assert_eq!(requests[1].turn.conversation.len(), 4);
    assert_eq!(
        requests[1].turn.conversation[1],
        ConversationItem::Message {
            role: ditto_model::MessageRole::Assistant,
            content: vec![ContentPart::Text {
                text: "preface".into()
            }],
        }
    );
    let replay =
        replay_artifact_read_turn(&fixture.events_for_session("session-1"), &outcome.turn_id)
            .expect("chunked text replay");
    assert_eq!(replay.requests.len(), 2);
}

#[tokio::test]
async fn malformed_negative_and_excessive_arguments_are_error_results_and_continue_without_read() {
    for (case, arguments, expected_code) in [
        (
            "malformed",
            json!({"reference": "../../state.db", "offset": 0, "length": 1}),
            "invalid_reference",
        ),
        (
            "negative",
            json!({"reference": format!("artifact:sha256:{}", "a".repeat(64)), "offset": -1, "length": 1}),
            "invalid_arguments",
        ),
        (
            "fractional-integer-spelling",
            json!({"reference": format!("artifact:sha256:{}", "a".repeat(64)), "offset": 0, "length": 1.0}),
            "invalid_arguments",
        ),
        (
            "excessive",
            json!({"reference": format!("artifact:sha256:{}", "a".repeat(64)), "offset": 0, "length": MAX_READ_BYTES + 1}),
            "invalid_arguments",
        ),
    ] {
        let fixture = Fixture::new();
        let driver = ScriptedDriver::new(vec![
            tool_request_script(
                "call-1",
                ARTIFACT_READ_ID,
                arguments,
                FinishReason::ToolCalls,
            ),
            final_script(&["explained error"]),
        ]);
        let outcome = fixture
            .kernel
            .run_artifact_read_turn(
                command("session-1", "task-1"),
                Vec::new(),
                &driver,
                CancellationToken::new(),
                ReadOnlyTurnControl::default(),
            )
            .await
            .unwrap_or_else(|error| panic!("{case} should continue: {error}"));
        let events = turn_events(&fixture.events_for_task("task-1"), &outcome.turn_id);
        let output = events
            .iter()
            .find(|event| event.kind == event_kind::EXECUTION_OUTPUT)
            .expect("execution output");
        let payload: ExecutionOutputPayload =
            serde_json::from_value(output.payload.clone()).expect("typed output");
        assert!(payload.result.is_error(), "{case}");
        assert_eq!(
            payload
                .result
                .error_projection()
                .expect("error projection")
                .code(),
            expected_code,
            "{case}"
        );
        assert_eq!(driver.requests().len(), 2, "{case}");
    }
}

#[tokio::test]
async fn scope_authorization_never_falls_back_across_tasks_in_the_same_session() {
    let fixture = Fixture::new();
    let reference = fixture.store(b"secret", "session-1", Some("other-task"));
    let (outcome, _) = run_success(&fixture, &reference, &["not authorized"]).await;
    let events = turn_events(&fixture.events_for_task("task-1"), &outcome.turn_id);
    let output: ExecutionOutputPayload = serde_json::from_value(
        events
            .iter()
            .find(|event| event.kind == event_kind::EXECUTION_OUTPUT)
            .expect("execution output")
            .payload
            .clone(),
    )
    .expect("typed output");
    assert_eq!(
        output
            .result
            .error_projection()
            .expect("authorization error")
            .code(),
        "unauthorized_reference"
    );
}

#[tokio::test]
async fn scope_authorization_requires_exact_session_and_allows_session_root_without_task() {
    let fixture = Fixture::new();
    let reference = fixture.store(b"secret", "other-session", None);
    let (outcome, _) = run_success(&fixture, &reference, &["not authorized"]).await;
    let output: ExecutionOutputPayload = serde_json::from_value(
        turn_events(&fixture.events_for_task("task-1"), &outcome.turn_id)
            .iter()
            .find(|event| event.kind == event_kind::EXECUTION_OUTPUT)
            .expect("execution output")
            .payload
            .clone(),
    )
    .expect("typed output");
    assert_eq!(
        output
            .result
            .error_projection()
            .expect("session denial")
            .code(),
        "unauthorized_reference"
    );

    let fixture = Fixture::new();
    let reference = fixture.store(b"abcdef", "session-1", None);
    let (outcome, driver) = run_success(&fixture, &reference, &["session root works"]).await;
    assert_eq!(outcome.status, ArtifactReadTurnStatus::Unverified);
    let requests = driver.requests();
    let ConversationItem::ToolResult { is_error, .. } = &requests[1].turn.conversation[2] else {
        panic!("continuation tool result")
    };
    assert!(!is_error);
}

#[tokio::test]
async fn forged_artifact_created_actor_never_authorizes_a_read() {
    let fixture = Fixture::new();
    let artifacts =
        ArtifactStore::open(fixture.config.data_dir.join("artifacts")).expect("open object store");
    let reference = artifacts
        .put(b"abcdef")
        .expect("put unrooted object")
        .reference
        .to_string();
    EventStore::open(fixture.config.data_dir.join("state.db"))
        .expect("open event store")
        .append(NewEvent {
            session_id: Some("session-1".into()),
            task_id: Some("task-1".into()),
            actor: EventActor::Model,
            kind: event_kind::ARTIFACT_CREATED.into(),
            payload: json!({"reference": reference}),
            causation_id: None,
            correlation_id: None,
            span_id: None,
        })
        .expect("append forged root fixture");

    let (outcome, driver) = run_success(&fixture, &reference, &["denied"]).await;
    let requests = driver.requests();
    let ConversationItem::ToolResult {
        is_error, content, ..
    } = &requests[1].turn.conversation[2]
    else {
        panic!("continuation tool result")
    };
    assert!(is_error);
    let [ContentPart::Structured { value }] = content.as_slice() else {
        panic!("structured denial")
    };
    assert_eq!(value["error"]["code"], "unauthorized_reference");
    let replay =
        replay_artifact_read_turn(&fixture.events_for_session("session-1"), &outcome.turn_id)
            .expect("forged actor denial replays");
    assert!(matches!(
        replay.terminal,
        ArtifactReadTurnReplay::Finished { .. }
    ));
}

#[tokio::test]
async fn omitted_task_id_is_generated_and_scopes_the_entire_turn() {
    let fixture = Fixture::new();
    let reference = fixture.store(b"abcdef", "session-1", None);
    let arguments = artifact_arguments(&reference, 0, 1);
    let driver = ScriptedDriver::new(vec![
        tool_request_script(
            "call-1",
            ARTIFACT_READ_ID,
            arguments,
            FinishReason::ToolCalls,
        ),
        final_script(&["done"]),
    ]);
    let outcome = fixture
        .kernel
        .run_artifact_read_turn(
            SubmitInputCommand {
                text: "read the artifact".into(),
                session_id: Some("session-1".into()),
                task_id: None,
            },
            Vec::new(),
            &driver,
            CancellationToken::new(),
            ReadOnlyTurnControl::default(),
        )
        .await
        .expect("generated task turn");
    assert!(outcome.task_id.starts_with("task_"));
    let events = fixture.events_for_task(&outcome.task_id);
    assert!(!events.is_empty());
    assert!(
        events
            .iter()
            .all(|event| event.task_id.as_deref() == Some(outcome.task_id.as_str()))
    );
}

#[tokio::test]
async fn a_preexisting_task_completion_rejects_turn_admission_without_new_events() {
    let fixture = Fixture::new();
    EventStore::open(fixture.config.data_dir.join("state.db"))
        .expect("open event store")
        .append(NewEvent {
            session_id: Some("session-1".into()),
            task_id: Some("task-1".into()),
            actor: EventActor::System,
            kind: event_kind::TASK_COMPLETED.into(),
            payload: json!({"verified": true}),
            causation_id: None,
            correlation_id: Some("prior-turn".into()),
            span_id: None,
        })
        .expect("append prior completion");
    let count_before = fixture.kernel.event_count().expect("event count");
    let error = fixture
        .kernel
        .run_artifact_read_turn(
            command("session-1", "task-1"),
            Vec::new(),
            &ScriptedDriver::new(Vec::new()),
            CancellationToken::new(),
            ReadOnlyTurnControl::default(),
        )
        .await
        .expect_err("completed task admission");
    assert!(matches!(
        error,
        TurnRunError::Kernel(KernelError::InvalidCommand(ref message))
            if message == "task task-1 is already completed"
    ));
    assert_eq!(
        fixture.kernel.event_count().expect("event count"),
        count_before
    );

    let reference = fixture.store(b"abcdef", "session-1", None);
    let driver = ScriptedDriver::new(vec![
        tool_request_script(
            "call-1",
            ARTIFACT_READ_ID,
            artifact_arguments(&reference, 0, 1),
            FinishReason::ToolCalls,
        ),
        final_script(&["other task done"]),
    ]);
    let outcome = fixture
        .kernel
        .run_artifact_read_turn(
            command("session-1", "task-2"),
            Vec::new(),
            &driver,
            CancellationToken::new(),
            ReadOnlyTurnControl::default(),
        )
        .await
        .expect("other task remains admissible");
    replay_artifact_read_turn(&fixture.events_for_session("session-1"), &outcome.turn_id)
        .expect("other-task completion is irrelevant to replay");
}

#[tokio::test]
async fn artifact_integrity_failure_is_a_structured_error_and_continues() {
    let fixture = Fixture::new();
    let reference = fixture.store(b"abcdef", "session-1", Some("task-1"));
    let digest = reference.strip_prefix("artifact:sha256:").expect("digest");
    fs::write(
        fixture
            .config
            .data_dir
            .join("artifacts/sha256")
            .join(digest),
        b"tampered",
    )
    .expect("tamper artifact object");
    let (outcome, driver) = run_success(&fixture, &reference, &["integrity failed"]).await;
    let events = turn_events(&fixture.events_for_task("task-1"), &outcome.turn_id);
    let output: ExecutionOutputPayload = serde_json::from_value(
        events
            .iter()
            .find(|event| event.kind == event_kind::EXECUTION_OUTPUT)
            .expect("execution output")
            .payload
            .clone(),
    )
    .expect("typed output");
    assert_eq!(
        output
            .result
            .error_projection()
            .expect("integrity error")
            .code(),
        "integrity_failure"
    );
    assert_eq!(driver.requests().len(), 2);
}

#[tokio::test]
async fn ready_call_with_non_tool_finish_and_multiple_ready_calls_fail_before_execution() {
    let fixture = Fixture::new();
    let reference = fixture.store(b"abcdef", "session-1", Some("task-1"));
    let arguments = artifact_arguments(&reference, 0, 1);
    let driver = ScriptedDriver::new(vec![tool_request_script(
        "call-1",
        ARTIFACT_READ_ID,
        arguments,
        FinishReason::EndTurn,
    )]);
    let error = fixture
        .kernel
        .run_artifact_read_turn(
            command("session-1", "task-1"),
            Vec::new(),
            &driver,
            CancellationToken::new(),
            ReadOnlyTurnControl::default(),
        )
        .await
        .expect_err("ready call with non-tool finish must fail");
    let failure = match error {
        TurnRunError::Failed(failure) => *failure,
        other => panic!("expected durable protocol failure, got {other}"),
    };
    assert_eq!(failure.code, TurnFailureCode::Protocol);
    let failed_snapshot = fixture.events_for_task("task-1");
    assert_eq!(
        replay_artifact_read_turn(&failed_snapshot, &failure.turn_id)
            .expect("replay post-terminal failure")
            .terminal,
        ArtifactReadTurnReplay::Failed {
            failure: failure.clone()
        }
    );
    let mut contradictory = failed_snapshot;
    contradictory.last_mut().expect("turn.failed").payload["failure"]["message"] =
        json!("unrelated failure");
    assert!(replay_artifact_read_turn(&contradictory, &failure.turn_id).is_err());
    assert!(
        !fixture
            .events_for_task("task-1")
            .iter()
            .any(|event| event.kind == event_kind::EXECUTION_STARTED)
    );

    let fixture = Fixture::new();
    let reference = fixture.store(b"abcdef", "session-1", Some("task-1"));
    let args = artifact_arguments(&reference, 0, 1);
    let first = call_id("call-1");
    let second = call_id("call-2");
    let driver = ScriptedDriver::new(vec![vec![
        ModelEvent::ToolCallStarted {
            call_id: first.clone(),
            capability_id: ARTIFACT_READ_ID.into(),
        },
        ModelEvent::ToolCallArgumentDelta {
            call_id: first.clone(),
            delta: serde_json::to_string(&args).expect("args"),
        },
        ModelEvent::ToolCallReady {
            call_id: first,
            capability_id: ARTIFACT_READ_ID.into(),
            arguments: args.clone(),
        },
        ModelEvent::ToolCallStarted {
            call_id: second.clone(),
            capability_id: ARTIFACT_READ_ID.into(),
        },
        ModelEvent::ToolCallArgumentDelta {
            call_id: second.clone(),
            delta: serde_json::to_string(&args).expect("args"),
        },
        ModelEvent::ToolCallReady {
            call_id: second,
            capability_id: ARTIFACT_READ_ID.into(),
            arguments: args,
        },
        ModelEvent::Completed {
            finish_reason: FinishReason::ToolCalls,
            continuation: None,
        },
    ]]);
    let error = fixture
        .kernel
        .run_artifact_read_turn(
            command("session-1", "task-1"),
            Vec::new(),
            &driver,
            CancellationToken::new(),
            ReadOnlyTurnControl::default(),
        )
        .await
        .expect_err("parallel ready calls must fail");
    assert!(
        matches!(error, TurnRunError::Failed(ref failure) if failure.code == TurnFailureCode::Protocol)
    );
    assert!(
        !fixture
            .events_for_task("task-1")
            .iter()
            .any(|event| event.kind == event_kind::EXECUTION_STARTED)
    );
}

#[tokio::test]
async fn unknown_capability_unknown_call_and_duplicate_ids_fail_closed() {
    let fixture = Fixture::new();
    let driver = ScriptedDriver::new(vec![vec![ModelEvent::ToolCallStarted {
        call_id: call_id("call-1"),
        capability_id: "device.process.run".into(),
    }]]);
    let error = fixture
        .kernel
        .run_artifact_read_turn(
            command("session-1", "task-1"),
            Vec::new(),
            &driver,
            CancellationToken::new(),
            ReadOnlyTurnControl::default(),
        )
        .await
        .expect_err("unknown capability");
    assert!(
        matches!(error, TurnRunError::Failed(ref failure) if failure.code == TurnFailureCode::Protocol)
    );

    let fixture = Fixture::new();
    let driver = ScriptedDriver::new(vec![vec![ModelEvent::ToolCallArgumentDelta {
        call_id: call_id("unknown"),
        delta: "{}".into(),
    }]]);
    let error = fixture
        .kernel
        .run_artifact_read_turn(
            command("session-1", "task-1"),
            Vec::new(),
            &driver,
            CancellationToken::new(),
            ReadOnlyTurnControl::default(),
        )
        .await
        .expect_err("unknown call");
    assert!(
        matches!(error, TurnRunError::Failed(ref failure) if failure.code == TurnFailureCode::Protocol)
    );

    let fixture = Fixture::new();
    let reference = fixture.store(b"abcdef", "session-1", Some("task-1"));
    let arguments = artifact_arguments(&reference, 0, 1);
    let driver = ScriptedDriver::new(vec![
        tool_request_script(
            "same-call",
            ARTIFACT_READ_ID,
            arguments.clone(),
            FinishReason::ToolCalls,
        ),
        tool_request_script(
            "same-call",
            ARTIFACT_READ_ID,
            arguments,
            FinishReason::ToolCalls,
        ),
    ]);
    let error = fixture
        .kernel
        .run_artifact_read_turn(
            command("session-1", "task-1"),
            Vec::new(),
            &driver,
            CancellationToken::new(),
            ReadOnlyTurnControl::default(),
        )
        .await
        .expect_err("epoch-wide duplicate call id");
    assert!(
        matches!(error, TurnRunError::Failed(ref failure) if failure.code == TurnFailureCode::Protocol)
    );
    assert_eq!(
        fixture
            .events_for_task("task-1")
            .iter()
            .filter(|event| event.kind == event_kind::EXECUTION_OUTPUT)
            .count(),
        1
    );
}

#[tokio::test]
async fn cancellation_after_durable_execution_start_emits_no_result() {
    let fixture = Fixture::new();
    let reference = fixture.store(b"abcdef", "session-1", Some("task-1"));
    let arguments = artifact_arguments(&reference, 0, 1);
    let driver = Arc::new(ScriptedDriver::new(vec![tool_request_script(
        "call-1",
        ARTIFACT_READ_ID,
        arguments,
        FinishReason::ToolCalls,
    )]));
    let cancellation = CancellationToken::new();
    let mut events = fixture.kernel.subscribe();
    let kernel = fixture.kernel.clone();
    let driver_for_turn = driver.clone();
    let cancellation_for_turn = cancellation.clone();
    let handle = tokio::spawn(async move {
        kernel
            .run_artifact_read_turn(
                command("session-1", "task-1"),
                Vec::new(),
                driver_for_turn.as_ref(),
                cancellation_for_turn,
                ReadOnlyTurnControl::default(),
            )
            .await
    });
    loop {
        let event = events.recv().await.expect("turn event");
        if event.kind == event_kind::EXECUTION_STARTED {
            cancellation.cancel();
            break;
        }
    }
    let error = handle
        .await
        .expect("turn task")
        .expect_err("cancelled turn");
    let failure = match error {
        TurnRunError::Failed(failure) => failure,
        other => panic!("unexpected cancellation error: {other}"),
    };
    assert_eq!(failure.code, TurnFailureCode::Cancelled);
    let task_events = fixture.events_for_task("task-1");
    assert!(
        !task_events
            .iter()
            .any(|event| event.kind == event_kind::EXECUTION_OUTPUT)
    );
    assert_eq!(
        task_events.last().expect("terminal event").kind,
        event_kind::TURN_FAILED
    );
    let snapshot = fixture.events_for_session("session-1");
    replay_artifact_read_turn(&snapshot, &failure.turn_id)
        .expect("post-start cancellation must replay");

    let deadline: chrono::DateTime<Utc> = serde_json::from_value(
        snapshot
            .iter()
            .find(|event| {
                event.kind == event_kind::MODEL_REQUESTED
                    && event.correlation_id.as_deref() == Some(failure.turn_id.as_str())
            })
            .expect("model request")
            .payload["request"]["control"]["deadline"]
            .clone(),
    )
    .expect("request deadline");
    let mut deadline_trace = snapshot;
    let terminal = deadline_trace
        .iter_mut()
        .find(|event| {
            event.kind == event_kind::TURN_FAILED
                && event.correlation_id.as_deref() == Some(failure.turn_id.as_str())
        })
        .expect("turn failure");
    terminal.payload["failure"]["code"] = json!("deadline_exceeded");
    terminal.payload["failure"]["message"] =
        json!("turn deadline elapsed after execution started and before its result");
    attach_deadline_evidence(terminal, deadline);
    terminal.recorded_at = deadline;
    replay_artifact_read_turn(&deadline_trace, &failure.turn_id)
        .expect("temporally valid post-start deadline trace");
    deadline_trace
        .iter_mut()
        .find(|event| event.kind == event_kind::TURN_FAILED)
        .expect("turn failure")
        .recorded_at = deadline - ChronoDuration::milliseconds(2);
    assert!(replay_artifact_read_turn(&deadline_trace, &failure.turn_id).is_err());
}

#[tokio::test]
async fn cancellation_and_deadline_are_checked_before_capability_request() {
    let fixture = Fixture::new();
    let reference = fixture.store(b"abcdef", "session-1", Some("task-1"));
    let driver = Arc::new(ScriptedDriver::new(vec![tool_request_script(
        "call-1",
        ARTIFACT_READ_ID,
        artifact_arguments(&reference, 0, 1),
        FinishReason::ToolCalls,
    )]));
    let cancellation = CancellationToken::new();
    let mut events = fixture.kernel.subscribe();
    let kernel = fixture.kernel.clone();
    let driver_for_turn = driver.clone();
    let cancellation_for_turn = cancellation.clone();
    let handle = tokio::spawn(async move {
        kernel
            .run_artifact_read_turn(
                command("session-1", "task-1"),
                Vec::new(),
                driver_for_turn.as_ref(),
                cancellation_for_turn,
                ReadOnlyTurnControl::default(),
            )
            .await
    });
    loop {
        let event = events.recv().await.expect("turn event");
        if event.kind == event_kind::MODEL_OUTPUT {
            let output: ModelOutputPayload =
                serde_json::from_value(event.payload).expect("model output payload");
            if matches!(output.stream_event.event, ModelEvent::Completed { .. }) {
                cancellation.cancel();
                break;
            }
        }
    }
    let failure = match handle
        .await
        .expect("turn task")
        .expect_err("pre-capability cancellation")
    {
        TurnRunError::Failed(failure) => failure,
        other => panic!("unexpected cancellation error: {other}"),
    };
    assert_eq!(
        failure.message,
        "turn was cancelled before capability request"
    );
    let snapshot = fixture.events_for_session("session-1");
    assert!(
        !turn_events(&snapshot, &failure.turn_id)
            .iter()
            .any(|event| event.kind == event_kind::CAPABILITY_REQUESTED)
    );
    replay_artifact_read_turn(&snapshot, &failure.turn_id)
        .expect("pre-capability cancellation must replay");

    let deadline: chrono::DateTime<Utc> = serde_json::from_value(
        snapshot
            .iter()
            .find(|event| event.kind == event_kind::MODEL_REQUESTED)
            .expect("model request")
            .payload["request"]["control"]["deadline"]
            .clone(),
    )
    .expect("request deadline");
    let mut deadline_trace = snapshot;
    let terminal = deadline_trace
        .iter_mut()
        .find(|event| event.kind == event_kind::TURN_FAILED)
        .expect("turn failure");
    terminal.payload["failure"]["code"] = json!("deadline_exceeded");
    terminal.payload["failure"]["message"] =
        json!("turn deadline elapsed before capability execution");
    attach_deadline_evidence(terminal, deadline);
    terminal.recorded_at = deadline;
    replay_artifact_read_turn(&deadline_trace, &failure.turn_id)
        .expect("temporally valid pre-capability deadline trace");
    terminal_recorded_before_deadline(&mut deadline_trace, deadline);
    assert!(replay_artifact_read_turn(&deadline_trace, &failure.turn_id).is_err());
}

#[tokio::test]
async fn cancellation_and_deadline_are_checked_after_capability_request_before_start() {
    let fixture = Fixture::new();
    let reference = fixture.store(b"abcdef", "session-1", Some("task-1"));
    let driver = Arc::new(ScriptedDriver::new(vec![tool_request_script(
        "call-1",
        ARTIFACT_READ_ID,
        artifact_arguments(&reference, 0, 1),
        FinishReason::ToolCalls,
    )]));
    let cancellation = CancellationToken::new();
    let mut events = fixture.kernel.subscribe();
    let kernel = fixture.kernel.clone();
    let driver_for_turn = driver.clone();
    let cancellation_for_turn = cancellation.clone();
    let handle = tokio::spawn(async move {
        kernel
            .run_artifact_read_turn(
                command("session-1", "task-1"),
                Vec::new(),
                driver_for_turn.as_ref(),
                cancellation_for_turn,
                ReadOnlyTurnControl::default(),
            )
            .await
    });
    loop {
        let event = events.recv().await.expect("turn event");
        if event.kind == event_kind::CAPABILITY_REQUESTED {
            cancellation.cancel();
            break;
        }
    }
    let failure = match handle
        .await
        .expect("turn task")
        .expect_err("post-capability cancellation")
    {
        TurnRunError::Failed(failure) => failure,
        other => panic!("unexpected cancellation error: {other}"),
    };
    assert_eq!(
        failure.message,
        "turn was cancelled after capability request and before execution started"
    );
    let snapshot = fixture.events_for_session("session-1");
    assert!(
        !turn_events(&snapshot, &failure.turn_id)
            .iter()
            .any(|event| event.kind == event_kind::EXECUTION_STARTED)
    );
    let replay = replay_artifact_read_turn(&snapshot, &failure.turn_id)
        .expect("post-capability cancellation must replay");
    assert_eq!(replay.calls.len(), 1);
    assert!(replay.calls[0].started.is_none());

    let deadline: chrono::DateTime<Utc> = serde_json::from_value(
        snapshot
            .iter()
            .find(|event| event.kind == event_kind::MODEL_REQUESTED)
            .expect("model request")
            .payload["request"]["control"]["deadline"]
            .clone(),
    )
    .expect("request deadline");
    let mut deadline_trace = snapshot;
    let terminal = deadline_trace
        .iter_mut()
        .find(|event| event.kind == event_kind::TURN_FAILED)
        .expect("turn failure");
    terminal.payload["failure"]["code"] = json!("deadline_exceeded");
    terminal.payload["failure"]["message"] =
        json!("turn deadline elapsed after capability request and before execution started");
    attach_deadline_evidence(terminal, deadline);
    terminal.recorded_at = deadline;
    replay_artifact_read_turn(&deadline_trace, &failure.turn_id)
        .expect("temporally valid post-capability deadline trace");
    terminal_recorded_before_deadline(&mut deadline_trace, deadline);
    assert!(replay_artifact_read_turn(&deadline_trace, &failure.turn_id).is_err());
}

#[tokio::test]
async fn cancellation_after_model_request_stops_before_driver_and_stage_cannot_drift() {
    let fixture = Fixture::new();
    let driver = Arc::new(ScriptedDriver::new(vec![final_script(&["unused"])]));
    let cancellation = CancellationToken::new();
    let mut events = fixture.kernel.subscribe();
    let kernel = fixture.kernel.clone();
    let driver_for_turn = driver.clone();
    let cancellation_for_turn = cancellation.clone();
    let handle = tokio::spawn(async move {
        kernel
            .run_artifact_read_turn(
                command("session-1", "task-1"),
                Vec::new(),
                driver_for_turn.as_ref(),
                cancellation_for_turn,
                ReadOnlyTurnControl::default(),
            )
            .await
    });
    loop {
        let event = events.recv().await.expect("turn event");
        if event.kind == event_kind::MODEL_REQUESTED {
            cancellation.cancel();
            break;
        }
    }
    let failure = match handle
        .await
        .expect("turn task")
        .expect_err("post-request cancellation")
    {
        TurnRunError::Failed(failure) => failure,
        other => panic!("unexpected cancellation error: {other}"),
    };
    assert_eq!(
        failure.message,
        "turn was cancelled after persisting a model request and before driver invocation"
    );
    assert!(driver.requests().is_empty());
    replay_artifact_read_turn(&fixture.events_for_session("session-1"), &failure.turn_id)
        .expect("post-request cancellation must replay");

    let fixture = Fixture::new();
    let driver = Arc::new(ScriptedDriver::new(vec![vec![
        ModelEvent::ProviderWarning {
            warning: ProviderWarning {
                code: None,
                message: "first output".into(),
            },
        },
        ModelEvent::ProviderWarning {
            warning: ProviderWarning {
                code: None,
                message: "unused".into(),
            },
        },
    ]]));
    let cancellation = CancellationToken::new();
    let mut events = fixture.kernel.subscribe();
    let kernel = fixture.kernel.clone();
    let driver_for_turn = driver.clone();
    let cancellation_for_turn = cancellation.clone();
    let handle = tokio::spawn(async move {
        kernel
            .run_artifact_read_turn(
                command("session-1", "task-1"),
                Vec::new(),
                driver_for_turn.as_ref(),
                cancellation_for_turn,
                ReadOnlyTurnControl::default(),
            )
            .await
    });
    loop {
        let event = events.recv().await.expect("turn event");
        if event.kind == event_kind::MODEL_OUTPUT {
            cancellation.cancel();
            break;
        }
    }
    let failure = match handle
        .await
        .expect("turn task")
        .expect_err("awaiting-output cancellation")
    {
        TurnRunError::Failed(failure) => failure,
        other => panic!("unexpected cancellation error: {other}"),
    };
    assert_eq!(
        failure.message,
        "turn was cancelled while awaiting model output"
    );
    let mut drifted = fixture.events_for_session("session-1");
    drifted
        .iter_mut()
        .find(|event| event.kind == event_kind::TURN_FAILED)
        .expect("turn failure")
        .payload["failure"]["message"] =
        json!("turn was cancelled after persisting a model request and before driver invocation");
    assert!(replay_artifact_read_turn(&drifted, &failure.turn_id).is_err());
}

#[tokio::test]
async fn bounded_deadline_is_identical_across_requests_and_an_expired_deadline_fails() {
    let fixture = Fixture::new();
    let reference = fixture.store(b"abcdef", "session-1", Some("task-1"));
    let requested_deadline = Utc::now() + ChronoDuration::seconds(30);
    let arguments = artifact_arguments(&reference, 0, 1);
    let driver = ScriptedDriver::new(vec![
        tool_request_script(
            "call-1",
            ARTIFACT_READ_ID,
            arguments,
            FinishReason::ToolCalls,
        ),
        final_script(&["done"]),
    ]);
    fixture
        .kernel
        .run_artifact_read_turn(
            command("session-1", "task-1"),
            Vec::new(),
            &driver,
            CancellationToken::new(),
            ReadOnlyTurnControl {
                deadline: Some(requested_deadline),
            },
        )
        .await
        .expect("turn before deadline");
    let requests = driver.requests();
    let requested_deadline =
        chrono::DateTime::from_timestamp_millis(requested_deadline.timestamp_millis())
            .expect("canonical requested deadline");
    assert!(
        requests
            .iter()
            .all(|request| request.control.deadline == Some(requested_deadline))
    );

    let fixture = Fixture::new();
    let reference = fixture.store(b"abcdef", "session-1", Some("task-1"));
    let driver = ScriptedDriver::new(vec![
        tool_request_script(
            "call-1",
            ARTIFACT_READ_ID,
            artifact_arguments(&reference, 0, 1),
            FinishReason::ToolCalls,
        ),
        final_script(&["done"]),
    ]);
    let outcome = fixture
        .kernel
        .run_artifact_read_turn(
            command("session-1", "task-1"),
            Vec::new(),
            &driver,
            CancellationToken::new(),
            ReadOnlyTurnControl {
                deadline: Some(Utc::now() + ChronoDuration::minutes(10)),
            },
        )
        .await
        .expect("caller cannot extend hard ceiling");
    let snapshot = fixture.events_for_session("session-1");
    let input = snapshot
        .iter()
        .find(|event| {
            event.kind == event_kind::INPUT_RECEIVED
                && event.correlation_id.as_deref() == Some(outcome.turn_id.as_str())
        })
        .expect("turn input");
    let expected_ceiling =
        chrono::DateTime::from_timestamp_millis(input.recorded_at.timestamp_millis())
            .expect("millisecond timestamp")
            + ChronoDuration::from_std(ditto_kernel::turn::MAX_TURN_DURATION)
                .expect("chrono duration");
    assert!(
        driver
            .requests()
            .iter()
            .all(|request| request.control.deadline == Some(expected_ceiling))
    );
    drop(fixture.kernel.clone());
    let reopened = DittoKernel::open(fixture.config.clone()).expect("reopen hard-ceiling kernel");
    replay_artifact_read_turn(
        &all_session_events(&reopened, "session-1"),
        &outcome.turn_id,
    )
    .expect("hard ceiling replay after reopen");

    let fixture = Fixture::new();
    let driver = ScriptedDriver::new(vec![]);
    let requested_expired_deadline = Utc::now() - ChronoDuration::seconds(1);
    let error = fixture
        .kernel
        .run_artifact_read_turn(
            command("session-1", "task-1"),
            Vec::new(),
            &driver,
            CancellationToken::new(),
            ReadOnlyTurnControl {
                deadline: Some(requested_expired_deadline),
            },
        )
        .await
        .expect_err("expired deadline");
    let failure = match error {
        TurnRunError::Failed(failure) => failure,
        other => panic!("unexpected expired-deadline error: {other}"),
    };
    assert_eq!(failure.code, TurnFailureCode::DeadlineExceeded);
    let effective_deadline = match failure.evidence {
        Some(TurnFailureEvidence::Deadline { deadline }) => deadline,
        other => panic!("expired deadline lacks typed evidence: {other:?}"),
    };
    assert_eq!(
        effective_deadline,
        chrono::DateTime::from_timestamp_millis(requested_expired_deadline.timestamp_millis())
            .expect("canonical expired deadline")
    );
    assert!(driver.requests().is_empty());
    let snapshot = fixture.events_for_session("session-1");
    replay_artifact_read_turn(&snapshot, &failure.turn_id)
        .expect("initial deadline failure must replay");
    let reopened = DittoKernel::open(fixture.config.clone()).expect("reopen deadline kernel");
    replay_artifact_read_turn(
        &all_session_events(&reopened, "session-1"),
        &failure.turn_id,
    )
    .expect("initial deadline failure must replay after reopen");

    let mut missing_evidence = snapshot.clone();
    missing_evidence
        .iter_mut()
        .find(|event| event.kind == event_kind::TURN_FAILED)
        .expect("deadline terminal")
        .payload["failure"]
        .as_object_mut()
        .expect("failure object")
        .remove("evidence");
    assert!(replay_artifact_read_turn(&missing_evidence, &failure.turn_id).is_err());

    let input_recorded_at = snapshot
        .iter()
        .find(|event| event.kind == event_kind::INPUT_RECEIVED)
        .expect("deadline input")
        .recorded_at;
    let extended_deadline = input_recorded_at
        + ChronoDuration::from_std(ditto_kernel::turn::MAX_TURN_DURATION).expect("turn duration")
        + ChronoDuration::milliseconds(1);
    let mut extended_evidence = snapshot.clone();
    let terminal = extended_evidence
        .iter_mut()
        .find(|event| event.kind == event_kind::TURN_FAILED)
        .expect("deadline terminal");
    terminal.payload["failure"]["evidence"]["deadline"] =
        json!(extended_deadline.timestamp_millis());
    terminal.recorded_at = extended_deadline;
    assert!(replay_artifact_read_turn(&extended_evidence, &failure.turn_id).is_err());

    let mut early_terminal = snapshot;
    terminal_recorded_before_deadline(&mut early_terminal, effective_deadline);
    assert!(replay_artifact_read_turn(&early_terminal, &failure.turn_id).is_err());
}

#[tokio::test]
async fn a_held_provider_cannot_cross_the_model_output_deadline() {
    let fixture = Fixture::new();
    let error = fixture
        .kernel
        .run_artifact_read_turn(
            command("session-1", "task-1"),
            Vec::new(),
            &DelayedDriver::new(std::time::Duration::from_secs(2)),
            CancellationToken::new(),
            ReadOnlyTurnControl {
                deadline: Some(Utc::now() + ChronoDuration::seconds(1)),
            },
        )
        .await
        .expect_err("provider held beyond deadline");
    let failure = match error {
        TurnRunError::Failed(failure) => failure,
        other => panic!("unexpected held-provider error: {other}"),
    };
    assert_eq!(failure.code, TurnFailureCode::DeadlineExceeded);
    assert_eq!(
        failure.message,
        "turn deadline elapsed while awaiting model output"
    );
    let snapshot = fixture.events_for_session("session-1");
    assert!(
        !turn_events(&snapshot, &failure.turn_id)
            .iter()
            .any(|event| event.kind == event_kind::MODEL_OUTPUT)
    );
    replay_artifact_read_turn(&snapshot, &failure.turn_id)
        .expect("held-provider deadline must replay");
}

#[tokio::test]
async fn cancellation_and_deadline_are_checked_before_turn_finished() {
    let fixture = Fixture::new();
    let reference = fixture.store(b"abcdef", "session-1", Some("task-1"));
    let driver = Arc::new(ScriptedDriver::new(vec![
        tool_request_script(
            "call-1",
            ARTIFACT_READ_ID,
            artifact_arguments(&reference, 0, 1),
            FinishReason::ToolCalls,
        ),
        final_script(&["done"]),
    ]));
    let cancellation = CancellationToken::new();
    let mut events = fixture.kernel.subscribe();
    let kernel = fixture.kernel.clone();
    let driver_for_turn = driver.clone();
    let cancellation_for_turn = cancellation.clone();
    let handle = tokio::spawn(async move {
        kernel
            .run_artifact_read_turn(
                command("session-1", "task-1"),
                Vec::new(),
                driver_for_turn.as_ref(),
                cancellation_for_turn,
                ReadOnlyTurnControl::default(),
            )
            .await
    });
    loop {
        let event = events.recv().await.expect("turn event");
        if event.kind == event_kind::MODEL_OUTPUT {
            let output: ModelOutputPayload =
                serde_json::from_value(event.payload).expect("model output payload");
            if output.request_index == 1
                && matches!(output.stream_event.event, ModelEvent::Completed { .. })
            {
                cancellation.cancel();
                break;
            }
        }
    }
    let failure = match handle
        .await
        .expect("turn task")
        .expect_err("pre-finish cancellation")
    {
        TurnRunError::Failed(failure) => failure,
        other => panic!("unexpected pre-finish cancellation: {other}"),
    };
    assert_eq!(
        failure.message,
        "turn was cancelled after final model output and before turn completion"
    );
    let snapshot = fixture.events_for_session("session-1");
    assert!(
        !turn_events(&snapshot, &failure.turn_id)
            .iter()
            .any(|event| event.kind == event_kind::TURN_FINISHED)
    );
    replay_artifact_read_turn(&snapshot, &failure.turn_id)
        .expect("pre-finish cancellation must replay");

    let deadline: chrono::DateTime<Utc> = serde_json::from_value(
        snapshot
            .iter()
            .find(|event| {
                event.kind == event_kind::MODEL_REQUESTED
                    && event.payload["request_index"] == json!(1)
            })
            .expect("final model request")
            .payload["request"]["control"]["deadline"]
            .clone(),
    )
    .expect("request deadline");
    let mut deadline_trace = snapshot;
    let terminal = deadline_trace
        .iter_mut()
        .find(|event| event.kind == event_kind::TURN_FAILED)
        .expect("turn failure");
    terminal.payload["failure"]["code"] = json!("deadline_exceeded");
    terminal.payload["failure"]["message"] =
        json!("turn deadline elapsed after final model output and before turn completion");
    attach_deadline_evidence(terminal, deadline);
    terminal.recorded_at = deadline;
    replay_artifact_read_turn(&deadline_trace, &failure.turn_id)
        .expect("temporally valid pre-finish deadline trace");
    terminal_recorded_before_deadline(&mut deadline_trace, deadline);
    assert!(replay_artifact_read_turn(&deadline_trace, &failure.turn_id).is_err());
}

#[tokio::test]
async fn turn_ingress_reuses_the_canonical_input_and_identifier_bounds() {
    let fixture = Fixture::new();
    let driver = ScriptedDriver::new(vec![]);
    let error = fixture
        .kernel
        .run_artifact_read_turn(
            SubmitInputCommand {
                text: "x".repeat(64 * 1024 + 1),
                session_id: Some("session-1".into()),
                task_id: Some("task-1".into()),
            },
            Vec::new(),
            &driver,
            CancellationToken::new(),
            ReadOnlyTurnControl::default(),
        )
        .await
        .expect_err("oversized input");
    assert!(matches!(
        error,
        TurnRunError::Kernel(ditto_kernel::KernelError::InvalidCommand(_))
    ));
    assert_eq!(fixture.kernel.event_count().expect("event count"), 0);

    let error = fixture
        .kernel
        .run_artifact_read_turn(
            SubmitInputCommand {
                text: "valid".into(),
                session_id: Some("bad\nsession".into()),
                task_id: Some("task-1".into()),
            },
            Vec::new(),
            &driver,
            CancellationToken::new(),
            ReadOnlyTurnControl::default(),
        )
        .await
        .expect_err("control character in identifier");
    assert!(matches!(
        error,
        TurnRunError::Kernel(ditto_kernel::KernelError::InvalidCommand(_))
    ));
    assert_eq!(fixture.kernel.event_count().expect("event count"), 0);
}

#[tokio::test]
async fn provider_closure_event_and_text_bounds_fail_without_completion_claim() {
    let fixture = Fixture::new();
    let driver = ScriptedDriver::new(vec![vec![]]);
    let error = fixture
        .kernel
        .run_artifact_read_turn(
            command("session-1", "task-1"),
            Vec::new(),
            &driver,
            CancellationToken::new(),
            ReadOnlyTurnControl::default(),
        )
        .await
        .expect_err("provider closure");
    let failure = match error {
        TurnRunError::Failed(failure) => failure,
        other => panic!("unexpected provider-closure error: {other}"),
    };
    assert_eq!(failure.code, TurnFailureCode::Protocol);
    assert_eq!(
        failure.message,
        "provider stream ended without a terminal event"
    );
    let snapshot = fixture.events_for_session("session-1");
    let output: ModelOutputPayload = serde_json::from_value(
        turn_events(&snapshot, &failure.turn_id)
            .into_iter()
            .find(|event| event.kind == event_kind::MODEL_OUTPUT)
            .expect("raw EOF synthesized model failure")
            .payload,
    )
    .expect("decode admitted EOF failure");
    assert!(matches!(
        output.stream_event.event,
        ModelEvent::Failed {
            failure: ditto_model::ModelFailure {
                kind: FailureKind::Protocol,
                ..
            }
        }
    ));
    replay_artifact_read_turn(&snapshot, &failure.turn_id)
        .expect("raw EOF synthesized failure must replay");
    let reopened = DittoKernel::open(fixture.config.clone()).expect("reopen EOF kernel");
    replay_artifact_read_turn(
        &all_session_events(&reopened, "session-1"),
        &failure.turn_id,
    )
    .expect("raw EOF synthesized failure must replay after reopen");

    let mut forged_outputless = snapshot;
    let request_id = forged_outputless
        .iter()
        .find(|event| {
            event.kind == event_kind::MODEL_REQUESTED
                && event.correlation_id.as_deref() == Some(failure.turn_id.as_str())
        })
        .expect("model request")
        .event_id
        .clone();
    let output_position = forged_outputless
        .iter()
        .position(|event| {
            event.kind == event_kind::MODEL_OUTPUT
                && event.correlation_id.as_deref() == Some(failure.turn_id.as_str())
        })
        .expect("admitted EOF failure");
    forged_outputless.remove(output_position);
    forged_outputless
        .iter_mut()
        .find(|event| {
            event.kind == event_kind::TURN_FAILED
                && event.correlation_id.as_deref() == Some(failure.turn_id.as_str())
        })
        .expect("turn failure")
        .causation_id = Some(request_id);
    assert!(replay_artifact_read_turn(&forged_outputless, &failure.turn_id).is_err());

    let fixture = Fixture::new();
    let driver = ScriptedDriver::new(vec![vec![ModelEvent::TextDelta {
        text: "x".repeat(ditto_kernel::turn::MAX_ASSISTANT_TEXT_BYTES + 1),
    }]]);
    let error = fixture
        .kernel
        .run_artifact_read_turn(
            command("session-1", "task-1"),
            Vec::new(),
            &driver,
            CancellationToken::new(),
            ReadOnlyTurnControl::default(),
        )
        .await
        .expect_err("assistant text bound");
    assert!(
        matches!(error, TurnRunError::Failed(ref failure) if failure.code == TurnFailureCode::BoundExceeded)
    );
    assert!(
        !fixture
            .events_for_task("task-1")
            .iter()
            .any(|event| event.kind == event_kind::TASK_COMPLETED)
    );
}

#[tokio::test]
async fn invalid_provider_event_is_terminalized_before_kernel_admission_and_replays_after_reopen() {
    let fixture = Fixture::new();
    let driver = ScriptedDriver::new(vec![vec![ModelEvent::TextDelta {
        text: String::new(),
    }]]);
    let failure = match fixture
        .kernel
        .run_artifact_read_turn(
            command("session-1", "task-1"),
            Vec::new(),
            &driver,
            CancellationToken::new(),
            ReadOnlyTurnControl::default(),
        )
        .await
        .expect_err("empty provider text delta")
    {
        TurnRunError::Failed(failure) => failure,
        other => panic!("unexpected invalid-provider error: {other}"),
    };
    assert_eq!(failure.code, TurnFailureCode::Protocol);
    assert_eq!(
        failure.message,
        "provider emitted an invalid model event: text delta text is empty"
    );
    assert!(failure.evidence.is_none());

    let snapshot = fixture.events_for_session("session-1");
    let output: ModelOutputPayload = serde_json::from_value(
        turn_events(&snapshot, &failure.turn_id)
            .into_iter()
            .find(|event| event.kind == event_kind::MODEL_OUTPUT)
            .expect("validated failure model output")
            .payload,
    )
    .expect("decode validated model failure");
    assert!(matches!(
        output.stream_event.event,
        ModelEvent::Failed { .. }
    ));
    replay_artifact_read_turn(&snapshot, &failure.turn_id)
        .expect("validated provider failure must replay");
    let reopened = DittoKernel::open(fixture.config.clone()).expect("reopen provider failure");
    replay_artifact_read_turn(
        &all_session_events(&reopened, "session-1"),
        &failure.turn_id,
    )
    .expect("validated provider failure must replay after reopen");

    let mut mutated = snapshot;
    mutated
        .iter_mut()
        .find(|event| event.kind == event_kind::TURN_FAILED)
        .expect("provider terminal")
        .payload["failure"]["message"] = json!("provider emitted an invalid model event");
    assert!(replay_artifact_read_turn(&mutated, &failure.turn_id).is_err());
}

#[tokio::test]
async fn provider_reported_deadline_is_a_model_failure_not_a_kernel_deadline_claim() {
    let fixture = Fixture::new();
    let driver = ScriptedDriver::new(vec![vec![ModelEvent::Failed {
        failure: ditto_model::ModelFailure::new(
            FailureKind::DeadlineExceeded,
            "provider-local deadline elapsed",
        ),
    }]]);
    let failure = match fixture
        .kernel
        .run_artifact_read_turn(
            command("session-1", "task-1"),
            Vec::new(),
            &driver,
            CancellationToken::new(),
            ReadOnlyTurnControl {
                deadline: Some(Utc::now() + ChronoDuration::seconds(30)),
            },
        )
        .await
        .expect_err("provider deadline failure")
    {
        TurnRunError::Failed(failure) => failure,
        other => panic!("unexpected provider-deadline error: {other}"),
    };
    assert_eq!(failure.code, TurnFailureCode::ModelFailure);
    assert_eq!(failure.message, "provider-local deadline elapsed");
    assert!(failure.evidence.is_none());

    let snapshot = fixture.events_for_session("session-1");
    replay_artifact_read_turn(&snapshot, &failure.turn_id)
        .expect("provider deadline failure must replay");
    let reopened = DittoKernel::open(fixture.config.clone()).expect("reopen provider deadline");
    replay_artifact_read_turn(
        &all_session_events(&reopened, "session-1"),
        &failure.turn_id,
    )
    .expect("provider deadline failure must replay after reopen");

    let mut forged_kernel_deadline = snapshot;
    let terminal = forged_kernel_deadline
        .iter_mut()
        .find(|event| event.kind == event_kind::TURN_FAILED)
        .expect("provider deadline terminal");
    terminal.payload["failure"]["code"] = json!("deadline_exceeded");
    assert!(replay_artifact_read_turn(&forged_kernel_deadline, &failure.turn_id).is_err());
}

#[tokio::test]
async fn reasoning_events_are_durably_rejected_and_replay_requires_the_exact_failure() {
    let fixture = Fixture::new();
    let driver = ScriptedDriver::new(vec![vec![ModelEvent::ReasoningItemStarted {
        item_id: ReasoningItemId::new("reasoning-1").expect("reasoning id"),
    }]]);
    let error = fixture
        .kernel
        .run_artifact_read_turn(
            command("session-1", "task-1"),
            Vec::new(),
            &driver,
            CancellationToken::new(),
            ReadOnlyTurnControl::default(),
        )
        .await
        .expect_err("reasoning is outside the text+tool loop");
    let failure = match error {
        TurnRunError::Failed(failure) => *failure,
        other => panic!("expected durable protocol failure, got {other}"),
    };
    assert_eq!(failure.code, TurnFailureCode::Protocol);
    assert_eq!(
        failure.message,
        "reasoning events are not permitted in this turn loop"
    );
    let snapshot = fixture.events_for_session("session-1");
    let replay =
        replay_artifact_read_turn(&snapshot, &failure.turn_id).expect("reasoning failure replays");
    assert_eq!(replay.outputs.len(), 1);

    let mut missing_output = snapshot;
    missing_output.retain(|event| event.kind != event_kind::MODEL_OUTPUT);
    assert!(replay_artifact_read_turn(&missing_output, &failure.turn_id).is_err());
}

#[tokio::test]
async fn request_round_bound_accepts_seven_tools_plus_final_and_rejects_an_eighth_tool() {
    let fixture = Fixture::new();
    let reference = fixture.store(b"abcdef", "session-1", Some("task-1"));
    let mut scripts = (0..7)
        .map(|index| {
            tool_request_script(
                &format!("call-{index}"),
                ARTIFACT_READ_ID,
                artifact_arguments(&reference, 0, 1),
                FinishReason::ToolCalls,
            )
        })
        .collect::<Vec<_>>();
    scripts.push(final_script(&["finished on request eight"]));
    let driver = ScriptedDriver::new(scripts);
    let outcome = fixture
        .kernel
        .run_artifact_read_turn(
            command("session-1", "task-1"),
            Vec::new(),
            &driver,
            CancellationToken::new(),
            ReadOnlyTurnControl::default(),
        )
        .await
        .expect("eighth request may finish");
    assert_eq!(outcome.request_count, 8);
    assert_eq!(outcome.tool_call_count, 7);

    let fixture = Fixture::new();
    let reference = fixture.store(b"abcdef", "session-1", Some("task-1"));
    let scripts = (0..8)
        .map(|index| {
            tool_request_script(
                &format!("call-{index}"),
                ARTIFACT_READ_ID,
                artifact_arguments(&reference, 0, 1),
                FinishReason::ToolCalls,
            )
        })
        .collect::<Vec<_>>();
    let driver = ScriptedDriver::new(scripts);
    let error = fixture
        .kernel
        .run_artifact_read_turn(
            command("session-1", "task-1"),
            Vec::new(),
            &driver,
            CancellationToken::new(),
            ReadOnlyTurnControl::default(),
        )
        .await
        .expect_err("eighth request cannot execute another tool");
    assert!(
        matches!(error, TurnRunError::Failed(ref failure) if failure.code == TurnFailureCode::BoundExceeded)
    );
    assert_eq!(
        fixture
            .events_for_task("task-1")
            .iter()
            .filter(|event| event.kind == event_kind::EXECUTION_OUTPUT)
            .count(),
        7
    );
}

#[tokio::test]
async fn event_bound_accepts_terminal_at_4096_and_rejects_a_4097th_event() {
    fn warning(index: usize) -> ModelEvent {
        ModelEvent::ProviderWarning {
            warning: ProviderWarning {
                code: Some("bounded".into()),
                message: format!("warning-{index}"),
            },
        }
    }

    let fixture = Fixture::new();
    let reference = fixture.store(b"abcdef", "session-1", Some("task-1"));
    let mut first = (0..(ditto_kernel::turn::MAX_MODEL_EVENTS_PER_REQUEST - 4))
        .map(warning)
        .collect::<Vec<_>>();
    first.extend(tool_request_script(
        "call-1",
        ARTIFACT_READ_ID,
        artifact_arguments(&reference, 0, 1),
        FinishReason::ToolCalls,
    ));
    assert_eq!(
        first.len(),
        ditto_kernel::turn::MAX_MODEL_EVENTS_PER_REQUEST
    );
    let driver = ScriptedDriver::new(vec![first, final_script(&["done"])]);
    let outcome = fixture
        .kernel
        .run_artifact_read_turn(
            command("session-1", "task-1"),
            Vec::new(),
            &driver,
            CancellationToken::new(),
            ReadOnlyTurnControl::default(),
        )
        .await
        .expect("terminal at event bound");
    assert_eq!(outcome.request_count, 2);

    let fixture = Fixture::new();
    let overflow = (0..=ditto_kernel::turn::MAX_MODEL_EVENTS_PER_REQUEST)
        .map(warning)
        .collect::<Vec<_>>();
    let driver = ScriptedDriver::new(vec![overflow]);
    let error = fixture
        .kernel
        .run_artifact_read_turn(
            command("session-1", "task-1"),
            Vec::new(),
            &driver,
            CancellationToken::new(),
            ReadOnlyTurnControl::default(),
        )
        .await
        .expect_err("4097th semantic event is not consumed");
    assert!(
        matches!(error, TurnRunError::Failed(ref failure) if failure.code == TurnFailureCode::BoundExceeded)
    );
    assert_eq!(
        fixture
            .events_for_task("task-1")
            .iter()
            .filter(|event| event.kind == event_kind::MODEL_OUTPUT)
            .count(),
        ditto_kernel::turn::MAX_MODEL_EVENTS_PER_REQUEST
    );
}

#[tokio::test]
async fn encoded_model_output_bounds_accept_exact_n_and_reject_n_plus_one() {
    for case in [
        OutputBudgetCase::EventExact,
        OutputBudgetCase::EventOverflow,
        OutputBudgetCase::RequestExact,
        OutputBudgetCase::RequestOverflow,
    ] {
        let fixture = Fixture::new();
        let error = fixture
            .kernel
            .run_artifact_read_turn(
                command("session-1", "task-1"),
                Vec::new(),
                &OutputBudgetDriver::new(case),
                CancellationToken::new(),
                ReadOnlyTurnControl::default(),
            )
            .await
            .expect_err("output-only script cannot finish the read turn");
        let failure = match error {
            TurnRunError::Failed(failure) => failure,
            other => panic!("unexpected output-bound error: {other}"),
        };
        let snapshot = fixture.events_for_session("session-1");
        let replay = replay_artifact_read_turn(&snapshot, &failure.turn_id)
            .expect("bounded output trace must replay");
        assert!(matches!(
            replay.terminal,
            ArtifactReadTurnReplay::Failed { .. }
        ));
        let output_sizes = turn_events(&snapshot, &failure.turn_id)
            .iter()
            .filter(|event| event.kind == event_kind::MODEL_OUTPUT)
            .map(|event| {
                serde_json::to_vec(&event.payload)
                    .expect("encode output")
                    .len()
            })
            .collect::<Vec<_>>();

        match case {
            OutputBudgetCase::EventExact => {
                assert_eq!(failure.code, TurnFailureCode::Protocol);
                assert_eq!(
                    output_sizes.first().copied(),
                    Some(ditto_kernel::turn::MAX_MODEL_OUTPUT_EVENT_BYTES)
                );
                assert_eq!(output_sizes.len(), 2);
            }
            OutputBudgetCase::EventOverflow => {
                assert_eq!(failure.code, TurnFailureCode::BoundExceeded);
                assert_eq!(
                    failure.message,
                    format!(
                        "model output exceeded {} encoded bytes",
                        ditto_kernel::turn::MAX_MODEL_OUTPUT_EVENT_BYTES
                    )
                );
                assert!(output_sizes.is_empty());
            }
            OutputBudgetCase::RequestExact => {
                assert_eq!(failure.code, TurnFailureCode::Protocol);
                assert_eq!(
                    output_sizes.iter().sum::<usize>(),
                    ditto_kernel::turn::MAX_MODEL_OUTPUT_BYTES_PER_REQUEST
                );
                assert_eq!(output_sizes.len(), 15);
            }
            OutputBudgetCase::RequestOverflow => {
                assert_eq!(failure.code, TurnFailureCode::BoundExceeded);
                assert_eq!(
                    failure.message,
                    format!(
                        "model request output exceeded {} encoded bytes",
                        ditto_kernel::turn::MAX_MODEL_OUTPUT_BYTES_PER_REQUEST
                    )
                );
                assert_eq!(output_sizes.len(), 14);
                assert!(
                    output_sizes.iter().sum::<usize>()
                        < ditto_kernel::turn::MAX_MODEL_OUTPUT_BYTES_PER_REQUEST
                );
            }
        }
    }
}

#[tokio::test]
async fn assistant_text_bound_accepts_n_and_rejects_n_plus_one() {
    let fixture = Fixture::new();
    let reference = fixture.store(b"abcdef", "session-1", Some("task-1"));
    let arguments = artifact_arguments(&reference, 0, 1);
    let driver = ScriptedDriver::new(vec![
        tool_request_script(
            "call-1",
            ARTIFACT_READ_ID,
            arguments,
            FinishReason::ToolCalls,
        ),
        vec![
            ModelEvent::TextDelta {
                text: "x".repeat(ditto_kernel::turn::MAX_ASSISTANT_TEXT_BYTES),
            },
            ModelEvent::Completed {
                finish_reason: FinishReason::EndTurn,
                continuation: None,
            },
        ],
    ]);
    let outcome = fixture
        .kernel
        .run_artifact_read_turn(
            command("session-1", "task-1"),
            Vec::new(),
            &driver,
            CancellationToken::new(),
            ReadOnlyTurnControl::default(),
        )
        .await
        .expect("exact text bound");
    assert_eq!(
        outcome.response.len(),
        ditto_kernel::turn::MAX_ASSISTANT_TEXT_BYTES
    );

    let fixture = Fixture::new();
    let driver = ScriptedDriver::new(vec![vec![ModelEvent::TextDelta {
        text: "x".repeat(ditto_kernel::turn::MAX_ASSISTANT_TEXT_BYTES + 1),
    }]]);
    let error = fixture
        .kernel
        .run_artifact_read_turn(
            command("session-1", "task-1"),
            Vec::new(),
            &driver,
            CancellationToken::new(),
            ReadOnlyTurnControl::default(),
        )
        .await
        .expect_err("text N plus one");
    assert!(
        matches!(error, TurnRunError::Failed(ref failure) if failure.code == TurnFailureCode::BoundExceeded)
    );
}

#[tokio::test]
async fn turn_failure_messages_are_utf8_safe_at_n_and_truncated_at_n_plus_one() {
    for (message, expect_truncated) in [
        (
            "é".repeat(ditto_kernel::turn::MAX_TURN_FAILURE_MESSAGE_BYTES / 2),
            false,
        ),
        (
            "é".repeat(ditto_kernel::turn::MAX_TURN_FAILURE_MESSAGE_BYTES / 2 + 1),
            true,
        ),
    ] {
        let fixture = Fixture::new();
        let driver = ScriptedDriver::new(vec![vec![ModelEvent::Failed {
            failure: ditto_model::ModelFailure::new(FailureKind::Provider, message.clone()),
        }]]);
        let error = fixture
            .kernel
            .run_artifact_read_turn(
                command("session-1", "task-1"),
                Vec::new(),
                &driver,
                CancellationToken::new(),
                ReadOnlyTurnControl::default(),
            )
            .await
            .expect_err("provider failure must fail the turn");
        let failure = match error {
            TurnRunError::Failed(failure) => failure,
            other => panic!("unexpected provider failure: {other}"),
        };
        assert_eq!(failure.code, TurnFailureCode::ModelFailure);
        assert!(failure.message.len() <= ditto_kernel::turn::MAX_TURN_FAILURE_MESSAGE_BYTES);
        if expect_truncated {
            assert!(failure.message.ends_with("...[truncated]"));
        } else {
            assert_eq!(failure.message, message);
        }
        replay_artifact_read_turn(&fixture.events_for_session("session-1"), &failure.turn_id)
            .expect("bounded UTF-8 provider failure must replay");
    }
}

#[tokio::test]
async fn truncated_long_context_failure_replays_with_exact_stage_grammar() {
    let fixture = Fixture::new();
    let node = ContextNode {
        id: format!("node-{}", "é".repeat(3_000)),
        kind: ContextNodeKind::Resource,
        summary: "invalid durable context".into(),
        origin: ContextOrigin::User,
        epistemic: EpistemicStatus::Asserted,
        scope: ContextScope::Task,
        lens: ContextLens::Task,
        confidence: 1.0,
        source_event_ids: Vec::new(),
        supersedes: Vec::new(),
        valid_from: None,
        valid_until: None,
    };
    let error = fixture
        .kernel
        .run_artifact_read_turn(
            command("session-1", "task-1"),
            vec![ContextCandidate::user_pinned(node)],
            &ScriptedDriver::new(Vec::new()),
            CancellationToken::new(),
            ReadOnlyTurnControl::default(),
        )
        .await
        .expect_err("invalid required context must fail");
    let failure = match error {
        TurnRunError::Failed(failure) => failure,
        other => panic!("unexpected context failure: {other}"),
    };
    assert_eq!(failure.code, TurnFailureCode::ContextCompilation);
    assert!(failure.message.ends_with("...[truncated]"));
    assert!(failure.message.len() <= ditto_kernel::turn::MAX_TURN_FAILURE_MESSAGE_BYTES);
    replay_artifact_read_turn(&fixture.events_for_session("session-1"), &failure.turn_id)
        .expect("truncated context failure must replay");
}

#[tokio::test]
async fn invalid_context_directives_fail_durably_and_replay_exactly() {
    let fixture = Fixture::new();
    let node = ContextNode {
        id: "duplicate-context".into(),
        kind: ContextNodeKind::Resource,
        summary: "read the artifact".into(),
        origin: ContextOrigin::User,
        epistemic: EpistemicStatus::Asserted,
        scope: ContextScope::Turn,
        lens: ContextLens::Task,
        confidence: 1.0,
        source_event_ids: Vec::new(),
        supersedes: Vec::new(),
        valid_from: None,
        valid_until: None,
    };
    let policy_node = node.clone();
    let failure = match fixture
        .kernel
        .run_artifact_read_turn(
            command("session-1", "task-1"),
            vec![
                ContextCandidate::user_pinned(node.clone()),
                ContextCandidate::user_pinned(node),
            ],
            &ScriptedDriver::new(Vec::new()),
            CancellationToken::new(),
            ReadOnlyTurnControl::default(),
        )
        .await
        .expect_err("duplicate context candidate")
    {
        TurnRunError::Failed(failure) => failure,
        other => panic!("unexpected duplicate context error: {other}"),
    };
    assert_eq!(failure.code, TurnFailureCode::ContextCompilation);
    assert_eq!(
        failure.message,
        "context candidate duplicate-context appears more than once"
    );
    replay_artifact_read_turn(&fixture.events_for_session("session-1"), &failure.turn_id)
        .expect("duplicate context failure must replay");

    let fixture = Fixture::new();
    let failure = match fixture
        .kernel
        .run_artifact_read_turn(
            command("session-1", "task-1"),
            vec![ContextCandidate::policy_required(policy_node, "   ")],
            &ScriptedDriver::new(Vec::new()),
            CancellationToken::new(),
            ReadOnlyTurnControl::default(),
        )
        .await
        .expect_err("empty policy reason")
    {
        TurnRunError::Failed(failure) => failure,
        other => panic!("unexpected policy context error: {other}"),
    };
    assert_eq!(failure.code, TurnFailureCode::ContextCompilation);
    assert_eq!(
        failure.message,
        "policy-required context duplicate-context has an empty reason"
    );
    replay_artifact_read_turn(&fixture.events_for_session("session-1"), &failure.turn_id)
        .expect("invalid policy context failure must replay");
}

#[tokio::test]
async fn compiled_context_requires_resolved_same_scope_provenance() {
    let fixture = Fixture::new();
    let source = fixture
        .kernel
        .record_user_input(SubmitInputCommand {
            text: "source".into(),
            session_id: Some("session-1".into()),
            task_id: Some("task-1".into()),
        })
        .expect("source event");
    let node = ContextNode {
        id: "context-1".into(),
        kind: ContextNodeKind::Resource,
        summary: "read the artifact".into(),
        origin: ContextOrigin::User,
        epistemic: EpistemicStatus::Asserted,
        scope: ContextScope::Task,
        lens: ContextLens::Task,
        confidence: 1.0,
        source_event_ids: vec![source.event_id],
        supersedes: Vec::new(),
        valid_from: None,
        valid_until: None,
    };
    let reference = fixture.store(b"abcdef", "session-1", Some("task-1"));
    let arguments = artifact_arguments(&reference, 0, 1);
    let driver = ScriptedDriver::new(vec![
        tool_request_script(
            "call-1",
            ARTIFACT_READ_ID,
            arguments,
            FinishReason::ToolCalls,
        ),
        final_script(&["done"]),
    ]);
    let outcome = fixture
        .kernel
        .run_artifact_read_turn(
            command("session-1", "task-1"),
            vec![ContextCandidate::user_pinned(node)],
            &driver,
            CancellationToken::new(),
            ReadOnlyTurnControl::default(),
        )
        .await
        .expect("same-scope provenance");
    let snapshot = fixture.events_for_session("session-1");
    replay_artifact_read_turn(&snapshot, &outcome.turn_id).expect("provenance replay");

    let input_recorded_at = snapshot
        .iter()
        .find(|event| {
            event.kind == event_kind::INPUT_RECEIVED
                && event.correlation_id.as_deref() == Some(outcome.turn_id.as_str())
        })
        .expect("turn input")
        .recorded_at;
    let valid_until = input_recorded_at + ChronoDuration::seconds(1);
    let failure_time = input_recorded_at + ChronoDuration::seconds(2);
    let request_position = snapshot
        .iter()
        .position(|event| {
            event.kind == event_kind::MODEL_REQUESTED
                && event.correlation_id.as_deref() == Some(outcome.turn_id.as_str())
        })
        .expect("first model request");
    let output_position = snapshot
        .iter()
        .enumerate()
        .skip(request_position + 1)
        .find(|(_, event)| {
            event.kind == event_kind::MODEL_OUTPUT
                && event.correlation_id.as_deref() == Some(outcome.turn_id.as_str())
        })
        .map(|(index, _)| index)
        .expect("first model output");
    let mut expiry_failure = snapshot[..=output_position].to_vec();
    let context = expiry_failure
        .iter_mut()
        .find(|event| event.kind == event_kind::CONTEXT_COMPILED)
        .expect("context compiled");
    context.payload["compiled"]["nodes"][0]["valid_until"] = json!(valid_until);
    context.payload["capsule"]["nodes"][0]["valid_until"] = json!(valid_until);
    refresh_context_token_accounting(context);
    let request = &mut expiry_failure[request_position];
    request.recorded_at = failure_time;
    request.payload["request"]["turn"]["context"]["nodes"][0]["valid_until"] = json!(valid_until);
    let terminal = expiry_failure.last_mut().expect("synthetic terminal");
    terminal.kind = event_kind::TURN_FAILED.into();
    terminal.actor = EventActor::System;
    terminal.span_id = None;
    terminal.recorded_at = failure_time;
    terminal.payload = json!({
        "event_version": 1,
        "turn_id": outcome.turn_id.clone(),
        "failure": {
            "turn_id": outcome.turn_id.clone(),
            "session_id": "session-1",
            "task_id": "task-1",
            "code": "driver_contract",
            "message": "model context capsule is invalid: context capsule item context-1 is disputed or not valid at the requested time",
            "request_index": 0
        },
        "status": "unverified",
        "request_count": 1,
        "tool_call_count": 0
    });
    replay_artifact_read_turn(&expiry_failure, &outcome.turn_id)
        .expect("exact post-request context expiry must replay");
    let mut forged_expiry = expiry_failure;
    let still_valid = failure_time + ChronoDuration::seconds(1);
    let context = forged_expiry
        .iter_mut()
        .find(|event| event.kind == event_kind::CONTEXT_COMPILED)
        .expect("context compiled");
    context.payload["compiled"]["nodes"][0]["valid_until"] = json!(still_valid);
    context.payload["capsule"]["nodes"][0]["valid_until"] = json!(still_valid);
    refresh_context_token_accounting(context);
    forged_expiry[request_position].payload["request"]["turn"]["context"]["nodes"][0]["valid_until"] =
        json!(still_valid);
    assert!(replay_artifact_read_turn(&forged_expiry, &outcome.turn_id).is_err());

    let mut missing_source = snapshot.clone();
    let context = missing_source
        .iter_mut()
        .find(|event| {
            event.kind == event_kind::CONTEXT_COMPILED
                && event.correlation_id.as_deref() == Some(outcome.turn_id.as_str())
        })
        .expect("compiled context");
    context.payload["compiled"]["nodes"][0]["source_event_ids"][0] = json!("missing-source");
    context.payload["compiled"]["receipt"]["included"][0]["source_event_ids"][0] =
        json!("missing-source");
    context.payload["capsule"]["nodes"][0]["source_event_ids"][0] = json!("missing-source");
    assert!(replay_artifact_read_turn(&missing_source, &outcome.turn_id).is_err());

    let mut bad_cutoff = snapshot;
    let context = bad_cutoff
        .iter_mut()
        .find(|event| {
            event.kind == event_kind::CONTEXT_COMPILED
                && event.correlation_id.as_deref() == Some(outcome.turn_id.as_str())
        })
        .expect("compiled context");
    context.payload["provenance_through_seq"] = json!(context.seq);
    assert!(replay_artifact_read_turn(&bad_cutoff, &outcome.turn_id).is_err());

    let fixture = Fixture::new();
    let source = fixture
        .kernel
        .record_user_input(SubmitInputCommand {
            text: "source".into(),
            session_id: Some("other-session".into()),
            task_id: Some("task-1".into()),
        })
        .expect("cross-scope source");
    let node = ContextNode {
        id: "context-1".into(),
        kind: ContextNodeKind::Resource,
        summary: "read the artifact".into(),
        origin: ContextOrigin::User,
        epistemic: EpistemicStatus::Asserted,
        scope: ContextScope::Task,
        lens: ContextLens::Task,
        confidence: 1.0,
        source_event_ids: vec![source.event_id],
        supersedes: Vec::new(),
        valid_from: None,
        valid_until: None,
    };
    let driver = ScriptedDriver::new(vec![]);
    let error = fixture
        .kernel
        .run_artifact_read_turn(
            command("session-1", "task-1"),
            vec![ContextCandidate::user_pinned(node)],
            &driver,
            CancellationToken::new(),
            ReadOnlyTurnControl::default(),
        )
        .await
        .expect_err("cross-scope provenance");
    assert!(
        matches!(error, TurnRunError::Failed(ref failure) if failure.code == TurnFailureCode::ContextCompilation)
    );
    assert!(
        !fixture
            .events_for_task("task-1")
            .iter()
            .any(|event| event.kind == event_kind::CONTEXT_COMPILED)
    );
}

#[tokio::test]
async fn context_replay_revalidates_acceptance_structure_score_and_capability_stage_failure() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let capabilities = directory.path().join("empty-capabilities");
    fs::create_dir_all(&capabilities).expect("empty capability directory");
    let kernel = DittoKernel::open(KernelConfig::new(
        directory.path().join("data"),
        capabilities,
    ))
    .expect("open kernel");
    let source = kernel
        .record_user_input(SubmitInputCommand {
            text: "source".into(),
            session_id: Some("session-1".into()),
            task_id: Some("task-1".into()),
        })
        .expect("source event");
    let node = ContextNode {
        id: "context-1".into(),
        kind: ContextNodeKind::Resource,
        summary: "read the artifact".into(),
        origin: ContextOrigin::User,
        epistemic: EpistemicStatus::Asserted,
        scope: ContextScope::Task,
        lens: ContextLens::Task,
        confidence: 1.0,
        source_event_ids: vec![source.event_id],
        supersedes: Vec::new(),
        valid_from: None,
        valid_until: None,
    };
    let failure = match kernel
        .run_artifact_read_turn(
            command("session-1", "task-1"),
            vec![ContextCandidate::user_pinned(node)],
            &ScriptedDriver::new(Vec::new()),
            CancellationToken::new(),
            ReadOnlyTurnControl::default(),
        )
        .await
        .expect_err("missing installed capability")
    {
        TurnRunError::Failed(failure) => failure,
        other => panic!("unexpected capability-stage failure: {other}"),
    };
    assert_eq!(failure.code, TurnFailureCode::CapabilityUnavailable);
    let snapshot = all_session_events(&kernel, "session-1");
    replay_artifact_read_turn(&snapshot, &failure.turn_id).expect("valid capability-stage failure");

    let mut bad_score = snapshot.clone();
    bad_score
        .iter_mut()
        .find(|event| event.kind == event_kind::CONTEXT_COMPILED)
        .expect("context compiled")
        .payload["compiled"]["receipt"]["included"][0]["score"] = json!(42.0);
    assert!(replay_artifact_read_turn(&bad_score, &failure.turn_id).is_err());

    let accepted_at = snapshot
        .iter()
        .find(|event| {
            event.kind == event_kind::INPUT_RECEIVED
                && event.correlation_id.as_deref() == Some(failure.turn_id.as_str())
        })
        .expect("turn input")
        .recorded_at;
    let mut expired_at_acceptance = snapshot.clone();
    let context = expired_at_acceptance
        .iter_mut()
        .find(|event| event.kind == event_kind::CONTEXT_COMPILED)
        .expect("context compiled");
    context.payload["compiled"]["nodes"][0]["valid_until"] = json!(accepted_at);
    context.payload["capsule"]["nodes"][0]["valid_until"] = json!(accepted_at);
    assert!(replay_artifact_read_turn(&expired_at_acceptance, &failure.turn_id).is_err());

    let mut impossible_confidence = snapshot;
    let context = impossible_confidence
        .iter_mut()
        .find(|event| event.kind == event_kind::CONTEXT_COMPILED)
        .expect("context compiled");
    context.payload["compiled"]["nodes"][0]["confidence"] = json!(2.0);
    context.payload["capsule"]["nodes"][0]["confidence"] = json!(2.0);
    assert!(replay_artifact_read_turn(&impossible_confidence, &failure.turn_id).is_err());
}

#[tokio::test]
async fn submillisecond_context_expiry_replays_identically_after_reopen() {
    let fixture = Fixture::new();
    let source = fixture
        .kernel
        .record_user_input(SubmitInputCommand {
            text: "source".into(),
            session_id: Some("session-1".into()),
            task_id: Some("task-1".into()),
        })
        .expect("source event");
    let failure = match fixture
        .kernel
        .run_artifact_read_turn(
            command("session-1", "task-1"),
            AcceptanceRelativeCandidates {
                kernel: fixture.kernel.clone(),
                source_event_id: source.event_id,
            },
            &ScriptedDriver::new(Vec::new()),
            CancellationToken::new(),
            ReadOnlyTurnControl::default(),
        )
        .await
        .expect_err("empty provider script")
    {
        TurnRunError::Failed(failure) => failure,
        other => panic!("unexpected submillisecond trace error: {other}"),
    };
    let snapshot = fixture.events_for_session("session-1");
    let valid_until: chrono::DateTime<Utc> = serde_json::from_value(
        snapshot
            .iter()
            .find(|event| {
                event.kind == event_kind::CONTEXT_COMPILED
                    && event.correlation_id.as_deref() == Some(failure.turn_id.as_str())
            })
            .expect("context compiled")
            .payload["capsule"]["nodes"][0]["valid_until"]
            .clone(),
    )
    .expect("submillisecond validity");
    assert_eq!(valid_until.timestamp_subsec_micros() % 1_000, 500);
    replay_artifact_read_turn(&snapshot, &failure.turn_id)
        .expect("live submillisecond trace replay");

    let reopened = DittoKernel::open(fixture.config.clone()).expect("reopen kernel");
    replay_artifact_read_turn(
        &all_session_events(&reopened, "session-1"),
        &failure.turn_id,
    )
    .expect("reopened submillisecond trace replay");
}

#[tokio::test]
async fn retrieval_manifest_contract_failure_is_durable_and_replayable() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let capabilities = directory.path().join("capabilities/core/artifact-read");
    fs::create_dir_all(&capabilities).expect("capability directory");
    let source_manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../capabilities/core/artifact-read/capability.toml");
    let manifest = fs::read_to_string(source_manifest)
        .expect("read canonical manifest")
        .replace(
            "inspect the full output of a previous command",
            "forged retrieval intent",
        );
    fs::write(capabilities.join("capability.toml"), manifest).expect("write test manifest");
    let kernel = DittoKernel::open(KernelConfig::new(
        directory.path().join("data"),
        directory.path().join("capabilities"),
    ))
    .expect("open kernel");
    let failure = match kernel
        .run_artifact_read_turn(
            command("session-1", "task-1"),
            Vec::new(),
            &ScriptedDriver::new(Vec::new()),
            CancellationToken::new(),
            ReadOnlyTurnControl::default(),
        )
        .await
        .expect_err("retrieval contract mismatch")
    {
        TurnRunError::Failed(failure) => failure,
        other => panic!("unexpected manifest error: {other}"),
    };
    assert_eq!(failure.code, TurnFailureCode::CapabilityContract);
    assert_eq!(
        failure.message,
        "artifact.read manifest does not match the builtin contract (retrieval.intents)"
    );
    replay_artifact_read_turn(&all_session_events(&kernel, "session-1"), &failure.turn_id)
        .expect("retrieval manifest failure must replay");
}

#[tokio::test]
async fn durable_publication_precedes_broadcast_and_replay_rejects_corruption() {
    let fixture = Fixture::new();
    let reference = fixture.store(b"abcdef", "session-1", Some("task-1"));
    let arguments = artifact_arguments(&reference, 0, 1);
    let driver = Arc::new(ScriptedDriver::new(vec![
        tool_request_script(
            "call-1",
            ARTIFACT_READ_ID,
            arguments,
            FinishReason::ToolCalls,
        ),
        final_script(&["done"]),
    ]));
    let mut subscriber = fixture.kernel.subscribe();
    let kernel = fixture.kernel.clone();
    let driver_for_turn = driver.clone();
    let handle = tokio::spawn(async move {
        kernel
            .run_artifact_read_turn(
                command("session-1", "task-1"),
                Vec::new(),
                driver_for_turn.as_ref(),
                CancellationToken::new(),
                ReadOnlyTurnControl::default(),
            )
            .await
    });
    loop {
        let event = subscriber.recv().await.expect("published event");
        if event.kind == event_kind::MODEL_REQUESTED {
            let durable = fixture.events_for_task("task-1");
            let stored = durable
                .iter()
                .find(|stored| stored.seq == event.seq)
                .expect("published event is already durable");
            assert_eq!(event.recorded_at, stored.recorded_at);
            break;
        }
    }
    let outcome = handle.await.expect("turn task").expect("turn outcome");
    let events = fixture.events_for_session("session-1");
    replay_artifact_read_turn(&events, &outcome.turn_id).expect("baseline replay");

    let mut truncated = events.clone();
    truncated.pop();
    assert!(replay_artifact_read_turn(&truncated, &outcome.turn_id).is_err());

    let mut duplicated = events.clone();
    duplicated.insert(2, duplicated[1].clone());
    assert!(replay_artifact_read_turn(&duplicated, &outcome.turn_id).is_err());

    let mut reordered = events.clone();
    let first_output = reordered
        .iter()
        .position(|event| event.kind == event_kind::MODEL_OUTPUT)
        .expect("model output");
    reordered.swap(first_output, first_output + 1);
    assert!(replay_artifact_read_turn(&reordered, &outcome.turn_id).is_err());

    let mut corrupted = events.clone();
    let requested = corrupted
        .iter_mut()
        .find(|event| event.kind == event_kind::MODEL_REQUESTED)
        .expect("model request");
    requested.payload["request_index"] = json!(7);
    assert!(replay_artifact_read_turn(&corrupted, &outcome.turn_id).is_err());

    let mut corrupted_span = events.clone();
    corrupted_span
        .iter_mut()
        .find(|event| event.kind == event_kind::MODEL_OUTPUT)
        .expect("model output")
        .span_id = Some("forged-span".into());
    assert!(replay_artifact_read_turn(&corrupted_span, &outcome.turn_id).is_err());

    let mut oversized_input = events.clone();
    oversized_input
        .iter_mut()
        .find(|event| {
            event.correlation_id.as_deref() == Some(outcome.turn_id.as_str())
                && event.kind == event_kind::INPUT_RECEIVED
        })
        .expect("turn input")
        .payload["text"] = json!("x".repeat(64 * 1024 + 1));
    assert!(replay_artifact_read_turn(&oversized_input, &outcome.turn_id).is_err());

    let mut corrupted_result = events.clone();
    let execution_output = corrupted_result
        .iter_mut()
        .find(|event| event.kind == event_kind::EXECUTION_OUTPUT)
        .expect("execution output");
    execution_output.payload["result"]["offset"] = json!(9);
    assert!(replay_artifact_read_turn(&corrupted_result, &outcome.turn_id).is_err());

    let mut corrupted_manifest = events.clone();
    corrupted_manifest
        .iter_mut()
        .find(|event| event.kind == event_kind::CAPABILITIES_SELECTED)
        .expect("capabilities selected")
        .payload["manifest"]["runtime"]["lazy"] = json!(false);
    assert!(replay_artifact_read_turn(&corrupted_manifest, &outcome.turn_id).is_err());

    let mut corrupted_retrieval = events.clone();
    corrupted_retrieval
        .iter_mut()
        .find(|event| event.kind == event_kind::CAPABILITIES_SELECTED)
        .expect("capabilities selected")
        .payload["manifest"]["retrieval"]["intents"] = json!(["forged"]);
    assert!(replay_artifact_read_turn(&corrupted_retrieval, &outcome.turn_id).is_err());

    let mut corrupted_epoch_card = events.clone();
    corrupted_epoch_card
        .iter_mut()
        .find(|event| event.kind == event_kind::CAPABILITIES_SELECTED)
        .expect("capabilities selected")
        .payload["epoch"]["capabilities"][0]["namespace"] = json!("forged");
    assert!(replay_artifact_read_turn(&corrupted_epoch_card, &outcome.turn_id).is_err());

    let mut corrupted_invocation_revision = events.clone();
    corrupted_invocation_revision
        .iter_mut()
        .find(|event| event.kind == event_kind::CAPABILITIES_SELECTED)
        .expect("capabilities selected")
        .payload["epoch"]["invocation_revisions"][0]["deriver_revision"] =
        json!("artifact.read/v999");
    assert!(replay_artifact_read_turn(&corrupted_invocation_revision, &outcome.turn_id).is_err());

    let mut corrupted_generation = events.clone();
    corrupted_generation
        .iter_mut()
        .find(|event| event.kind == event_kind::MODEL_REQUESTED)
        .expect("model request")
        .payload["request"]["generation"]["prompt_cache"] = json!({"type": "disabled"});
    assert!(replay_artifact_read_turn(&corrupted_generation, &outcome.turn_id).is_err());

    let mut output_at_deadline = events.clone();
    let deadline: chrono::DateTime<Utc> = serde_json::from_value(
        output_at_deadline
            .iter()
            .find(|event| event.kind == event_kind::MODEL_REQUESTED)
            .expect("model request")
            .payload["request"]["control"]["deadline"]
            .clone(),
    )
    .expect("request deadline");
    output_at_deadline
        .iter_mut()
        .find(|event| event.kind == event_kind::MODEL_OUTPUT)
        .expect("model output")
        .payload["admitted_at"] = json!(deadline.timestamp_millis());
    assert!(replay_artifact_read_turn(&output_at_deadline, &outcome.turn_id).is_err());

    let request_recorded_at = events
        .iter()
        .find(|event| event.kind == event_kind::MODEL_REQUESTED)
        .expect("model request")
        .recorded_at;
    let mut output_before_request = events.clone();
    output_before_request
        .iter_mut()
        .find(|event| event.kind == event_kind::MODEL_OUTPUT)
        .expect("model output")
        .payload["admitted_at"] = json!(request_recorded_at.timestamp_millis() - 1);
    assert!(replay_artifact_read_turn(&output_before_request, &outcome.turn_id).is_err());

    let mut decreasing_admission = events.clone();
    let output_positions = decreasing_admission
        .iter()
        .enumerate()
        .filter(|(_, event)| {
            event.kind == event_kind::MODEL_OUTPUT && event.payload["request_index"] == json!(0)
        })
        .map(|(index, _)| index)
        .take(2)
        .collect::<Vec<_>>();
    assert_eq!(output_positions.len(), 2);
    let later_recorded_at = request_recorded_at + ChronoDuration::milliseconds(2);
    decreasing_admission[output_positions[0]].recorded_at = later_recorded_at;
    decreasing_admission[output_positions[0]].payload["admitted_at"] =
        json!(request_recorded_at.timestamp_millis() + 2);
    decreasing_admission[output_positions[1]].recorded_at = later_recorded_at;
    decreasing_admission[output_positions[1]].payload["admitted_at"] =
        json!(request_recorded_at.timestamp_millis() + 1);
    assert!(replay_artifact_read_turn(&decreasing_admission, &outcome.turn_id).is_err());

    let mut corrupted_receipt = events.clone();
    corrupted_receipt
        .iter_mut()
        .find(|event| event.kind == event_kind::CONTEXT_COMPILED)
        .expect("context compiled")
        .payload["compiled"]["receipt"]["total_token_cost"] = json!(1);
    assert!(replay_artifact_read_turn(&corrupted_receipt, &outcome.turn_id).is_err());

    let mut forged_root_actor = events.clone();
    forged_root_actor
        .iter_mut()
        .find(|event| event.kind == event_kind::ARTIFACT_CREATED)
        .expect("artifact root")
        .actor = EventActor::Model;
    assert!(replay_artifact_read_turn(&forged_root_actor, &outcome.turn_id).is_err());

    let mut corrupted_authorization_cutoff = events.clone();
    let started = corrupted_authorization_cutoff
        .iter_mut()
        .find(|event| event.kind == event_kind::EXECUTION_STARTED)
        .expect("execution start");
    started.payload["authorization_through_seq"] = json!(started.seq);
    assert!(replay_artifact_read_turn(&corrupted_authorization_cutoff, &outcome.turn_id).is_err());

    let mut mixed_session = events.clone();
    mixed_session[0].session_id = Some("other-session".into());
    assert!(replay_artifact_read_turn(&mixed_session, &outcome.turn_id).is_err());

    let mut additive = events.clone();
    let requested = additive
        .iter_mut()
        .find(|event| event.kind == event_kind::MODEL_REQUESTED)
        .expect("model request");
    requested.payload["future_additive_field"] = json!(true);
    replay_artifact_read_turn(&additive, &outcome.turn_id)
        .expect("additive payload field is ignored");

    let mut unrelated_completion = events.clone();
    let terminal = unrelated_completion.last().expect("turn terminal").clone();
    unrelated_completion.push(EventRecord {
        seq: terminal.seq + 1,
        event_id: format!("{}-other-task-completed", terminal.event_id),
        recorded_at: terminal.recorded_at,
        session_id: terminal.session_id.clone(),
        task_id: Some("other-task".into()),
        actor: EventActor::System,
        kind: event_kind::TASK_COMPLETED.into(),
        payload: json!({"verified": true}),
        causation_id: None,
        correlation_id: Some("different-turn".into()),
        span_id: None,
    });
    replay_artifact_read_turn(&unrelated_completion, &outcome.turn_id)
        .expect("other task completion is unrelated");

    let mut completed = events;
    let terminal = completed.last().expect("turn terminal").clone();
    completed.push(EventRecord {
        seq: terminal.seq + 1,
        event_id: format!("{}-task-completed", terminal.event_id),
        recorded_at: terminal.recorded_at,
        session_id: terminal.session_id.clone(),
        task_id: terminal.task_id.clone(),
        actor: EventActor::System,
        kind: event_kind::TASK_COMPLETED.into(),
        payload: json!({"verified": false}),
        causation_id: None,
        correlation_id: Some("different-turn".into()),
        span_id: None,
    });
    assert!(replay_artifact_read_turn(&completed, &outcome.turn_id).is_err());
}

#[test]
fn event_and_round_constants_are_the_accepted_contract() {
    assert_eq!(ditto_kernel::turn::MAX_MODEL_REQUESTS, 8);
    assert_eq!(ditto_kernel::turn::MAX_MODEL_EVENTS_PER_REQUEST, 4_096);
    assert_eq!(ditto_kernel::turn::MAX_ASSISTANT_TEXT_BYTES, 256 * 1_024);
}

#[test]
fn scripted_descriptor_is_exactly_closed_for_turn_controls() {
    let driver = ScriptedDriver::new(vec![]);
    assert_eq!(
        driver.descriptor.request_capabilities.tool_choices,
        [ToolChoiceKind::Required, ToolChoiceKind::Auto]
            .into_iter()
            .collect::<BTreeSet<_>>()
    );
    assert_eq!(
        driver.descriptor.request_capabilities.parallel_tool_calls,
        [ParallelToolCalls::Forbid]
            .into_iter()
            .collect::<BTreeSet<_>>()
    );
}

#[test]
fn warning_fixture_remains_a_valid_nonterminal_event() {
    ModelEvent::ProviderWarning {
        warning: ProviderWarning {
            code: Some("bounded".into()),
            message: "bounded warning".into(),
        },
    }
    .validate()
    .expect("valid warning");
}

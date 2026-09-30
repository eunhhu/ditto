//! Built CLI clients (`ditto chat`, `ditto telegram`) against the real daemon
//! router with a deterministic model; the gateway also meets a mock Bot API
//! and the real scheduler.
use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{
    Json, Router,
    extract::{Path as UrlPath, State},
    routing::post,
};
use chrono::{DateTime, Utc};
use ditto_kernel::{DittoKernel, KernelConfig};
use ditto_model::{
    CancellationToken, ContentPart, ConversationItem, DriverDescriptor, DriverId, FinishReason,
    ModelDriver, ModelEvent, ModelEventStream, ModelFeature, ModelRequest, ParallelToolCalls,
    RequestCapabilities, ToolChoiceKind,
};
use ditto_protocol::{EventQuery, MemoryQuery, ScheduleRunCommand};
use serde_json::{Value, json};

use super::{AppState, api_routes};

const TOKEN: &str = "123456:TEST_ONLY_not_a_real_token";
const SECRET: &str = "TEST_ONLY_not_a_real_token";

/// Answers `Echo: <question>` in two deltas; a question containing "wait"
/// never finishes, so only cancellation ends it. Requests are recorded.
struct EchoDriver {
    descriptor: DriverDescriptor,
    calls: AtomicUsize,
    requests: Mutex<Vec<ModelRequest>>,
}

impl EchoDriver {
    fn new() -> Self {
        Self {
            descriptor: DriverDescriptor {
                id: DriverId::new("telegram-test").unwrap(),
                request_capabilities: RequestCapabilities {
                    tool_choices: [ToolChoiceKind::Auto].into_iter().collect(),
                    parallel_tool_calls: [ParallelToolCalls::Forbid].into_iter().collect(),
                    ..Default::default()
                },
                emitted_features: [ModelFeature::Text, ModelFeature::ToolCalls]
                    .into_iter()
                    .collect(),
            },
            calls: AtomicUsize::new(0),
            requests: Mutex::default(),
        }
    }
}

impl ModelDriver for EchoDriver {
    fn descriptor(&self) -> &DriverDescriptor {
        &self.descriptor
    }

    fn stream(&self, request: ModelRequest, _: CancellationToken) -> ModelEventStream {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests.lock().unwrap().push(request.clone());
        let question = request
            .turn
            .conversation
            .iter()
            .rev()
            .find_map(|item| match item {
                ConversationItem::Message { content, .. } => {
                    content.iter().find_map(|part| match part {
                        ContentPart::Text { text } => Some(text.clone()),
                        _ => None,
                    })
                }
                _ => None,
            })
            .unwrap_or_default();
        // Like a model, answer the user's words, not Ditto's leading time note.
        let question = match question.split_once("]\n\n") {
            Some((note, words)) if note.starts_with("[Ditto:") => words.to_owned(),
            _ => question,
        };
        ModelEventStream::new(async_stream::stream! {
            if question.contains("wait") {
                std::future::pending::<()>().await;
            }
            yield ModelEvent::TextDelta { text: "Echo: ".into() };
            yield ModelEvent::TextDelta { text: question };
            yield ModelEvent::Completed { finish_reason: FinishReason::EndTurn, continuation: None };
        })
    }
}

/// Mock Bot API: queued updates for long polling, recorded outgoing calls.
#[derive(Default)]
struct Telegram {
    updates: Mutex<Vec<Value>>,
    calls: Mutex<Vec<(String, Value)>>,
}

async fn bot_api(
    State(telegram): State<Arc<Telegram>>,
    UrlPath((bot, method)): UrlPath<(String, String)>,
    Json(body): Json<Value>,
) -> Json<Value> {
    if bot != format!("bot{TOKEN}") {
        return Json(json!({"ok": false, "error_code": 401, "description": "Unauthorized"}));
    }
    let result = match method.as_str() {
        "getMe" => {
            json!({"id": 1, "is_bot": true, "first_name": "Ditto", "username": "ditto_test_bot"})
        }
        "getUpdates" => {
            let offset = body["offset"].as_i64().unwrap_or(0);
            let mut ready = Vec::new();
            for _ in 0..10 {
                ready = telegram
                    .updates
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|update| update["update_id"].as_i64().unwrap() >= offset)
                    .cloned()
                    .collect();
                if !ready.is_empty() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            json!(ready)
        }
        _ => {
            telegram.calls.lock().unwrap().push((method, body));
            json!({"message_id": 1})
        }
    };
    Json(json!({"ok": true, "result": result}))
}

fn private(update: i64, message: i64, user: i64, language: &str, text: &str) -> Value {
    json!({"update_id": update, "message": {
        "message_id": message, "date": 1_790_000_000 + message,
        "chat": {"id": user, "type": "private"},
        "from": {"id": user, "is_bot": false, "first_name": "User", "language_code": language},
        "text": text,
    }})
}

fn sent(telegram: &Telegram, chat: i64) -> Vec<Value> {
    telegram
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(method, body)| method == "sendMessage" && body["chat_id"] == chat)
        .map(|(_, body)| body.clone())
        .collect()
}

async fn wait_for(what: &str, mut ready: impl FnMut() -> bool) {
    for _ in 0..300 {
        if ready() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for {what}");
}

fn built_cli() -> std::path::PathBuf {
    let cli = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("ditto");
    assert!(cli.is_file(), "build ditto-cli before this smoke test");
    cli
}

/// The real daemon router on loopback with `driver`, plus its scheduler.
async fn daemon(
    root: &Path,
    driver: Arc<dyn ModelDriver>,
) -> (
    DittoKernel,
    String,
    CancellationToken,
    tokio::task::JoinHandle<()>,
) {
    let kernel = DittoKernel::open(KernelConfig::new(
        root.join("data"),
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../capabilities"),
    ))
    .unwrap();
    let shutdown = CancellationToken::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api = format!("http://{}", listener.local_addr().unwrap());
    let app = api_routes(true).with_state(AppState {
        kernel: kernel.clone(),
        driver: Some(driver.clone()),
        shutdown: shutdown.clone(),
    });
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let scheduler = {
        let (kernel, shutdown) = (kernel.clone(), shutdown.clone());
        tokio::spawn(async move {
            let _ = kernel.run_scheduler(Some(driver), shutdown).await;
        })
    };
    (kernel, api, shutdown, scheduler)
}

fn gateway(api: &str, telegram_api: &str, root: &Path) -> std::process::Child {
    let cli = built_cli();
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join("gateway.log"))
        .unwrap();
    std::process::Command::new(cli)
        .args([
            "--api",
            api,
            "telegram",
            "--allow-user",
            "42",
            "--telegram-api",
        ])
        .arg(telegram_api)
        .arg("--state-file")
        .arg(root.join("telegram.json"))
        .env("DITTO_TELEGRAM_BOT_TOKEN", TOKEN)
        .env_remove("DITTO_TELEGRAM_ALLOWED_USERS")
        .stdout(log.try_clone().unwrap())
        .stderr(log)
        .spawn()
        .unwrap()
}

#[tokio::test]
#[ignore = "requires cargo build -p ditto-cli; runs the actual Telegram gateway against a mock Bot API"]
async fn built_cli_telegram_gateway_relays_allowed_chats_and_scheduled_results() {
    let root = tempfile::tempdir().unwrap();
    let echo = Arc::new(EchoDriver::new());
    let (kernel, api, shutdown, scheduler) = daemon(root.path(), echo.clone()).await;

    let telegram = Arc::new(Telegram::default());
    let mock = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let telegram_api = format!("http://{}", mock.local_addr().unwrap());
    let bot = Router::new()
        .route("/{bot}/{method}", post(bot_api))
        .with_state(telegram.clone());
    tokio::spawn(async move { axum::serve(mock, bot).await.unwrap() });
    let mut child = gateway(&api, &telegram_api, root.path());
    let push = |update: Value| telegram.updates.lock().unwrap().push(update);

    // An allowed private message is answered, streamed through a draft.
    push(private(1, 10, 42, "en", "hello there"));
    wait_for("the first answer", || {
        sent(&telegram, 42)
            .iter()
            .any(|body| body["text"] == "Echo: hello there")
    })
    .await;
    let answer = sent(&telegram, 42).remove(0);
    assert_eq!(answer["reply_parameters"]["message_id"], 10);
    assert!(telegram.calls.lock().unwrap().iter().any(|(method, body)| {
        method == "sendMessageDraft"
            && body["chat_id"] == 42
            && body["draft_id"] == 10
            && body["can_stop"] == true
    }));

    // Other users and group chats are ignored without a reply.
    push(private(2, 20, 43, "en", "intruder"));
    push(json!({"update_id": 3, "message": {
        "message_id": 21, "date": 1_790_000_021,
        "chat": {"id": -100, "type": "group"},
        "from": {"id": 42, "is_bot": false, "first_name": "User"},
        "text": "group hello",
    }}));
    push(private(4, 11, 42, "ko", "/new"));
    wait_for("the Korean reset reply", || {
        sent(&telegram, 42)
            .iter()
            .any(|body| body["text"] == "새 대화를 시작했습니다.")
    })
    .await;
    assert!(
        telegram
            .calls
            .lock()
            .unwrap()
            .iter()
            .all(|(_, body)| body["chat_id"] == 42)
    );

    // Memories are saved through the daemon's typed commands.
    push(private(5, 12, 42, "en", "/remember I like green tea"));
    wait_for("the memory reply", || {
        sent(&telegram, 42)
            .iter()
            .any(|body| body["text"] == "Saved to memory.")
    })
    .await;
    let memories = kernel
        .list_memories(MemoryQuery {
            session_id: "personal".into(),
            after_id: None,
            limit: None,
        })
        .unwrap();
    assert!(
        memories
            .memories
            .iter()
            .any(|memory| memory.text == "I like green tea")
    );

    // The draft's stop button cancels the answer in progress.
    push(private(6, 13, 42, "en", "please wait for me"));
    wait_for("the waiting draft", || {
        telegram
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|(method, body)| method == "sendMessageDraft" && body["draft_id"] == 13)
    })
    .await;
    push(json!({"update_id": 7, "stopped_message_generation": {
        "chat": {"id": 42, "type": "private"}, "draft_id": 13,
    }}));
    wait_for("the stop reply", || {
        sent(&telegram, 42)
            .iter()
            .any(|body| body["text"] == "Stopped." && body["reply_parameters"]["message_id"] == 13)
    })
    .await;

    // A scheduled run's result is delivered with its request.
    let due = DateTime::from_timestamp_millis(Utc::now().timestamp_millis() + 2_000).unwrap();
    kernel
        .schedule_run(ScheduleRunCommand {
            request_id: ulid::Ulid::new().to_string(),
            session_id: "personal".into(),
            text: "Remind me to water the plants".into(),
            due_at: due,
            expires_at: due + chrono::Duration::minutes(5),
        })
        .unwrap();
    let delivery = "⏰ Remind me to water the plants\n\nEcho: Remind me to water the plants";
    wait_for("the scheduled delivery", || {
        sent(&telegram, 42)
            .iter()
            .any(|body| body["text"] == delivery)
    })
    .await;

    // A run from another client is not pushed to Telegram.
    let client = reqwest::Client::new();
    let direct = ulid::Ulid::new().to_string();
    let accepted = client
        .post(format!("{api}/v1/commands/run"))
        .json(&json!({"request_id": direct, "session_id": "personal", "text": "from the terminal"}))
        .send()
        .await
        .unwrap();
    assert!(accepted.status().is_success());
    let finished_seq = || {
        kernel
            .list_events(&EventQuery {
                session_id: Some("personal".into()),
                task_id: Some(format!("run_{direct}")),
                limit: Some(1_000),
                ..Default::default()
            })
            .unwrap()
            .iter()
            .find(|event| event.kind == "turn.finished")
            .map(|event| event.seq)
    };
    wait_for("the terminal run", || finished_seq().is_some()).await;
    let state = root.path().join("telegram.json");
    wait_for("the gateway cursor to pass the terminal run", || {
        std::fs::read(&state)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .and_then(|value| value["after_seq"].as_i64())
            .is_some_and(|seq| Some(seq) >= finished_seq())
    })
    .await;
    assert!(
        sent(&telegram, 42)
            .iter()
            .all(|body| !body["text"].as_str().unwrap().contains("from the terminal"))
    );

    // After a restart a redelivered message retries the same run, and the
    // scheduled result is not delivered twice.
    child.kill().unwrap();
    child.wait().unwrap();
    let model_calls = echo.calls.load(Ordering::SeqCst);
    let answers = sent(&telegram, 42).len();
    let mut child = gateway(&api, &telegram_api, root.path());
    push(private(8, 10, 42, "en", "hello there"));
    wait_for("the repeated answer", || {
        sent(&telegram, 42).len() > answers
    })
    .await;
    assert_eq!(
        sent(&telegram, 42).last().unwrap()["text"],
        "Echo: hello there"
    );
    assert_eq!(echo.calls.load(Ordering::SeqCst), model_calls);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        sent(&telegram, 42)
            .iter()
            .filter(|body| body["text"] == delivery)
            .count(),
        1
    );
    child.kill().unwrap();
    child.wait().unwrap();

    // The token stays in the gateway: not in its output or in daemon events.
    let log = std::fs::read_to_string(root.path().join("gateway.log")).unwrap();
    assert!(!log.contains(SECRET), "{log}");
    let events = serde_json::to_string(
        &kernel
            .list_events(&EventQuery {
                limit: Some(1_000),
                ..Default::default()
            })
            .unwrap(),
    )
    .unwrap();
    assert!(!events.contains(SECRET));
    assert!(!events.contains("intruder") && !events.contains("group hello"));
    shutdown.cancel();
    kernel.shutdown_agent_runs().await.unwrap();
    let _ = scheduler.await;
}

#[tokio::test]
#[ignore = "requires cargo build -p ditto-cli; runs the actual ditto chat"]
async fn built_cli_chat_threads_remembers_and_resets() {
    let root = tempfile::tempdir().unwrap();
    let echo = Arc::new(EchoDriver::new());
    let (kernel, api, shutdown, scheduler) = daemon(root.path(), echo.clone()).await;
    let input = "hello\n/remember I like green tea\nsecond\n/new\nthird\n/exit\n";
    let output = tokio::task::spawn_blocking(move || {
        use std::io::Write;
        let mut child = std::process::Command::new(built_cli())
            .args(["--api", &api, "chat"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    })
    .await
    .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");
    for expected in [
        "ditto> Echo: hello",
        "-- saved to memory --",
        "ditto> Echo: second",
        "-- new conversation --",
        "ditto> Echo: third",
    ] {
        assert!(
            stdout.contains(expected),
            "{expected} missing from {stdout}"
        );
    }

    // The follow-up carried the first exchange; after /new only the question.
    let conversation = |index: usize| echo.requests.lock().unwrap()[index].turn.conversation.len();
    assert_eq!(echo.calls.load(Ordering::SeqCst), 3);
    assert_eq!(
        (conversation(0), conversation(1), conversation(2)),
        (1, 3, 1)
    );
    // The memory saved mid-chat reached the next request's context.
    let context = serde_json::to_string(&echo.requests.lock().unwrap()[1].turn.context).unwrap();
    assert!(context.contains("I like green tea"));
    let memories = kernel
        .list_memories(MemoryQuery {
            session_id: "personal".into(),
            after_id: None,
            limit: None,
        })
        .unwrap();
    assert_eq!(memories.memories.len(), 1);
    shutdown.cancel();
    kernel.shutdown_agent_runs().await.unwrap();
    let _ = scheduler.await;
}

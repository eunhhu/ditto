//! `web.search` (ADR 0033): Ditto searches on its own, only at the configured
//! service, at most three times a turn, never with a secret, and replays
//! without network I/O.
use super::agent_runs::{start_command, terminal};
use super::*;
use ditto_protocol::{AgentRunResponse, AgentRunStatus};
use std::sync::Mutex as StdMutex;

/// A SearXNG-shaped service on loopback: it answers every request with
/// `status` and `body`, and records the request targets it saw.
struct SearchService {
    endpoint: String,
    targets: Arc<StdMutex<Vec<String>>>,
}

fn serve_search(status: u16, body: String) -> SearchService {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let targets = Arc::new(StdMutex::new(Vec::new()));
    let seen = targets.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1_024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                match stream.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => request.extend_from_slice(&buffer[..read]),
                }
            }
            let line = String::from_utf8_lossy(&request);
            let target = line
                .split_whitespace()
                .nth(1)
                .unwrap_or_default()
                .to_owned();
            seen.lock().unwrap().push(target);
            let response = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    SearchService { endpoint, targets }
}

impl SearchService {
    fn targets(&self) -> Vec<String> {
        self.targets.lock().unwrap().clone()
    }
}

fn results(count: usize) -> String {
    let results = (0..count)
        .map(|index| {
            json!({
                "url": format!("https://news.example/rust-{index}"),
                "title": format!("Rust news {index}"),
                "content": format!("Rust 2.{index} was released."),
            })
        })
        .collect::<Vec<_>>();
    json!({ "query": "rust", "results": results }).to_string()
}

fn search(query: &str) -> Vec<ModelEvent> {
    tool_request_script(
        &format!("search-{}", ulid::Ulid::new().to_string().to_lowercase()),
        "web.search",
        json!({ "query": query }),
        FinishReason::ToolCalls,
    )
}

async fn ask(
    fixture: &Fixture,
    text: &str,
    scripts: Vec<Vec<ModelEvent>>,
) -> (AgentRunResponse, ScriptedDriver) {
    let driver = ScriptedDriver::new(scripts);
    let command = start_command(text);
    fixture
        .kernel
        .start_agent_run(command.clone(), Arc::new(driver.clone()))
        .unwrap();
    let status = terminal(&fixture.kernel, &command).await;
    fixture.kernel.shutdown_agent_runs().await.unwrap();
    (status, driver)
}

fn tool_results(request: &ModelRequest) -> Vec<Value> {
    request
        .turn
        .conversation
        .iter()
        .filter_map(|item| match item {
            ConversationItem::ToolResult { content, .. } => {
                content.iter().find_map(|part| match part {
                    ContentPart::Structured { value } => Some(value.clone()),
                    _ => None,
                })
            }
            _ => None,
        })
        .collect()
}

fn tool_ids(request: &ModelRequest) -> Vec<String> {
    request.tools.iter().map(|tool| tool.id.clone()).collect()
}

#[tokio::test]
async fn a_search_runs_on_its_own_and_replays_without_network() {
    let service = serve_search(200, results(7));
    let fixture = Fixture::with_web_search(&service.endpoint);
    let (status, driver) = ask(
        &fixture,
        "What is new in Rust?",
        vec![
            search("  rust release & news "),
            final_script(&["Rust 2.0."]),
        ],
    )
    .await;
    assert_eq!(status.status, AgentRunStatus::Unverified, "{status:?}");
    let requests = driver.requests();
    assert_eq!(
        tool_ids(&requests[0]),
        [
            "artifact.read",
            "web.fetch",
            "web.search",
            "memory.search",
            "memory.remember",
            "memory.forget"
        ]
    );
    // No approval: the trimmed query went to the configured service once.
    assert_eq!(
        service.targets(),
        ["/search?q=rust+release+%26+news&format=json"]
    );
    let found = &tool_results(&requests[1])[0];
    assert_eq!(found["content_origin"], "untrusted search results");
    let hits = found["results"].as_array().unwrap();
    assert_eq!(hits.len(), 5);
    assert_eq!(hits[0]["url"], "https://news.example/rust-0");
    assert_eq!(hits[0]["snippet"], "Rust 2.0 was released.");

    let events = fixture.events_for_session("personal");
    let replay = replay_artifact_read_turn(&events, &status.turn_id).unwrap();
    assert_eq!(replay.search_calls.len(), 1);
    assert_eq!(service.targets().len(), 1, "replay must not search");

    // The recorded results are what the model read, and the start names the
    // query's request at the service.
    let position = |kind: &str| events.iter().position(|event| event.kind == kind).unwrap();
    let rejects = |forged: Vec<EventRecord>| {
        assert!(replay_artifact_read_turn(&forged, &status.turn_id).is_err());
    };
    let mut forged = events.clone();
    forged[position(event_kind::AGENT_SEARCH_OUTPUT)].payload["result"]["results"][0]["title"] =
        json!("Rust is cancelled");
    rejects(forged);
    let mut forged = events.clone();
    forged[position(event_kind::AGENT_SEARCH_STARTED)].payload["request_url"] = json!(format!(
        "{}/search?q=elsewhere&format=json",
        service.endpoint
    ));
    rejects(forged);
    let mut forged = events.clone();
    forged.remove(position(event_kind::AGENT_SEARCH_STARTED));
    rejects(forged);
    let mut forged = events.clone();
    forged[position(event_kind::CAPABILITIES_SELECTED)].payload["contracts"]
        .as_array_mut()
        .unwrap()
        .retain(|contract| contract["capability_id"] != "web.search");
    rejects(forged);
}

#[tokio::test]
async fn searches_are_bounded_and_a_secret_is_never_sent() {
    let service = serve_search(200, results(2));
    let fixture = Fixture::with_web_search(&service.endpoint);
    let (status, driver) = ask(
        &fixture,
        "Look these up.",
        vec![
            search(&format!("my wifi password is {}", "hunter2")),
            search("   "),
            search("one"),
            search("two"),
            search("three"),
            search("four"),
            final_script(&["Done."]),
        ],
    )
    .await;
    assert_eq!(status.status, AgentRunStatus::Unverified, "{status:?}");
    let results = tool_results(&driver.requests()[6]);
    assert_eq!(results[0], json!({"error": "credential"}));
    assert_eq!(results[1], json!({"error": "invalid_arguments"}));
    for found in &results[2..5] {
        assert_eq!(found["results"].as_array().unwrap().len(), 2, "{found}");
    }
    assert_eq!(results[5], json!({"error": "lease_exhausted"}));
    assert_eq!(service.targets().len(), 3);
    assert!(
        service
            .targets()
            .iter()
            .all(|target| !target.contains("hunter2"))
    );
    replay_artifact_read_turn(&fixture.events_for_session("personal"), &status.turn_id).unwrap();
}

#[tokio::test]
async fn service_failures_reach_the_model_as_errors() {
    for (status_code, body, expected) in [
        (
            500,
            "{}".to_owned(),
            json!({"error": "http_status", "status": 500}),
        ),
        (
            200,
            "not json".to_owned(),
            json!({"error": "invalid_response"}),
        ),
    ] {
        let service = serve_search(status_code, body);
        let fixture = Fixture::with_web_search(&service.endpoint);
        let (status, driver) = ask(
            &fixture,
            "Search something.",
            vec![search("anything"), final_script(&["It failed."])],
        )
        .await;
        assert_eq!(status.status, AgentRunStatus::Unverified, "{status:?}");
        assert_eq!(tool_results(&driver.requests()[1])[0], expected);
        replay_artifact_read_turn(&fixture.events_for_session("personal"), &status.turn_id)
            .unwrap();
    }
}

#[tokio::test]
async fn nothing_is_remembered_after_a_search() {
    let service = serve_search(200, results(1));
    let fixture = Fixture::with_web_search(&service.endpoint);
    let (status, driver) = ask(
        &fixture,
        "Find my favourite team's score and remember it.",
        vec![
            search("team score"),
            tool_request_script(
                "remember-1",
                "memory.remember",
                json!({"text": "The user's team won."}),
                FinishReason::ToolCalls,
            ),
            final_script(&["I can't save that from search results."]),
        ],
    )
    .await;
    assert_eq!(status.status, AgentRunStatus::Unverified, "{status:?}");
    assert_eq!(
        tool_results(&driver.requests()[2])[1],
        json!({"error": "untrusted_content_read"})
    );
    replay_artifact_read_turn(&fixture.events_for_session("personal"), &status.turn_id).unwrap();
}

#[tokio::test]
async fn without_a_search_service_the_tool_is_not_offered() {
    let fixture = Fixture::new();
    let (status, driver) = ask(&fixture, "Search the web.", vec![search("anything")]).await;
    assert!(!tool_ids(&driver.requests()[0]).contains(&"web.search".to_owned()));
    assert_eq!(status.status, AgentRunStatus::Failed);
    assert_eq!(status.failure_code.as_deref(), Some("protocol"));
    replay_artifact_read_turn(&fixture.events_for_session("personal"), &status.turn_id).unwrap();
}

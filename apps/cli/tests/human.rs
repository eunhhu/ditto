use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::TcpListener,
    process::Command,
    sync::mpsc,
    time::Duration,
};

fn cli(command: &str, body: Value, human: bool) -> std::process::Output {
    let api = serve(body);
    let mut child = Command::new(env!("CARGO_BIN_EXE_ditto"));
    child.args(["--api", &api, command, "request", "--session", "personal"]);
    if human {
        child.arg("--human");
    }
    child.output().unwrap()
}

fn serve(body: Value) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let api = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0; 4096];
        assert!(stream.read(&mut request).unwrap() > 0);
        let body = body.to_string();
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
    });
    api
}

fn serve_and_capture(body: Value, requests: usize) -> (String, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let api = format!("http://{}", listener.local_addr().unwrap());
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for _ in 0..requests {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            let read = stream.read(&mut request).unwrap();
            assert!(read > 0);
            sender
                .send(String::from_utf8_lossy(&request[..read]).into_owned())
                .unwrap();
            let body = body.to_string();
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
        }
    });
    (api, receiver)
}

fn run(status: &str) -> Value {
    json!({"request_id":"request", "session_id":"personal", "task_id":"run_task",
        "turn_id":"turn", "status":status, "cancellation_requested":false})
}

#[test]
fn default_json_is_preserved() {
    let body = run("unverified");
    let output = cli("run-status", body.clone(), false);
    assert!(output.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        body
    );
}

#[test]
fn human_keeps_failed_model_and_verified_sort_independent_and_escapes_content() {
    let mut body = run("failed");
    body["failure_code"] = json!("provider_failed\u{1b}[2J\r");
    body["sort"] = json!({"input_reference":"artifact:sha256:input", "allow_deduplicate":false,
        "state":"verified", "output_reference":"artifact:sha256:output", "output":"a\nb\u{1b}]52;c;evil\u{7}\u{9b}2J\u{202e}"});
    let output = cli("run-status", body, true);
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(!output.status.success());
    for required in [
        "Model: failed",
        "Sort: verified",
        "deduplication not allowed",
        "artifact:sha256:input",
        "artifact:sha256:output",
        "run-status",
        "--session",
        "personal",
    ] {
        assert!(
            text.contains(required),
            "missing {required}: {text} / {:?}",
            output.stderr
        );
    }
    assert!(
        !text
            .chars()
            .any(|c| (c.is_control() && c != '\n') || c == '\u{202e}')
    );
}

#[test]
fn human_answers_are_unverified_and_cancellation_request_is_not_terminal() {
    let mut body = run("running");
    body["cancellation_requested"] = json!(true);
    let output = cli("run-cancel", body, true);
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(output.status.success(), "{:?}", output.stderr);
    assert!(
        text.contains("Cancellation: requested; terminal cancellation not confirmed"),
        "{text}"
    );
    let mut body = run("failed");
    body["failure_code"] = json!("cancelled");
    let text = String::from_utf8(cli("run-status", body.clone(), true).stdout).unwrap();
    assert!(text.contains("Cancellation: terminal"), "{text}");

    let schedule = json!({
        "request_id":"scheduled", "session_id":"personal",
        "due_at":"2026-09-17T00:00:00Z", "expires_at":"2026-09-17T00:01:00Z",
        "status":"failed", "cancellation_requested":true, "run":body
    });
    let text = String::from_utf8(cli("schedule-status", schedule, true).stdout).unwrap();
    assert!(
        text.contains("Schedule cancellation: terminal; confirmed by child run"),
        "{text}"
    );
    assert!(
        !text.contains("terminal cancellation not confirmed"),
        "{text}"
    );
    assert!(text.contains("Cancellation: terminal"), "{text}");

    let mut body = run("unverified");
    body["response"] = json!("answer\u{1b}[31m");
    let text = String::from_utf8(cli("run-status", body, true).stdout).unwrap();
    assert!(text.contains("Model answer (unverified): answer"), "{text}");
    assert!(!text.contains('\u{1b}'));
}

#[test]
fn human_repeat_exhaustion_retains_child_failure_and_recovery_identity() {
    let child = json!({"request_id":"child", "session_id":"personal",
        "due_at":"2026-09-17T00:00:00Z", "expires_at":"2026-09-17T00:01:00Z",
        "status":"interrupted", "cancellation_requested":false, "run":run("interrupted")});
    let body = json!({"request_id":"parent", "session_id":"personal",
        "due_at":"2026-09-17T00:00:00Z", "expires_at":"2026-09-17T00:01:00Z",
        "every_seconds":60, "occurrences":3, "status":"exhausted",
        "claimed_occurrences":1, "missed_occurrences":2, "last_occurrence_id":"child", "last_occurrence":child});
    let output = cli("repeat-status", body, true);
    assert!(output.status.success(), "{:?}", output.stderr); // Preserve existing exit semantics.
    let text = String::from_utf8(output.stdout).unwrap();
    for required in [
        "Repeat: exhausted",
        "not a success claim",
        "claimed: 1",
        "missed: 2",
        "Schedule: interrupted",
        "Model: interrupted",
        "schedule-status 'child'",
        "repeat-status 'parent'",
    ] {
        assert!(text.contains(required), "missing {required}: {text}");
    }
    assert!(!text.contains("completed successfully"));
}

#[test]
fn human_pending_schedule_explains_wait_and_sort_status_preserves_evidence() {
    let body = json!({"request_id":"scheduled", "session_id":"personal",
        "due_at":"2026-09-17T00:00:00Z", "expires_at":"2026-09-17T00:01:00Z",
        "status":"pending", "cancellation_requested":false, "waiting_for":"provider_disabled"});
    let text = String::from_utf8(cli("schedule-status", body, true).stdout).unwrap();
    assert!(text.contains("provider disabled"), "{text}");
    assert!(text.contains("2026-09-17"), "{text}");
    let mut body = run("verified");
    body["output"] = json!("a\nb\n");
    body["output_reference"] = json!("artifact:sha256:output");
    let text = String::from_utf8(cli("sort-status", body, true).stdout).unwrap();
    assert!(text.contains("Sort: verified"), "{text}");
    assert!(text.contains("line ordering and multiplicity"), "{text}");
}

#[test]
fn human_denied_or_interrupted_sort_never_becomes_verified_by_model_answer() {
    for state in ["not_run", "running", "failed", "interrupted"] {
        let mut body = run("unverified");
        body["response"] = json!("I completed the sort successfully.");
        body["sort"] = json!({"input_reference":"exact-input", "allow_deduplicate":true,
            "state":state, "failure_code":"permission_denied"});
        let output = cli("run-status", body, true);
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(output.status.success());
        assert!(
            text.contains(&format!("Sort: {state}")) && text.contains("Model answer (unverified)"),
            "{text}"
        );
        assert!(text.contains("deduplication allowed") && text.contains("permission_denied"));
        assert!(!text.contains("Sort: verified"));
    }
}

#[test]
fn malformed_status_fails_without_terminal_injection() {
    let mut body = run("running");
    body["status"] = json!("forged\u{1b}[2J\r\u{202e}");
    let output = cli("run-status", body, true);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(!error.contains(['\u{1b}', '\r', '\u{202e}']), "{error}");
}

#[test]
fn uncertain_submission_prints_safe_recoverable_identity_before_transport() {
    let output = Command::new(env!("CARGO_BIN_EXE_ditto"))
        .args([
            "--api",
            "http://127.0.0.1:0",
            "run",
            "hello",
            "--human",
            "--request-id",
            "id'\u{1b}[2J",
            "--session",
            "session\r\u{202e}",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8(output.stderr).unwrap();
    for required in [
        "request:",
        "Inspect: ditto",
        "run-status",
        "--session",
        "identical request",
    ] {
        assert!(error.contains(required), "{error}");
    }
    assert!(!error.contains(['\u{1b}', '\r', '\u{202e}']), "{error}");
}

#[test]
fn parser_errors_escape_untrusted_arguments_and_keep_exit_code_two() {
    let output = Command::new(env!("CARGO_BIN_EXE_ditto"))
        .args(["--human", "run-status", "request", "bad\u{1b}[2J\r\u{202e}"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(!error.contains(['\u{1b}', '\r', '\u{202e}']), "{error:?}");
    for arg in ["--help", "--version"] {
        let help = Command::new(env!("CARGO_BIN_EXE_ditto"))
            .arg(arg)
            .env("DITTO_API", "untrusted\r\u{202e}")
            .output()
            .unwrap();
        assert!(help.status.success() && help.stderr.is_empty() && !help.stdout.is_empty());
        let text = String::from_utf8(help.stdout).unwrap();
        assert!(!text.contains(['\r', '\u{202e}']), "{text:?}");
    }
}

#[test]
fn recovery_redacts_url_userinfo_in_human_and_default_json_submissions() {
    // Deliberately synthetic userinfo, including percent-encoded delimiters.
    let userinfo = "synthetic%40user:synthetic%3Apassword";
    for human in [false, true] {
        let api = serve(run("unverified"));
        let authenticated = api.replacen("http://", &format!("http://{userinfo}@"), 1);
        let mut command = Command::new(env!("CARGO_BIN_EXE_ditto"));
        command.args([
            "--api",
            &authenticated,
            "run",
            "hello",
            "--detach",
            "--request-id",
            "request",
        ]);
        if human {
            command.arg("--human");
        }
        let output = command.output().unwrap();
        assert!(output.status.success(), "{:?}", output.stderr);
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        for text in [&stdout, &stderr] {
            assert!(!text.contains("synthetic"), "{text}");
        }
        assert!(stderr.contains(&api), "{stderr}");
        assert!(stderr.contains("userinfo removed"), "{stderr}");
        if human {
            assert!(stdout.contains(&api), "{stdout}");
        } else {
            assert_eq!(
                serde_json::from_str::<Value>(&stdout).unwrap(),
                run("unverified")
            );
        }
    }
}

#[test]
fn recovery_command_preserves_terminal_sensitive_session_through_shell_and_parser() {
    let request_id = "01K5CP3YVQ8WRV0Y1GH3KJ7M9N";
    let session = "-personal's $(exit 42)\u{2028}middle\u{2029}end";
    let mut body = run("unverified");
    body["request_id"] = json!(request_id);
    body["session_id"] = json!(session);
    let (api, requests) = serve_and_capture(body, 2);
    let output = Command::new(env!("CARGO_BIN_EXE_ditto"))
        .args([
            "--api",
            &api,
            "run",
            "hello",
            "--detach",
            "--request-id",
            request_id,
            &format!("--session={session}"),
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output.stderr);
    let stderr = String::from_utf8(output.stderr).unwrap();
    let recovery = stderr
        .lines()
        .find_map(|line| line.strip_prefix("Inspect: ditto "))
        .unwrap();
    // Run the emitted command through a real POSIX shell and the real parser.
    let result = Command::new("/bin/sh")
        .args([
            "-c",
            &format!("exec \"$1\" {recovery}"),
            "recovery",
            env!("CARGO_BIN_EXE_ditto"),
        ])
        .output()
        .unwrap();
    let error = String::from_utf8(result.stderr).unwrap();
    assert!(result.status.success(), "{error}");
    assert!(recovery.contains("--session="), "{recovery}");
    assert!(recovery.is_ascii(), "{recovery:?}");

    let _submission = requests.recv_timeout(Duration::from_secs(1)).unwrap();
    let inspection = requests.recv_timeout(Duration::from_secs(1)).unwrap();
    let target = inspection
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap();
    let url = reqwest::Url::parse(&format!("http://localhost{target}")).unwrap();
    let query = url
        .query_pairs()
        .collect::<std::collections::HashMap<_, _>>();
    assert_eq!(
        query.get("request_id").map(|value| value.as_ref()),
        Some(request_id)
    );
    assert_eq!(
        query.get("session_id").map(|value| value.as_ref()),
        Some(session)
    );
}

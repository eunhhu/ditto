//! Embedded local web app and the loopback host guard.
//!
//! The page is compiled into the daemon, loads nothing from other origins and
//! holds no authority: it calls the same typed HTTP API as the CLI.
use axum::{
    Json, Router,
    extract::Request,
    http::{StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::get,
};

use super::AppState;

const INDEX_HTML: &str = include_str!("web/index.html");
const APP_JS: &str = include_str!("web/app.js");
const APP_CSS: &str = include_str!("web/app.css");
const FAVICON_SVG: &str = include_str!("web/favicon.svg");

/// Same-origin scripts, styles and API calls only; no inline code, framing,
/// form posts or plugins.
pub(super) const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; script-src 'self'; \
     style-src 'self'; connect-src 'self'; img-src 'self'; base-uri 'none'; \
     form-action 'none'; frame-ancestors 'none'";

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/",
            get(|| async { asset("text/html; charset=utf-8", INDEX_HTML) }),
        )
        .route(
            "/app.js",
            get(|| async { asset("text/javascript; charset=utf-8", APP_JS) }),
        )
        .route(
            "/app.css",
            get(|| async { asset("text/css; charset=utf-8", APP_CSS) }),
        )
        .route(
            "/favicon.svg",
            get(|| async { asset("image/svg+xml", FAVICON_SVG) }),
        )
}

fn asset(content_type: &'static str, body: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CONTENT_SECURITY_POLICY, CONTENT_SECURITY_POLICY),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::X_FRAME_OPTIONS, "DENY"),
            (header::REFERRER_POLICY, "no-referrer"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        body,
    )
        .into_response()
}

/// While bound to loopback, serve only requests addressed to a loopback name.
/// A page whose DNS name was rebound to 127.0.0.1 still sends its own name,
/// so it cannot drive the API or read the page.
pub(super) async fn loopback_host_only(request: Request, next: Next) -> Response {
    let host = request
        .uri()
        .authority()
        .map(|authority| authority.host().to_owned())
        .or_else(|| {
            request
                .headers()
                .get(header::HOST)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<axum::http::uri::Authority>().ok())
                .map(|authority| authority.host().to_owned())
        });
    if host.as_deref().is_some_and(is_loopback_name) {
        return next.run(request).await;
    }
    (
        StatusCode::FORBIDDEN,
        Json(serde_json::json!({ "error": "requests must address a loopback host" })),
    )
        .into_response()
}

fn is_loopback_name(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

#[cfg(test)]
mod tests {
    use std::{net::SocketAddr, path::Path, sync::Arc, time::Duration};

    use axum::http::header::{CONTENT_SECURITY_POLICY, CONTENT_TYPE};
    use ditto_kernel::{DittoKernel, KernelConfig};
    use ditto_model::{CancellationToken, ModelDriver};
    use ditto_protocol::{AgentRunQuery, AgentRunStatus, ConversationView, StartAgentRunCommand};

    use super::super::{AppState, api_routes};
    use crate::runs::tests::HttpDriver;

    async fn serve(
        loopback: bool,
        driver: Option<Arc<dyn ModelDriver>>,
    ) -> (tempfile::TempDir, DittoKernel, SocketAddr) {
        let root = tempfile::tempdir().unwrap();
        let kernel = DittoKernel::open(KernelConfig::new(
            root.path().join("data"),
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../capabilities"),
        ))
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = api_routes(loopback).with_state(AppState {
            kernel: kernel.clone(),
            driver,
            shutdown: CancellationToken::new(),
        });
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (root, kernel, address)
    }

    /// One raw request, so the test controls the request target and Host.
    async fn raw(address: SocketAddr, request: String) -> u16 {
        tokio::task::spawn_blocking(move || {
            use std::io::{Read, Write};
            let mut stream = std::net::TcpStream::connect(address).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            stream.write_all(request.as_bytes()).unwrap();
            let mut response = Vec::new();
            stream.read_to_end(&mut response).unwrap();
            let response = String::from_utf8_lossy(&response);
            response
                .split(' ')
                .nth(1)
                .and_then(|code| code.parse().ok())
                .unwrap_or(0)
        })
        .await
        .unwrap()
    }

    fn get(path: &str, host: &str) -> String {
        format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n")
    }

    fn post(path: &str, host: &str, body: &str) -> String {
        format!(
            "POST {path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    #[tokio::test]
    async fn web_app_assets_are_embedded_with_a_strict_policy() {
        let (_root, _kernel, address) = serve(true, None).await;
        let client = reqwest::Client::new();
        for (path, kind) in [
            ("/", "text/html"),
            ("/app.js", "text/javascript"),
            ("/app.css", "text/css"),
            ("/favicon.svg", "image/svg+xml"),
        ] {
            let response = client
                .get(format!("http://{address}{path}"))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 200, "{path}");
            let headers = response.headers();
            assert!(headers[CONTENT_TYPE].to_str().unwrap().starts_with(kind));
            assert_eq!(
                headers[CONTENT_SECURITY_POLICY],
                super::CONTENT_SECURITY_POLICY
            );
            assert_eq!(headers["x-content-type-options"], "nosniff");
            assert_eq!(headers["x-frame-options"], "DENY");
            assert_eq!(headers["referrer-policy"], "no-referrer");
        }
        let page = client
            .get(format!("http://{address}/"))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        // One same-origin script; no inline code, inline style or remote load.
        assert_eq!(page.matches("<script").count(), 1);
        assert!(page.contains(r#"<script src="/app.js" defer></script>"#));
        for forbidden in ["http://", "https://", "<style", " style="] {
            assert!(!page.contains(forbidden), "{forbidden}");
        }
        // No inline event-handler attribute such as `onclick=`.
        let handler = page.match_indices(" on").any(|(index, _)| {
            let rest = &page[index + 3..];
            let name = rest.bytes().take_while(u8::is_ascii_lowercase).count();
            name > 0 && rest.as_bytes().get(name) == Some(&b'=')
        });
        assert!(!handler);
        let missing = client
            .get(format!("http://{address}/missing.js"))
            .send()
            .await
            .unwrap();
        assert_eq!(missing.status(), 404);
    }

    #[tokio::test]
    async fn loopback_daemon_serves_only_loopback_host_names() {
        let (_root, kernel, address) = serve(true, None).await;
        let port = address.port();
        for host in [
            format!("127.0.0.1:{port}"),
            format!("localhost:{port}"),
            format!("LocalHost:{port}"),
            format!("[::1]:{port}"),
            "127.0.0.2".to_owned(),
        ] {
            assert_eq!(raw(address, get("/health", &host)).await, 200, "{host}");
        }
        let accepted = post(
            "/v1/commands/input",
            &format!("localhost:{port}"),
            r#"{"text":"local"}"#,
        );
        assert_eq!(raw(address, accepted).await, 201);
        let before = kernel.event_count().unwrap();
        // A rebound DNS name still arrives with its own name.
        for host in [
            "evil.example",
            "evil.example:8787",
            "127.0.0.1.evil.example",
            "localhost.evil.example",
            "0x7f000001",
            "[::ffff:127.0.0.1]",
        ] {
            for request in [
                get("/", host),
                get("/health", host),
                get("/v1/memories?session_id=personal", host),
                post("/v1/commands/input", host, r#"{"text":"forged"}"#),
            ] {
                assert_eq!(raw(address, request).await, 403, "{host}");
            }
        }
        let absolute = format!(
            "GET http://evil.example/health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\
             Connection: close\r\n\r\n"
        );
        assert_eq!(raw(address, absolute).await, 403);
        let no_host = "GET /health HTTP/1.0\r\n\r\n".to_owned();
        assert_ne!(raw(address, no_host).await, 200);
        assert_eq!(kernel.event_count().unwrap(), before);

        // The explicit unauthenticated-remote assembly keeps its behavior.
        let (_root, _kernel, remote) = serve(false, None).await;
        assert_eq!(raw(remote, get("/health", "ditto.lan:8787")).await, 200);
    }

    #[tokio::test]
    async fn conversation_endpoint_lists_the_current_thread() {
        let root = tempfile::tempdir().unwrap();
        let driver: Arc<dyn ModelDriver> = Arc::new(HttpDriver::new(false));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let kernel = DittoKernel::open(KernelConfig::new(
            root.path().join("data"),
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../capabilities"),
        ))
        .unwrap();
        let app = api_routes(true).with_state(AppState {
            kernel: kernel.clone(),
            driver: Some(driver.clone()),
            shutdown: CancellationToken::new(),
        });
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let mut events = kernel.subscribe();
        for (id, text) in [
            ("01K00000000000000000000W01", "first question"),
            ("01K00000000000000000000W02", "second question"),
        ] {
            kernel
                .start_agent_run(
                    StartAgentRunCommand {
                        request_id: id.into(),
                        session_id: "personal".into(),
                        text: text.into(),
                        sort: None,
                    },
                    driver.clone(),
                )
                .unwrap();
            let query = AgentRunQuery {
                request_id: id.into(),
                session_id: "personal".into(),
            };
            tokio::time::timeout(Duration::from_secs(20), async {
                while kernel.inspect_agent_run(query.clone()).unwrap().status
                    == AgentRunStatus::Running
                {
                    let _ = events.recv().await;
                }
            })
            .await
            .unwrap();
        }

        let client = reqwest::Client::new();
        let url = format!("http://{address}/v1/conversation");
        let view: ConversationView = client
            .get(format!("{url}?session_id=personal"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let users = view
            .exchanges
            .iter()
            .map(|exchange| exchange.user.as_str())
            .collect::<Vec<_>>();
        assert_eq!(users, ["first question", "second question"]);
        assert!(
            view.exchanges
                .iter()
                .all(|exchange| exchange.assistant == "HTTP artifact answer")
        );
        assert_eq!(view.exchanges[0].task_id, "run_01K00000000000000000000W01");
        assert_eq!(view.through_seq, kernel.latest_event_seq().unwrap());
        for invalid in [
            "?session_id=%20personal",
            "?session_id=personal&actor=model",
            "",
        ] {
            let response = client.get(format!("{url}{invalid}")).send().await.unwrap();
            assert_eq!(response.status(), 400, "{invalid}");
        }

        let reset = client
            .post(format!("http://{address}/v1/commands/conversation/reset"))
            .json(&serde_json::json!({"session_id": "personal"}))
            .send()
            .await
            .unwrap();
        assert_eq!(reset.status(), 201);
        let view: ConversationView = client
            .get(format!("{url}?session_id=personal"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(view.exchanges.is_empty());
        kernel.shutdown_agent_runs().await.unwrap();
    }
}

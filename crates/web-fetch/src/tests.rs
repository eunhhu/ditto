use std::time::Duration;

use axum::{
    Router,
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
    routing::get,
};
use ditto_capability::{
    CanonicalResource, CapabilityDeriver, InvocationCompiler, LiveExecutionEpoch, UntrustedToolCall,
};
use ditto_model::CancellationToken;
use serde_json::json;

use super::*;

#[test]
fn urls_are_canonical_and_taken_only_from_the_message() {
    assert_eq!(
        canonical_url(" HTTPS://Example.COM:443/a/../b?q=1#part ").unwrap(),
        "https://example.com/b?q=1"
    );
    assert_eq!(
        canonical_url("http://example.com").unwrap(),
        "http://example.com/"
    );
    for invalid in [
        "ftp://example.com/",
        "javascript:alert(1)",
        "https://",
        "example.com",
        "/path",
    ] {
        assert_eq!(canonical_url(invalid), Err(UrlError::Invalid), "{invalid}");
    }
    assert_eq!(
        canonical_url("https://user:pass@example.com/"),
        Err(UrlError::Credentials)
    );
    let long = format!("https://example.com/{}", "a".repeat(MAX_URL_BYTES));
    assert_eq!(canonical_url(&long), Err(UrlError::TooLong));

    let text = "Read https://example.com/a. Also (see https://example.com/wiki/Rust_(lang)), \
                <https://example.org/x> and https://example.com/a again, \
                https://한국.kr/경로에서 HTTP://Upper.example/Z! https://user:pw@example.net/ \
                https://e1.test/ https://e2.test/ https://e3.test/";
    assert_eq!(
        user_urls(text),
        [
            "https://example.com/a",
            "https://example.com/wiki/Rust_(lang)",
            "https://example.org/x",
            "http://upper.example/Z",
            "https://e1.test/",
        ]
    );
    assert!(user_urls("no links here").is_empty());
    assert_eq!(user_urls("https://x.test/한국어"), ["https://x.test/"]);
}

#[test]
fn invocations_normalize_urls_into_exact_network_resources() {
    assert!(validate_manifest(&manifest()));
    schema().validate().unwrap();
    let deriver = FetchDeriver::default();
    let mut epoch = LiveExecutionEpoch::new(1);
    epoch
        .page_in_invocable(&manifest(), &schema(), deriver.revision().clone())
        .unwrap();
    let binding = epoch.invocable_binding(ID).unwrap();
    let compile = |arguments| {
        InvocationCompiler::compile(
            binding,
            UntrustedToolCall::new("call-1", ID, arguments).unwrap(),
            &deriver,
        )
    };
    let invocation = compile(json!({"url": "HTTPS://Example.com/Page#top"})).unwrap();
    assert_eq!(
        invocation.normalized_arguments(),
        &json!({"url": "https://example.com/Page"})
    );
    assert_eq!(
        invocation.resources(),
        &BTreeSet::from([CanonicalResource::url("https://example.com/Page").unwrap()])
    );
    assert_eq!(invocation.effect(), effect());
    for invalid in [
        json!({"url": "file:///etc/passwd"}),
        json!({"url": "https://a:b@example.com/"}),
        json!({"url": "https://example.com/", "method": "POST"}),
        json!({}),
    ] {
        assert!(compile(invalid.clone()).is_err(), "{invalid}");
    }
    assert!(CanonicalResource::url("https://example.com/#frag").is_err());
    assert!(CanonicalResource::url("ftp://example.com/").is_err());
    assert_eq!(
        CanonicalResource::url("https://example.com/")
            .unwrap()
            .to_string(),
        "url:https://example.com/"
    );
}

#[test]
fn text_bounds_cut_at_breaks_and_mark_the_cut() {
    assert_eq!(bounded_chars("short", 10), ("short".into(), false));
    // A break in the last quarter is used; an earlier one is not.
    assert_eq!(
        bounded_chars("one two three four", 14),
        ("one two three…".into(), true)
    );
    assert_eq!(
        bounded_chars("one two three four", 12),
        ("one two thre…".into(), true)
    );
    let (text, cut) = bounded_chars(&"가".repeat(20), 10);
    assert!(cut);
    assert_eq!(text.chars().count(), 11);
}

async fn serve() -> String {
    async fn page() -> impl IntoResponse {
        (
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            "<html><head><title>Test page</title><script>secret()</script></head>\
             <body><h1>Hello</h1><p>Readable &amp; clear.</p></body></html>",
        )
    }
    async fn redirect(headers: HeaderMap) -> impl IntoResponse {
        let host = headers[header::HOST].to_str().unwrap().to_owned();
        (
            StatusCode::FOUND,
            [(header::LOCATION, format!("http://{host}/page"))],
        )
    }
    let app = Router::new()
        .route("/page", get(page))
        .route(
            "/relative",
            get(|| async { (StatusCode::MOVED_PERMANENTLY, [(header::LOCATION, "/page")]) }),
        )
        .route("/absolute", get(redirect))
        .route(
            "/loop",
            get(|| async { (StatusCode::FOUND, [(header::LOCATION, "/loop")]) }),
        )
        .route("/missing", get(|| async { StatusCode::NOT_FOUND }))
        .route(
            "/image",
            get(|| async { ([(header::CONTENT_TYPE, "image/png")], vec![0_u8; 16]) }),
        )
        .route(
            "/json",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "application/json")],
                    r#"{"ok":true}"#,
                )
            }),
        )
        .route(
            "/large",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/plain")],
                    "x".repeat(MAX_BODY_BYTES + 10),
                )
            }),
        )
        .route(
            "/slow",
            get(|| async {
                tokio::time::sleep(Duration::from_secs(5)).await;
                "late"
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}

async fn local(url: &str) -> Result<FetchedPage, FetchError> {
    fetch(
        url,
        FetchPolicy::allow_private_addresses(),
        &CancellationToken::new(),
        Duration::from_secs(10),
    )
    .await
}

#[tokio::test]
async fn pages_are_fetched_as_bounded_readable_text() {
    let base = serve().await;
    let page = local(&format!("{base}/page")).await.unwrap();
    assert_eq!(page.title.as_deref(), Some("Test page"));
    assert_eq!(page.text, "Hello\n\nReadable & clear.");
    assert_eq!(
        (page.status, page.content_type.as_str()),
        (200, "text/html")
    );
    assert!(!page.truncated && page.is_bounded());

    for path in ["/relative", "/absolute"] {
        let followed = local(&format!("{base}{path}")).await.unwrap();
        assert_eq!(followed.url, format!("{base}{path}"));
        assert_eq!(followed.final_url, format!("{base}/page"));
    }
    assert_eq!(
        local(&format!("{base}/loop")).await,
        Err(FetchError::TooManyRedirects)
    );
    assert_eq!(
        local(&format!("{base}/missing")).await,
        Err(FetchError::HttpStatus { status: 404 })
    );
    assert_eq!(
        local(&format!("{base}/image")).await,
        Err(FetchError::UnsupportedContentType)
    );
    assert_eq!(
        local(&format!("{base}/json")).await.unwrap().text,
        r#"{"ok":true}"#
    );
    let large = local(&format!("{base}/large")).await.unwrap();
    assert!(large.truncated && large.is_bounded());
    assert_eq!(large.text.chars().count(), MAX_TEXT_CHARS + 1);

    let slow = fetch(
        &format!("{base}/slow"),
        FetchPolicy::allow_private_addresses(),
        &CancellationToken::new(),
        Duration::from_millis(200),
    )
    .await;
    assert_eq!(slow, Err(FetchError::Timeout));
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert_eq!(
        fetch(
            &format!("{base}/page"),
            FetchPolicy::allow_private_addresses(),
            &cancellation,
            Duration::from_secs(10)
        )
        .await,
        Err(FetchError::Cancelled)
    );
}

#[tokio::test]
async fn the_production_policy_never_connects_to_private_addresses() {
    // A server that fails the test if anything reaches it.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let reached = tokio::spawn(async move { listener.accept().await.is_ok() });
    for url in [
        format!("http://127.0.0.1:{port}/"),
        format!("http://localhost:{port}/"),
        format!("http://[::1]:{port}/"),
        "http://10.0.0.1/".to_owned(),
        "http://169.254.169.254/latest/meta-data/".to_owned(),
        "http://[::ffff:127.0.0.1]/".to_owned(),
    ] {
        let result = fetch(
            &url,
            FetchPolicy::public_only(),
            &CancellationToken::new(),
            Duration::from_secs(5),
        )
        .await;
        assert_eq!(result, Err(FetchError::BlockedAddress), "{url}");
    }
    assert_eq!(
        fetch(
            "https://user:pw@example.com/",
            FetchPolicy::public_only(),
            &CancellationToken::new(),
            Duration::from_secs(5)
        )
        .await,
        Err(FetchError::InvalidUrl)
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!reached.is_finished());
    reached.abort();
}

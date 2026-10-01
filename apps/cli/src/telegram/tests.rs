use std::time::Duration;

use serde_json::json;

use super::{
    Command, admit, bot::Bot, bot::BotToken, bot::split_message, bot::tail, command,
    events::SseParser, load_cursor, matching, request_id, save_cursor,
};

#[test]
fn commands_parse_with_bot_suffixes_and_arguments() {
    assert_eq!(command("/start"), Command::Help);
    assert_eq!(command("/new@DittoBot"), Command::New);
    assert_eq!(command("/NEW"), Command::New);
    assert_eq!(
        command("/remember  I like green tea "),
        Command::Remember("I like green tea")
    );
    assert_eq!(command("/remember"), Command::Remember(""));
    assert_eq!(command("/memories"), Command::Memories);
    assert_eq!(command("/forget  green tea "), Command::Forget("green tea"));
    assert_eq!(command("/forget"), Command::Forget(""));
    assert_eq!(command("/stop"), Command::Stop);
    assert_eq!(command("/unknown thing"), Command::Ask("/unknown thing"));
    assert_eq!(command("hello /new"), Command::Ask("hello /new"));
}

#[test]
fn only_allowed_private_chats_are_admitted() {
    let message = |chat: i64, kind: &str, from: i64| {
        json!({"message_id": 7, "date": 1_700_000_000, "text": "hi",
               "chat": {"id": chat, "type": kind},
               "from": {"id": from, "language_code": "ko"}})
    };
    let admitted = admit(&message(42, "private", 42), &[42]).unwrap();
    assert_eq!((admitted.chat, admitted.message), (42, 7));
    assert!(admitted.korean);
    assert!(admit(&message(43, "private", 43), &[42]).is_none());
    assert!(admit(&message(-100, "group", 42), &[42]).is_none());
    assert!(admit(&message(-100, "supergroup", 42), &[42]).is_none());
    assert!(admit(&message(43, "private", 42), &[42]).is_none());
    assert!(admit(&json!({"chat": {"id": 42, "type": "private"}}), &[42]).is_none());
}

#[test]
fn request_ids_are_deterministic_canonical_ulids() {
    let id = request_id(42, 7, 1_700_000_000);
    assert_eq!(id, request_id(42, 7, 1_700_000_000));
    assert_eq!(id.parse::<ulid::Ulid>().unwrap().to_string(), id);
    assert_ne!(id, request_id(42, 8, 1_700_000_000));
    assert_ne!(id, request_id(43, 7, 1_700_000_000));
    let early = request_id(42, 7, 0);
    assert_eq!(early.parse::<ulid::Ulid>().unwrap().to_string(), early);
}

#[test]
fn messages_split_within_utf16_limits_at_line_breaks() {
    assert!(split_message("   ", 10).is_empty());
    assert_eq!(split_message("short", 10), ["short"]);
    assert_eq!(
        split_message("first line\nsecond line", 15),
        ["first line", "second line"]
    );
    // Emoji take two UTF-16 units; Korean syllables take one.
    let text = "😀".repeat(5) + &"한".repeat(7);
    let parts = split_message(&text, 6);
    assert!(parts.iter().all(|part| part.encode_utf16().count() <= 6));
    assert_eq!(parts.concat(), text);
    assert_eq!(split_message("😀", 1), ["😀"]);
    let long = "word ".repeat(2_000);
    let parts = split_message(&long, 4_000);
    assert!(parts.len() >= 3);
    assert!(parts.iter().all(|part| !part.ends_with(' ')));
}

#[test]
fn draft_tail_keeps_the_end_within_the_limit() {
    assert_eq!(tail("short", 10), "short");
    let tailed = tail("abcdefghij", 5);
    assert_eq!(tailed, "…ghij");
    assert!(tailed.encode_utf16().count() <= 5);
}

#[test]
fn sse_parser_keeps_relevant_payloads_and_skips_others() {
    let big = "x".repeat(3 << 20);
    let stream = format!(
        ": keep-alive\n\n\
         id: 5\nevent: model.requested\ndata: {{\"payload\":\"{big}\"}}\n\n\
         id: 6\r\nevent: turn.finished\r\ndata: {{\"payload\":{{\"turn_id\":\"t1\"}}}}\r\n\r\n\
         id: 7\nevent: model.output\ndata: {{\"payload\":\"{big}\"}}\n\n\
         id: 8\nevent: input.received\ndata: not json\n\n"
    );
    // Feed in uneven chunks to cross every boundary.
    let mut parser = SseParser::default();
    let mut events = Vec::new();
    for chunk in stream.as_bytes().chunks(4_093) {
        events.extend(parser.push(chunk));
    }
    let summary = events
        .iter()
        .map(|event| (event.seq, event.kind.as_str(), event.data.is_some()))
        .collect::<Vec<_>>();
    assert_eq!(
        summary,
        [
            (5, "model.requested", false),
            (6, "turn.finished", true),
            (7, "model.output", false),
            (8, "input.received", false),
        ]
    );
    assert_eq!(events[1].data.as_ref().unwrap()["payload"]["turn_id"], "t1");
}

#[test]
fn bot_errors_and_debug_output_never_contain_the_token() {
    let secret = "SECRETabc123_-xyz";
    let token = format!("123456:{secret}");
    assert!(BotToken::new("not a token".into()).is_err());
    assert!(BotToken::new("12:short".into()).is_err());
    assert_eq!(
        format!("{:?}", BotToken::new(token.clone()).unwrap()),
        "<redacted>"
    );
    assert!(
        Bot::new(
            "http://api.example.com",
            BotToken::new(token.clone()).unwrap()
        )
        .is_err()
    );
    assert!(
        Bot::new(
            "https://api.example.com/?q=1",
            BotToken::new(token.clone()).unwrap()
        )
        .is_err()
    );

    // A server that echoes the token in its error description.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let echoed = token.clone();
    let server = std::thread::spawn(move || {
        use std::io::{Read, Write};
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 4096];
        let _ = stream.read(&mut request).unwrap();
        let body = json!({"ok": false, "error_code": 400,
                          "description": format!("Bad Request: {echoed} / {}", &echoed[7..])})
        .to_string();
        let response = format!(
            "HTTP/1.1 400 Bad Request\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).unwrap();
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let bot = Bot::new(
        &format!("http://{address}"),
        BotToken::new(token.clone()).unwrap(),
    )
    .unwrap();
    let error = runtime
        .block_on(bot.call("getMe", &json!({}), Duration::from_secs(10)))
        .unwrap_err();
    server.join().unwrap();
    assert_eq!(error.status, 400);
    assert!(error.message.contains("<redacted>"));
    assert!(!error.to_string().contains(secret));

    // No response at all: the transport error carries no URL.
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let closed_address = closed.local_addr().unwrap();
    drop(closed);
    let bot = Bot::new(
        &format!("http://{closed_address}"),
        BotToken::new(token).unwrap(),
    )
    .unwrap();
    let error = runtime
        .block_on(bot.call("getMe", &json!({}), Duration::from_secs(10)))
        .unwrap_err();
    assert_eq!(error.status, 0);
    assert!(!error.to_string().contains(secret) && !error.to_string().contains("/bot"));
}

#[test]
fn cursor_state_round_trips_and_rejects_corruption() {
    let root = std::env::temp_dir().join(format!("ditto-telegram-{}", ulid::Ulid::new()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("state.json");
    assert_eq!(load_cursor(&path).unwrap(), None);
    save_cursor(&path, 41).unwrap();
    save_cursor(&path, 42).unwrap();
    assert_eq!(load_cursor(&path).unwrap(), Some(42));
    assert!(!path.with_extension("tmp").exists());
    std::fs::write(&path, b"{broken").unwrap();
    assert!(load_cursor(&path).is_err());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn forget_names_only_memories_that_hold_the_words() {
    let memory = |id: &str, text: &str| ditto_protocol::UserMemory {
        id: id.into(),
        text: text.into(),
        input_event_id: id.into(),
        replaces: None,
        inferred: false,
    };
    let memories = [
        memory("a", "I like green tea"),
        memory("b", "I live in Seoul"),
    ];
    let ids = |words: &str| {
        matching(&memories, words)
            .iter()
            .map(|memory| memory.id.as_str())
            .collect::<Vec<_>>()
    };
    assert_eq!(ids("GREEN Tea"), ["a"]);
    assert_eq!(ids("I "), ["a", "b"]);
    assert!(ids("coffee").is_empty());
}

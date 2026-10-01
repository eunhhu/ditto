//! Telegram gateway (`ditto telegram`). A long-polling Bot API client relays
//! allowed private chats to the daemon's HTTP API, streams answers as message
//! drafts, and delivers the results of scheduled runs. The bot token never
//! leaves this process; the daemon sees only message text (ADR 0025).
mod bot;
mod events;
#[cfg(test)]
mod tests;

use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use ditto_protocol::{
    AgentRunQuery, AgentRunResponse, AgentRunStatus, ForgetMemoryCommand, HealthResponse,
    MemoryPage, RememberInputCommand, ResetConversationCommand, StartAgentRunCommand,
    SubmitInputCommand, SubmitInputResponse, UserMemory,
};
use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio::sync::{Mutex, mpsc, oneshot};

use super::presentation::safe;
use bot::{Bot, BotToken, split_message, tail};
use events::{SseParser, StreamEvent};

const TELEGRAM_API: &str = "https://api.telegram.org";
const POLL_SECONDS: u64 = 50;
/// Below Telegram's limit of 4,096 UTF-16 units per message.
const MESSAGE_UNITS: usize = 4_000;
const DRAFT_INTERVAL: Duration = Duration::from_millis(900);
const QUEUE: usize = 32;
const RECENT_INPUTS: usize = 64;
/// Longer than the daemon's five-minute turn ceiling.
const ANSWER_WAIT: Duration = Duration::from_secs(6 * 60);
/// How long a message waits for another client's run to finish.
const BUSY_RETRIES: u32 = 90;

#[derive(Debug, clap::Args)]
pub(super) struct TelegramArgs {
    /// Telegram user ID allowed to use the bot (repeat or comma-separate).
    #[arg(
        long = "allow-user",
        env = "DITTO_TELEGRAM_ALLOWED_USERS",
        value_delimiter = ',',
        required = true
    )]
    allow_users: Vec<i64>,
    /// Ditto session these chats share with the CLI and web app.
    #[arg(long, default_value = "personal")]
    session: String,
    /// Remembers the last handled event so scheduled results survive restarts.
    #[arg(long, default_value = ".ditto-telegram.json")]
    state_file: PathBuf,
    /// Bot API root; plain HTTP only for loopback (tests, local Bot API servers).
    #[arg(long, hide = true, default_value = TELEGRAM_API)]
    telegram_api: String,
}

pub(super) async fn run(
    client: &reqwest::Client,
    api: &str,
    args: TelegramArgs,
) -> anyhow::Result<()> {
    let token = std::env::var("DITTO_TELEGRAM_BOT_TOKEN")
        .context("set DITTO_TELEGRAM_BOT_TOKEN to the token from @BotFather")?;
    let bot = Bot::new(&args.telegram_api, BotToken::new(token)?)?;
    let me = bot
        .call("getMe", &json!({}), Duration::from_secs(30))
        .await
        .map_err(|error| anyhow::anyhow!("Telegram rejected the bot: {error}"))?;
    let health: HealthResponse = client
        .get(format!("{api}/health"))
        .send()
        .await
        .context("failed to reach Ditto daemon")?
        .error_for_status()?
        .json()
        .await?;
    // Without saved state, start at the present rather than replay history.
    let cursor = load_cursor(&args.state_file)?.unwrap_or(health.latest_seq);
    eprintln!(
        "Telegram gateway for @{} on session {}; allowed users: {:?}. Ctrl+C stops.",
        safe(me["username"].as_str().unwrap_or("?")),
        safe(&args.session),
        args.allow_users
    );
    let gateway = Arc::new(Gateway {
        bot,
        client: client.clone(),
        api: api.to_owned(),
        session: args.session,
        allowed: args.allow_users,
        state_file: args.state_file,
        shared: Mutex::default(),
    });
    let (queue, inbox) = mpsc::channel(QUEUE);
    tokio::select! {
        result = gateway.clone().poll(queue) => result,
        result = gateway.clone().work(inbox) => result,
        result = gateway.clone().follow(cursor) => result,
        signal = tokio::signal::ctrl_c() => signal.context("could not listen for Ctrl+C"),
    }
}

struct Gateway {
    bot: Bot,
    client: reqwest::Client,
    api: String,
    session: String,
    allowed: Vec<i64>,
    state_file: PathBuf,
    shared: Mutex<Shared>,
}

#[derive(Default)]
struct Shared {
    /// Telegram runs that still owe a reply, by request ID.
    replies: HashMap<String, Reply>,
    /// Turn ID to request ID for those runs.
    turns: HashMap<String, String>,
    /// Recent run inputs (request ID, text), to title scheduled results.
    inputs: VecDeque<(String, String)>,
}

struct Reply {
    chat: i64,
    /// The user's message: draft ID and reply target.
    message: i64,
    korean: bool,
    text: String,
    request_index: Option<i64>,
    last_draft: Option<Instant>,
    done: Option<oneshot::Sender<()>>,
}

struct Incoming {
    chat: i64,
    message: i64,
    date: i64,
    text: Option<String>,
    korean: bool,
}

#[derive(Debug, PartialEq)]
enum Command<'a> {
    Help,
    New,
    Remember(&'a str),
    Memories,
    Forget(&'a str),
    Stop,
    Ask(&'a str),
}

fn command(text: &str) -> Command<'_> {
    let Some(rest) = text.strip_prefix('/') else {
        return Command::Ask(text);
    };
    let (word, argument) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    // Group-style commands address a bot: `/new@DittoBot`.
    let word = word.split('@').next().unwrap_or(word).to_ascii_lowercase();
    match word.as_str() {
        "start" | "help" => Command::Help,
        "new" => Command::New,
        "remember" => Command::Remember(argument.trim()),
        "memories" => Command::Memories,
        "forget" => Command::Forget(argument.trim()),
        "stop" => Command::Stop,
        _ => Command::Ask(text),
    }
}

/// Only private chats with an allowed user; everything else is ignored
/// without a reply.
fn admit(message: &Value, allowed: &[i64]) -> Option<Incoming> {
    let chat = message["chat"]["id"].as_i64()?;
    let from = message["from"]["id"].as_i64()?;
    if message["chat"]["type"] != "private" || chat != from || !allowed.contains(&from) {
        return None;
    }
    Some(Incoming {
        chat,
        message: message["message_id"].as_i64()?,
        date: message["date"].as_i64().unwrap_or(0),
        text: message["text"].as_str().map(str::to_owned),
        korean: message["from"]["language_code"]
            .as_str()
            .is_some_and(|code| code.starts_with("ko")),
    })
}

/// A canonical ULID derived from the Telegram message, so a redelivered
/// update retries the same run instead of starting another.
fn request_id(chat: i64, message: i64, date: i64) -> String {
    const MASK_48: u64 = (1 << 48) - 1;
    let millis = u64::try_from(date).unwrap_or(0).saturating_mul(1_000) & MASK_48;
    let random = (u128::from(chat as u64 & MASK_48) << 32) | u128::from(message as u32);
    ulid::Ulid::from_parts(millis, random).to_string()
}

impl Gateway {
    /// Long-poll updates. Messages queue in order for one worker; stop
    /// requests act at once on the answer in progress.
    async fn poll(self: Arc<Self>, queue: mpsc::Sender<Incoming>) -> anyhow::Result<()> {
        let mut offset: Option<i64> = None;
        let mut backoff = Duration::from_secs(1);
        loop {
            let mut body = json!({
                "timeout": POLL_SECONDS,
                "allowed_updates": ["message", "stopped_message_generation"],
            });
            if let Some(offset) = offset {
                body["offset"] = json!(offset);
            }
            let wait = Duration::from_secs(POLL_SECONDS + 15);
            let updates = match self.bot.call("getUpdates", &body, wait).await {
                Ok(updates) => {
                    backoff = Duration::from_secs(1);
                    updates
                }
                Err(error) if matches!(error.status, 401 | 404) => {
                    bail!("Telegram rejected the bot token")
                }
                Err(error) if error.status == 409 => {
                    bail!("another client or a webhook is receiving this bot's updates")
                }
                Err(error) => {
                    eprintln!("Telegram polling failed: {}", safe(&error.to_string()));
                    let delay = error
                        .retry_after
                        .map_or(backoff, |seconds| Duration::from_secs(seconds.min(60)));
                    tokio::time::sleep(delay).await;
                    backoff = (backoff * 2).min(Duration::from_secs(30));
                    continue;
                }
            };
            for update in updates.as_array().into_iter().flatten() {
                if let Some(id) = update["update_id"].as_i64() {
                    offset = Some(id + 1);
                }
                if let Some(stopped) = update.get("stopped_message_generation") {
                    let chat = stopped["chat"]["id"].as_i64();
                    if chat.is_some_and(|chat| self.allowed.contains(&chat)) {
                        self.stop(chat, stopped["draft_id"].as_i64()).await;
                    }
                    continue;
                }
                let Some(incoming) = admit(&update["message"], &self.allowed) else {
                    continue;
                };
                if incoming.text.as_deref().map(str::trim).map(command) == Some(Command::Stop) {
                    self.stop(Some(incoming.chat), None).await;
                } else if let Err(mpsc::error::TrySendError::Full(incoming)) =
                    queue.try_send(incoming)
                {
                    let text = Say(incoming.korean).queue_full();
                    self.send(incoming.chat, text, Some(incoming.message)).await;
                }
            }
        }
    }

    async fn work(self: Arc<Self>, mut inbox: mpsc::Receiver<Incoming>) -> anyhow::Result<()> {
        while let Some(incoming) = inbox.recv().await {
            if let Err(error) = self.handle(&incoming).await {
                eprintln!("Telegram message failed: {}", safe(&format!("{error:#}")));
                let text = Say(incoming.korean).error();
                self.send(incoming.chat, text, Some(incoming.message)).await;
            }
        }
        Ok(())
    }

    async fn handle(&self, incoming: &Incoming) -> anyhow::Result<()> {
        let say = Say(incoming.korean);
        let reply = match incoming.text.as_deref().map(str::trim) {
            None => say.text_only().to_owned(),
            Some(text) => match command(text) {
                Command::Help | Command::Stop => say.help().to_owned(),
                Command::New => {
                    let reset = ResetConversationCommand {
                        session_id: self.session.clone(),
                    };
                    self.post(
                        "/v1/commands/conversation/reset",
                        serde_json::to_value(reset)?,
                    )
                    .await?;
                    say.new_thread().to_owned()
                }
                Command::Remember("") => say.remember_usage().to_owned(),
                Command::Remember(fact) => {
                    let input = SubmitInputCommand {
                        text: fact.to_owned(),
                        session_id: Some(self.session.clone()),
                        task_id: None,
                    };
                    let input: SubmitInputResponse = self
                        .post("/v1/commands/input", serde_json::to_value(input)?)
                        .await?
                        .json()
                        .await?;
                    let memory = RememberInputCommand {
                        session_id: self.session.clone(),
                        input_event_id: input.event.event_id,
                        replaces: None,
                    };
                    self.post("/v1/commands/memory", serde_json::to_value(memory)?)
                        .await?;
                    say.saved().to_owned()
                }
                Command::Memories => {
                    let list = self
                        .memories(false)
                        .await?
                        .iter()
                        .map(|memory| {
                            let by = if memory.inferred { say.by_ditto() } else { "" };
                            format!("• {}{by}", memory.text)
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    if list.is_empty() {
                        say.no_memories().to_owned()
                    } else {
                        list
                    }
                }
                Command::Forget("") => say.forget_usage().to_owned(),
                Command::Forget(words) => self.forget(words, say).await?,
                Command::Ask(question) => return self.ask(incoming, question).await,
            },
        };
        self.send(incoming.chat, &reply, Some(incoming.message))
            .await;
        Ok(())
    }

    async fn ask(&self, incoming: &Incoming, text: &str) -> anyhow::Result<()> {
        let say = Say(incoming.korean);
        let request_id = request_id(incoming.chat, incoming.message, incoming.date);
        let (done, finished) = oneshot::channel();
        self.shared.lock().await.replies.insert(
            request_id.clone(),
            Reply {
                chat: incoming.chat,
                message: incoming.message,
                korean: incoming.korean,
                text: String::new(),
                request_index: None,
                last_draft: None,
                done: Some(done),
            },
        );
        // An empty draft shows "Thinking…" with a stop button.
        self.draft(incoming.chat, incoming.message, "").await;
        let command = StartAgentRunCommand {
            request_id: request_id.clone(),
            session_id: self.session.clone(),
            text: text.to_owned(),
            sort: None,
        };
        let mut attempts = 0;
        let accepted = loop {
            let response = self
                .client
                .post(format!("{}/v1/commands/run", self.api))
                .timeout(Duration::from_secs(30))
                .json(&command)
                .send()
                .await;
            let refusal = match response {
                // Another client's run holds the single run slot; wait for it.
                Ok(response) if response.status() == 429 && attempts < BUSY_RETRIES => {
                    attempts += 1;
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    continue;
                }
                Ok(response) if response.status().is_success() => {
                    break response.json::<AgentRunResponse>().await?;
                }
                Ok(response) if response.status() == 503 => say.disabled().to_owned(),
                Ok(response) if response.status() == 429 => say.busy().to_owned(),
                Ok(response) => format!("{} ({})", say.error(), response.status()),
                Err(error) => {
                    self.shared.lock().await.replies.remove(&request_id);
                    return Err(error.into());
                }
            };
            self.shared.lock().await.replies.remove(&request_id);
            self.send(incoming.chat, &refusal, Some(incoming.message))
                .await;
            return Ok(());
        };
        if accepted.status == AgentRunStatus::Running {
            {
                // The stream may already have mapped, or even answered, it.
                let mut shared = self.shared.lock().await;
                if shared.replies.contains_key(&request_id) {
                    shared
                        .turns
                        .insert(accepted.turn_id.clone(), request_id.clone());
                }
            }
            if tokio::time::timeout(ANSWER_WAIT, finished).await.is_ok() {
                return Ok(());
            }
        }
        // Already terminal (a redelivered update) or the stream missed it.
        let status = self.status(&request_id).await?;
        let reply = self.shared.lock().await.replies.remove(&request_id);
        self.finish(reply, &status).await;
        Ok(())
    }

    /// Cancel the answer in progress in `chat` (a specific draft if given).
    async fn stop(&self, chat: Option<i64>, draft: Option<i64>) {
        let Some(chat) = chat else { return };
        let target = self
            .shared
            .lock()
            .await
            .replies
            .iter()
            .find(|(_, reply)| {
                reply.chat == chat && draft.is_none_or(|draft| draft == reply.message)
            })
            .map(|(request, _)| request.clone());
        if let Some(request_id) = target {
            let query = AgentRunQuery {
                request_id,
                session_id: self.session.clone(),
            };
            let cancel = serde_json::to_value(query).unwrap_or_default();
            if let Err(error) = self.post("/v1/commands/run/cancel", cancel).await {
                eprintln!("Telegram stop failed: {}", safe(&format!("{error:#}")));
            }
        }
    }

    async fn follow(self: Arc<Self>, mut cursor: i64) -> anyhow::Result<()> {
        loop {
            if let Err(error) = self.follow_once(&mut cursor).await {
                eprintln!("Ditto event stream: {}", safe(&format!("{error:#}")));
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    async fn follow_once(&self, cursor: &mut i64) -> anyhow::Result<()> {
        let response = self
            .client
            .get(format!("{}/v1/stream", self.api))
            .query(&[
                ("session_id", self.session.clone()),
                ("after_seq", cursor.to_string()),
            ])
            .send()
            .await?
            .error_for_status()?;
        let mut parser = SseParser::default();
        let mut body = response.bytes_stream();
        while let Some(chunk) = body.next().await {
            for event in parser.push(&chunk?) {
                let terminal = self.on_event(&event).await;
                *cursor = event.seq;
                if terminal {
                    save_cursor(&self.state_file, event.seq)?;
                }
            }
        }
        Ok(())
    }

    /// Returns true for a run terminal, after which the cursor is saved.
    async fn on_event(&self, event: &StreamEvent) -> bool {
        let Some(data) = &event.data else {
            return false;
        };
        let payload = &data["payload"];
        match event.kind.as_str() {
            "input.received" => {
                let (Some(request), Some(turn)) = (
                    payload["agent_run"]["request_id"].as_str(),
                    data["correlation_id"].as_str(),
                ) else {
                    return false;
                };
                let mut shared = self.shared.lock().await;
                if shared.replies.contains_key(request) {
                    shared.turns.insert(turn.to_owned(), request.to_owned());
                }
                if let Some(text) = payload["text"].as_str() {
                    shared
                        .inputs
                        .push_back((request.to_owned(), text.to_owned()));
                    if shared.inputs.len() > RECENT_INPUTS {
                        shared.inputs.pop_front();
                    }
                }
                false
            }
            "model.output" => {
                self.stream_text(payload).await;
                false
            }
            "turn.finished" | "turn.failed" => {
                self.on_terminal(data).await;
                true
            }
            _ => false,
        }
    }

    async fn stream_text(&self, payload: &Value) {
        let delta = &payload["stream_event"]["event"];
        if delta["type"] != "text_delta" {
            return;
        }
        let preview = {
            let mut shared = self.shared.lock().await;
            let Some(request) = payload["turn_id"]
                .as_str()
                .and_then(|turn| shared.turns.get(turn))
                .cloned()
            else {
                return;
            };
            let Some(reply) = shared.replies.get_mut(&request) else {
                return;
            };
            let index = payload["request_index"].as_i64();
            if reply.request_index != index {
                reply.request_index = index;
                reply.text.clear();
            }
            reply
                .text
                .push_str(delta["text"].as_str().unwrap_or_default());
            if reply
                .last_draft
                .is_some_and(|sent| sent.elapsed() < DRAFT_INTERVAL)
            {
                return;
            }
            reply.last_draft = Some(Instant::now());
            (reply.chat, reply.message, tail(&reply.text, MESSAGE_UNITS))
        };
        self.draft(preview.0, preview.1, &preview.2).await;
    }

    async fn on_terminal(&self, data: &Value) {
        let turn = data["payload"]["turn_id"].as_str().unwrap_or_default();
        let ours = self.shared.lock().await.turns.remove(turn);
        if let Some(request_id) = ours {
            match self.status(&request_id).await {
                Ok(status) => {
                    let reply = self.shared.lock().await.replies.remove(&request_id);
                    self.finish(reply, &status).await;
                }
                Err(error) => eprintln!("Ditto run status: {}", safe(&format!("{error:#}"))),
            }
            return;
        }
        // Another client's run: deliver it only when a schedule started it.
        let Some(request_id) = data["task_id"]
            .as_str()
            .and_then(|task| task.strip_prefix("run_"))
        else {
            return;
        };
        let status = match self.status(request_id).await {
            Ok(status) if status.schedule_request_id.is_some() => status,
            Ok(_) => return,
            Err(error) => {
                eprintln!("Ditto run status: {}", safe(&format!("{error:#}")));
                return;
            }
        };
        let prompt = self
            .shared
            .lock()
            .await
            .inputs
            .iter()
            .find(|(request, _)| request == request_id)
            .map(|(_, text)| text.clone());
        let result = match (&status.status, &status.response) {
            (AgentRunStatus::Unverified, Some(response)) => response.clone(),
            _ => format!(
                "⚠️ {}",
                status.failure_code.as_deref().unwrap_or("interrupted")
            ),
        };
        let text = match prompt {
            Some(prompt) => format!("⏰ {prompt}\n\n{result}"),
            None => format!("⏰ {result}"),
        };
        // In a private chat the chat ID is the user ID.
        for &user in &self.allowed {
            self.send(user, &text, None).await;
        }
    }

    async fn finish(&self, reply: Option<Reply>, status: &AgentRunResponse) {
        let Some(mut reply) = reply else { return };
        let say = Say(reply.korean);
        let text = match (&status.status, status.failure_code.as_deref()) {
            (AgentRunStatus::Unverified, _) => status.response.clone().unwrap_or_default(),
            (AgentRunStatus::Failed, Some("cancelled")) => say.stopped().to_owned(),
            (AgentRunStatus::Failed, code) => say.failed(code.unwrap_or("unknown")),
            (AgentRunStatus::Interrupted, _) => say.interrupted().to_owned(),
            (AgentRunStatus::Running, _) => return,
        };
        let text = if text.trim().is_empty() {
            say.empty().to_owned()
        } else {
            text
        };
        self.send(reply.chat, &text, Some(reply.message)).await;
        if let Some(done) = reply.done.take() {
            let _ = done.send(());
        }
    }

    async fn status(&self, request_id: &str) -> anyhow::Result<AgentRunResponse> {
        Ok(self
            .client
            .get(format!("{}/v1/runs", self.api))
            .query(&[
                ("session_id", self.session.as_str()),
                ("request_id", request_id),
            ])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    /// The session's memories: the first page, or every page.
    async fn memories(&self, every: bool) -> anyhow::Result<Vec<UserMemory>> {
        let mut memories = Vec::new();
        let mut after: Option<String> = None;
        loop {
            let mut query = vec![
                ("session_id", self.session.clone()),
                ("limit", "100".into()),
            ];
            if let Some(after) = &after {
                query.push(("after_id", after.clone()));
            }
            let page: MemoryPage = self
                .client
                .get(format!("{}/v1/memories", self.api))
                .query(&query)
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            memories.extend(page.memories);
            match page.next_after_id {
                Some(next) if every => after = Some(next),
                _ => return Ok(memories),
            }
        }
    }

    /// `/forget <words>` (ADR 0032): forget the one memory whose text holds
    /// the words. A number could name another memory once Ditto writes, so
    /// several matches are listed instead.
    async fn forget(&self, words: &str, say: Say) -> anyhow::Result<String> {
        let memories = self.memories(true).await?;
        let found = matching(&memories, words);
        let [memory] = found.as_slice() else {
            return Ok(if found.is_empty() {
                say.no_matching_memory().to_owned()
            } else {
                let list = found
                    .iter()
                    .map(|memory| format!("• {}", memory.text))
                    .collect::<Vec<_>>()
                    .join("\n");
                format!("{}\n{list}", say.several_memories_match())
            });
        };
        let forget = ForgetMemoryCommand {
            session_id: self.session.clone(),
            memory_id: memory.id.clone(),
        };
        let response = self
            .client
            .post(format!("{}/v1/commands/memory/forget", self.api))
            .timeout(Duration::from_secs(30))
            .json(&forget)
            .send()
            .await?;
        // Forgotten meanwhile, by Ditto or another client.
        if response.status() == reqwest::StatusCode::CONFLICT {
            return Ok(say.no_matching_memory().to_owned());
        }
        response.error_for_status()?;
        Ok(format!("{}{}", say.forgot(), memory.text))
    }

    async fn post(&self, path: &str, body: Value) -> anyhow::Result<reqwest::Response> {
        let response = self
            .client
            .post(format!("{}{path}", self.api))
            .timeout(Duration::from_secs(30))
            .json(&body)
            .send()
            .await?;
        if !response.status().is_success() {
            let status = response.status();
            let detail = response.text().await.unwrap_or_default();
            bail!(
                "{path} returned {status}: {}",
                detail.chars().take(300).collect::<String>()
            );
        }
        Ok(response)
    }

    /// Plain text only: model output never becomes Telegram markup.
    async fn send(&self, chat: i64, text: &str, reply_to: Option<i64>) {
        for (index, part) in split_message(text, MESSAGE_UNITS).into_iter().enumerate() {
            let mut body = json!({"chat_id": chat, "text": part});
            if let (0, Some(message)) = (index, reply_to) {
                body["reply_parameters"] =
                    json!({"message_id": message, "allow_sending_without_reply": true});
            }
            for attempt in 0..3 {
                match self
                    .bot
                    .call("sendMessage", &body, Duration::from_secs(30))
                    .await
                {
                    Ok(_) => break,
                    Err(error) => {
                        eprintln!("Telegram send failed: {}", safe(&error.to_string()));
                        match error.retry_after {
                            Some(seconds) if attempt < 2 => {
                                tokio::time::sleep(Duration::from_secs(seconds.min(60))).await;
                            }
                            _ => return,
                        }
                    }
                }
            }
        }
    }

    /// Best effort: a failed preview never affects the final answer.
    async fn draft(&self, chat: i64, message: i64, text: &str) {
        let body = json!({
            "chat_id": chat,
            "draft_id": message,
            "text": text,
            "can_stop": true,
        });
        if self
            .bot
            .call("sendMessageDraft", &body, Duration::from_secs(10))
            .await
            .is_err()
        {
            let typing = json!({"chat_id": chat, "action": "typing"});
            let _ = self
                .bot
                .call("sendChatAction", &typing, Duration::from_secs(10))
                .await;
        }
    }
}

fn load_cursor(path: &Path) -> anyhow::Result<Option<i64>> {
    match std::fs::read(path) {
        Ok(bytes) => {
            let state: Value = serde_json::from_slice(&bytes)
                .with_context(|| format!("{} is not gateway state", path.display()))?;
            Ok(state["after_seq"].as_i64())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("could not read {}", path.display())),
    }
}

/// Atomic replace, so a crash leaves the old or the new cursor.
fn save_cursor(path: &Path, seq: i64) -> anyhow::Result<()> {
    let temporary = path.with_extension("tmp");
    std::fs::write(
        &temporary,
        serde_json::to_vec(&json!({"version": 1, "after_seq": seq}))?,
    )
    .with_context(|| format!("could not write {}", temporary.display()))?;
    std::fs::rename(&temporary, path)
        .with_context(|| format!("could not replace {}", path.display()))?;
    Ok(())
}

/// The memories whose text holds `words`, ignoring case.
fn matching<'a>(memories: &'a [UserMemory], words: &str) -> Vec<&'a UserMemory> {
    let words = words.to_lowercase();
    memories
        .iter()
        .filter(|memory| memory.text.to_lowercase().contains(&words))
        .collect()
}

/// Fixed replies in the user's Telegram language (Korean or English).
#[derive(Clone, Copy)]
struct Say(bool);

impl Say {
    fn pick(self, english: &'static str, korean: &'static str) -> &'static str {
        if self.0 { korean } else { english }
    }
    fn help(self) -> &'static str {
        self.pick(
            "Hi, I'm Ditto. Send a message to ask.\n/new starts a new conversation\n/remember <fact> saves a memory\n/memories lists memories\n/forget <words> forgets the memory that holds them\n/stop cancels the current answer",
            "안녕하세요, Ditto입니다. 메시지를 보내 질문하세요.\n/new 새 대화 시작\n/remember <내용> 기억 저장\n/memories 기억 목록\n/forget <단어> 그 단어가 든 기억 지우기\n/stop 현재 답변 중단",
        )
    }
    fn new_thread(self) -> &'static str {
        self.pick("New conversation started.", "새 대화를 시작했습니다.")
    }
    fn saved(self) -> &'static str {
        self.pick("Saved to memory.", "기억에 저장했습니다.")
    }
    fn remember_usage(self) -> &'static str {
        self.pick("Usage: /remember <fact>", "사용법: /remember <내용>")
    }
    fn no_memories(self) -> &'static str {
        self.pick("No memories yet.", "아직 기억이 없습니다.")
    }
    fn forget_usage(self) -> &'static str {
        self.pick(
            "Usage: /forget <words from the memory>",
            "사용법: /forget <기억에 있는 단어>",
        )
    }
    fn no_matching_memory(self) -> &'static str {
        self.pick(
            "No memory holds those words.",
            "그 단어가 든 기억이 없습니다.",
        )
    }
    fn several_memories_match(self) -> &'static str {
        self.pick(
            "Several memories hold those words; send more of one:",
            "그 단어가 든 기억이 여럿입니다. 하나를 더 길게 보내 주세요:",
        )
    }
    fn forgot(self) -> &'static str {
        self.pick("Forgot: ", "잊었습니다: ")
    }
    /// Marks a memory Ditto inferred from the conversation (ADR 0031).
    fn by_ditto(self) -> &'static str {
        self.pick(" (inferred by Ditto)", " (Ditto가 추론)")
    }
    fn text_only(self) -> &'static str {
        self.pick(
            "Only text messages are supported.",
            "텍스트 메시지만 지원합니다.",
        )
    }
    fn stopped(self) -> &'static str {
        self.pick("Stopped.", "중단했습니다.")
    }
    fn interrupted(self) -> &'static str {
        self.pick("The answer was interrupted.", "답변이 중단되었습니다.")
    }
    fn empty(self) -> &'static str {
        self.pick("(empty answer)", "(빈 답변)")
    }
    fn disabled(self) -> &'static str {
        self.pick(
            "No model is configured on the Ditto daemon.",
            "Ditto 데몬에 모델이 설정되지 않았습니다.",
        )
    }
    fn busy(self) -> &'static str {
        self.pick(
            "Ditto is busy with another request; try again soon.",
            "다른 요청을 처리 중입니다. 잠시 후 다시 시도하세요.",
        )
    }
    fn queue_full(self) -> &'static str {
        self.pick(
            "Too many messages are waiting; try again soon.",
            "대기 중인 메시지가 너무 많습니다. 잠시 후 다시 시도하세요.",
        )
    }
    fn error(self) -> &'static str {
        self.pick("Something went wrong.", "문제가 발생했습니다.")
    }
    fn failed(self, code: &str) -> String {
        format!("{} ({code})", self.pick("No answer", "답변 없음"))
    }
}

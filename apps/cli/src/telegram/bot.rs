//! Minimal Telegram Bot API client. The token appears only in request paths;
//! every error is stripped of its URL and scrubbed of the token.
use std::{fmt, time::Duration};

use anyhow::bail;
use reqwest::Url;
use serde_json::Value;

pub(super) struct BotToken(String);

impl BotToken {
    pub(super) fn new(value: String) -> anyhow::Result<Self> {
        let valid = value.len() <= 128
            && value.split_once(':').is_some_and(|(id, secret)| {
                !id.is_empty()
                    && id.bytes().all(|byte| byte.is_ascii_digit())
                    && secret.len() >= 8
                    && secret
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            });
        if !valid {
            bail!("DITTO_TELEGRAM_BOT_TOKEN is not a Telegram bot token");
        }
        Ok(Self(value))
    }
}

impl fmt::Debug for BotToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<redacted>")
    }
}

#[derive(Debug)]
pub(super) struct BotError {
    /// HTTP status, or 0 when no response arrived.
    pub(super) status: u16,
    pub(super) retry_after: Option<u64>,
    pub(super) message: String,
}

impl fmt::Display for BotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.status == 0 {
            formatter.write_str(&self.message)
        } else {
            write!(formatter, "HTTP {}: {}", self.status, self.message)
        }
    }
}

pub(super) struct Bot {
    client: reqwest::Client,
    root: String,
    token: BotToken,
}

impl Bot {
    /// `root` is the Bot API origin; HTTPS unless it is a loopback server.
    pub(super) fn new(root: &str, token: BotToken) -> anyhow::Result<Self> {
        let url = Url::parse(root.trim())
            .map_err(|_| anyhow::anyhow!("Telegram API root is not a URL"))?;
        let loopback = url.host_str().is_some_and(|host| {
            let host = host.trim_start_matches('[').trim_end_matches(']');
            host.eq_ignore_ascii_case("localhost")
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|address| address.is_loopback())
        });
        if !(url.scheme() == "https" || (url.scheme() == "http" && loopback))
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            bail!("Telegram API root must be HTTPS (plain HTTP only for loopback)");
        }
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .https_only(!loopback)
            .build()?;
        Ok(Self {
            client,
            root: url.as_str().trim_end_matches('/').to_owned(),
            token,
        })
    }

    pub(super) async fn call(
        &self,
        method: &str,
        body: &Value,
        timeout: Duration,
    ) -> Result<Value, BotError> {
        let response = self
            .client
            .post(format!("{}/bot{}/{method}", self.root, self.token.0))
            .timeout(timeout)
            .json(body)
            .send()
            .await
            .map_err(|error| self.transport(error))?;
        let status = response.status().as_u16();
        let envelope: Value = response
            .json()
            .await
            .map_err(|error| self.transport(error))?;
        if envelope["ok"] == Value::Bool(true) {
            return Ok(envelope["result"].clone());
        }
        Err(BotError {
            status,
            retry_after: envelope["parameters"]["retry_after"].as_u64(),
            message: self.scrub(
                envelope["description"]
                    .as_str()
                    .unwrap_or("Telegram request failed"),
            ),
        })
    }

    fn transport(&self, error: reqwest::Error) -> BotError {
        BotError {
            status: 0,
            retry_after: None,
            message: self.scrub(&error.without_url().to_string()),
        }
    }

    pub(super) fn scrub(&self, text: &str) -> String {
        let mut text = text.replace(&self.token.0, "<redacted>");
        if let Some((_, secret)) = self.token.0.split_once(':') {
            text = text.replace(secret, "<redacted>");
        }
        text.chars().take(512).collect()
    }
}

/// Split text into messages of at most `limit` UTF-16 units (Telegram's
/// measure), preferring line breaks and then spaces.
pub(super) fn split_message(text: &str, limit: usize) -> Vec<String> {
    let mut parts = Vec::new();
    let mut rest = text.trim();
    while !rest.is_empty() {
        let mut units = 0;
        let mut cut = rest.len();
        for (index, character) in rest.char_indices() {
            if units + character.len_utf16() > limit {
                cut = index;
                break;
            }
            units += character.len_utf16();
        }
        if cut == 0 {
            // A single character wider than the limit still makes progress.
            cut = rest.chars().next().map_or(rest.len(), char::len_utf8);
        } else if cut < rest.len() {
            let window = &rest[..cut];
            cut = window
                .rfind('\n')
                .or_else(|| window.rfind(' '))
                .filter(|&index| index >= cut / 2)
                .map_or(cut, |index| index + 1);
        }
        let part = rest[..cut].trim();
        if !part.is_empty() {
            parts.push(part.to_owned());
        }
        rest = rest[cut..].trim_start();
    }
    parts
}

/// The end of a long text, for a draft preview of at most `limit` units.
pub(super) fn tail(text: &str, limit: usize) -> String {
    if text.encode_utf16().count() <= limit {
        return text.to_owned();
    }
    let mut units = 1;
    let mut start = text.len();
    for (index, character) in text.char_indices().rev() {
        if units + character.len_utf16() > limit {
            break;
        }
        units += character.len_utf16();
        start = index;
    }
    format!("…{}", &text[start..])
}

//! Incremental parser for the daemon's SSE stream. Only the payloads the
//! gateway reads are buffered, each up to a bound; other events keep just
//! their sequence and kind, so large model requests are never retained.
use serde_json::Value;

const RELEVANT: [&str; 4] = [
    "input.received",
    "model.output",
    "turn.finished",
    "turn.failed",
];
const MAX_DATA_BYTES: usize = 1 << 20;
const MAX_FIELD_BYTES: usize = 256;

#[derive(Debug, PartialEq)]
pub(super) struct StreamEvent {
    pub(super) seq: i64,
    pub(super) kind: String,
    /// The event record, for relevant kinds within the size bound.
    pub(super) data: Option<Value>,
}

#[derive(Default)]
pub(super) struct SseParser {
    line: Vec<u8>,
    /// The current line is a data line of an event whose data is not kept.
    skipping: bool,
    overflow: bool,
    id: Option<i64>,
    kind: Option<String>,
    data: Vec<u8>,
    keep: bool,
}

impl SseParser {
    pub(super) fn push(&mut self, chunk: &[u8]) -> Vec<StreamEvent> {
        let mut events = Vec::new();
        for &byte in chunk {
            if byte == b'\n' {
                if !self.skipping {
                    if self.line.last() == Some(&b'\r') {
                        self.line.pop();
                    }
                    let line = std::mem::take(&mut self.line);
                    if line.is_empty() {
                        events.extend(self.dispatch());
                    } else {
                        self.field(&line);
                    }
                }
                self.line.clear();
                self.skipping = false;
                continue;
            }
            if self.skipping {
                continue;
            }
            self.line.push(byte);
            if self.line.len() == 5 && self.line == b"data:" && !self.keep {
                self.skipping = true;
                self.line.clear();
            } else if self.line.len() > MAX_DATA_BYTES + MAX_FIELD_BYTES {
                self.overflow = true;
                self.skipping = true;
                self.line.clear();
            }
        }
        events
    }

    fn field(&mut self, line: &[u8]) {
        let (name, value) = match line.iter().position(|&byte| byte == b':') {
            Some(0) => return, // comment or keep-alive
            Some(index) => {
                let value = &line[index + 1..];
                (&line[..index], value.strip_prefix(b" ").unwrap_or(value))
            }
            None => (line, &b""[..]),
        };
        match name {
            b"id" => {
                self.id = std::str::from_utf8(value)
                    .ok()
                    .and_then(|id| id.parse().ok())
            }
            b"event" if value.len() <= MAX_FIELD_BYTES => {
                let kind = String::from_utf8_lossy(value).into_owned();
                self.keep = RELEVANT.contains(&kind.as_str());
                self.kind = Some(kind);
            }
            b"data" if self.keep => {
                if !self.data.is_empty() {
                    self.data.push(b'\n');
                }
                self.data.extend_from_slice(value);
                self.overflow |= self.data.len() > MAX_DATA_BYTES;
            }
            _ => {}
        }
    }

    fn dispatch(&mut self) -> Option<StreamEvent> {
        let id = self.id.take();
        let kind = self.kind.take();
        let data = std::mem::take(&mut self.data);
        let keep = std::mem::take(&mut self.keep);
        let overflow = std::mem::take(&mut self.overflow);
        Some(StreamEvent {
            seq: id?,
            data: (keep && !overflow)
                .then(|| serde_json::from_slice(&data).ok())
                .flatten(),
            kind: kind?,
        })
    }
}

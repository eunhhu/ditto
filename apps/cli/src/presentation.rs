//! Presentation only: status, authority and verification still come from the daemon.
use ditto_protocol::{AgentRunResponse, RepeatScheduleResponse, ScheduleResponse, SortRunResponse};
use serde_json::Value;

#[derive(Clone, Copy)]
pub(super) struct View<'a> {
    pub api: &'a str,
    pub human: bool,
}

#[derive(Clone, Copy)]
pub(super) enum Kind {
    Run,
    Sort,
    Schedule,
    Repeat,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Run => "run",
            Self::Sort => "sort",
            Self::Schedule => "schedule",
            Self::Repeat => "repeat",
        }
    }
}

/// Escape controls (including CR, LF, C1 and bidi formatting) in every untrusted
/// field. One field stays on one line; model text cannot forge a status heading.
pub(super) fn safe(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_control()
            || matches!(c, '\u{061c}' | '\u{200e}'..='\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        {
            output.extend(c.escape_default());
        } else {
            output.push(c);
        }
    }
    output
}

fn quote(text: &str) -> String {
    if safe(text) == text {
        return format!("'{}'", text.replace('\'', "'\"'\"'"));
    }
    // Keep terminal-sensitive Unicode out of the displayed command without
    // changing a valid opaque identity. POSIX printf reconstructs the exact
    // UTF-8 bytes; command substitution strips only trailing LF, which valid
    // Ditto identifiers reject as a control/surrounding-whitespace character.
    let octal = text
        .as_bytes()
        .iter()
        .map(|byte| format!("\\{byte:03o}"))
        .collect::<String>();
    format!("\"$(printf '%b' '{octal}')\"")
}

impl View<'_> {
    fn recovery(self, kind: Kind, id: &str, session: &str) -> String {
        // This also runs for default-JSON submissions. Never echo raw userinfo,
        // including when parsing fails; recovery must not disclose credentials.
        let (api, note) = match reqwest::Url::parse(self.api) {
            Ok(mut api) => {
                let userinfo = !api.username().is_empty() || api.password().is_some();
                let _ = api.set_password(None);
                let _ = api.set_username("");
                (
                    api.to_string(),
                    if userinfo {
                        "\nRecovery API: userinfo removed; supply authentication separately."
                    } else {
                        ""
                    },
                )
            }
            Err(_) => (
                "<invalid API URL>".to_owned(),
                "\nRecovery API unavailable: invalid URL.",
            ),
        };
        format!(
            "Inspect: ditto --api {} --human {}-status {} --session={}{note}",
            quote(&api),
            kind.name(),
            quote(id),
            quote(session)
        )
    }

    pub fn submitting(self, kind: Kind, id: &str, session: &str) {
        eprintln!(
            "{} request: {} (session: {})",
            kind.name(),
            safe(id),
            safe(session)
        );
        eprintln!("{}", self.recovery(kind, id, session));
        eprintln!(
            "If submission is uncertain, inspect first; retry only the identical request with this ID."
        );
    }

    pub fn print(self, kind: Kind, value: &Value) -> anyhow::Result<()> {
        if !self.human {
            return super::print_json(value);
        }
        let mut out = String::new();
        if let Some(items) = value.as_array() {
            if items.is_empty() {
                out.push_str("No pending entries.\n");
            }
            for item in items {
                self.render(kind, item, &mut out)?;
            }
        } else {
            self.render(kind, value, &mut out)?;
        }
        print!("{out}");
        Ok(())
    }

    fn render(self, kind: Kind, value: &Value, out: &mut String) -> anyhow::Result<()> {
        // Typed decoding prevents malformed/unknown wire states from silently
        // becoming plausible human summaries. Default JSON remains untouched.
        let normalized = match kind {
            Kind::Run => {
                serde_json::to_value(serde_json::from_value::<AgentRunResponse>(value.clone())?)?
            }
            Kind::Sort => {
                serde_json::to_value(serde_json::from_value::<SortRunResponse>(value.clone())?)?
            }
            Kind::Schedule => {
                serde_json::to_value(serde_json::from_value::<ScheduleResponse>(value.clone())?)?
            }
            Kind::Repeat => serde_json::to_value(
                serde_json::from_value::<RepeatScheduleResponse>(value.clone())?,
            )?,
        };
        let value = &normalized;
        let field = |key: &str| value[key].as_str().unwrap_or("unavailable");
        line(out, "Request", field("request_id"));
        line(out, "Session", field("session_id"));
        out.push_str(&self.recovery(kind, field("request_id"), field("session_id")));
        out.push('\n');
        match kind {
            Kind::Run => {
                line(out, "Model", field("status"));
                cancellation(out, value);
                optional(out, value, "failure_code", "Model failure");
                optional(out, value, "response", "Model answer (unverified)");
                if let Some(sort) = value.get("sort") {
                    let permission = if sort["allow_deduplicate"] == true {
                        "allowed"
                    } else {
                        "not allowed"
                    };
                    line(
                        out,
                        "Sort permission",
                        &format!(
                            "one sort of exact artifact {}; deduplication {permission}; expires with this run; no durable grant",
                            sort["input_reference"].as_str().unwrap_or("unavailable")
                        ),
                    );
                    sort_result(out, sort, "state");
                } else {
                    line(out, "Sort permission", "none attached");
                }
            }
            Kind::Sort => {
                sort_result(out, value, "status");
                cancellation(out, value);
                line(
                    out,
                    "Permission",
                    "one explicit exact-file sort; no durable grant. Input/mode are not included in this status response.",
                );
            }
            Kind::Schedule | Kind::Repeat => {
                let repeat = matches!(kind, Kind::Repeat);
                line(
                    out,
                    if repeat { "Repeat" } else { "Schedule" },
                    field("status"),
                );
                line(out, "First due", field("due_at"));
                line(out, "Latest start (exclusive)", field("expires_at"));
                if let Some(reason) = value["waiting_for"].as_str() {
                    line(
                        out,
                        "Waiting for",
                        match reason {
                            "provider_disabled" => {
                                "provider disabled; execution requires an explicitly enabled daemon provider"
                            }
                            "scheduler_stopped" => "scheduler stopped",
                            "due_time" => "due time",
                            "runtime_busy" => "shared execution slot (busy)",
                            "dispatch" => "dispatch",
                            _ => reason,
                        },
                    );
                }
                if repeat {
                    line(
                        out,
                        "Occurrences",
                        &format!(
                            "total: {}; claimed: {}; missed: {}; interval: {} seconds",
                            value["occurrences"],
                            value["claimed_occurrences"],
                            value["missed_occurrences"],
                            value["every_seconds"]
                        ),
                    );
                    if field("status") == "exhausted" {
                        line(
                            out,
                            "Timetable",
                            "exhausted means entries consumed; not a success claim. Inspect child outcomes.",
                        );
                    }
                    if field("status") == "cancelled" {
                        line(
                            out,
                            "Cancellation",
                            "future occurrences stopped; inspect the child for its independent terminal state",
                        );
                    }
                    optional(out, value, "next_due_at", "Next due");
                    optional(out, value, "last_occurrence_id", "Latest child");
                    if let Some(child) = value.get("last_occurrence") {
                        self.render(Kind::Schedule, child, out)?;
                    } else if let Some(id) = value["last_occurrence_id"].as_str() {
                        out.push_str(&self.recovery(Kind::Schedule, id, field("session_id")));
                        out.push('\n');
                    }
                } else {
                    schedule_cancellation(out, value);
                    if let Some(run) = value.get("run") {
                        self.render(Kind::Run, run, out)?;
                    }
                }
            }
        }
        if field("status") == "interrupted" {
            line(
                out,
                "Recovery",
                "original attempt will not restart automatically; inspect evidence before submitting new work",
            );
        }
        Ok(())
    }
}

fn line(out: &mut String, label: &str, value: &str) {
    out.push_str(&format!("{label}: {}\n", safe(value)));
}

fn optional(out: &mut String, value: &Value, key: &str, label: &str) {
    if let Some(text) = value[key].as_str() {
        line(out, label, text);
    }
}

fn cancellation(out: &mut String, value: &Value) {
    if terminal_cancellation(value) {
        line(out, "Cancellation", "terminal");
    } else if value["cancellation_requested"] == true {
        line(
            out,
            "Cancellation",
            "requested; terminal cancellation not confirmed",
        );
    }
}

fn schedule_cancellation(out: &mut String, value: &Value) {
    if value["cancellation_requested"] == true
        && value.get("run").is_some_and(terminal_cancellation)
    {
        line(
            out,
            "Schedule cancellation",
            "terminal; confirmed by child run",
        );
    } else {
        cancellation(out, value);
    }
}

fn terminal_cancellation(value: &Value) -> bool {
    value["status"] == "cancelled"
        || (value["status"] == "failed" && value["failure_code"] == "cancelled")
}

fn sort_result(out: &mut String, value: &Value, state: &str) {
    line(out, "Sort", value[state].as_str().unwrap_or("unavailable"));
    if value[state] == "verified" {
        line(
            out,
            "Verification",
            "exact line ordering and multiplicity contract only",
        );
    }
    optional(out, value, "failure_code", "Sort failure");
    optional(out, value, "output_reference", "Sort artifact");
    optional(out, value, "output", "Sort output");
}

#[cfg(test)]
mod tests {
    use super::quote;
    use std::process::Command;

    #[test]
    fn shell_quote_is_terminal_safe_and_preserves_valid_separator_identity() {
        let identity = "alpha\u{2028}beta\u{2029}gamma\u{202e}";
        let quoted = quote(identity);
        assert!(quoted.is_ascii());
        let output = Command::new("/bin/sh")
            .args(["-c", &format!("set -- {quoted}; printf '%s' \"$1\"")])
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output.stderr);
        assert_eq!(output.stdout, identity.as_bytes());
    }
}

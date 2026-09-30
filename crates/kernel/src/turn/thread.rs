//! The conversation thread before a turn (ADR 0022). Each session's thread is
//! kept between turns and advanced by the conversation events committed since,
//! usually one `turn.finished`, instead of being reread (ADR 0028 Phase B).

use std::{
    collections::{HashMap, VecDeque},
    sync::PoisonError,
};

use ditto_protocol::{EventRecord, event_kind};

use super::shared::{
    HistoryExchange, MAX_HISTORY_CANDIDATES, ThreadExchange, agent_run_text, bounded_history_text,
    select_history_stepped,
};
use super::types::TurnFinishedPayload;
use crate::{DittoKernel, KernelError};

/// Sessions whose thread is kept; past this the map is cleared.
const MAX_KEPT_THREADS: usize = 64;
/// Conversation events applied to a kept thread; more reload it instead.
const MAX_THREAD_ADVANCE: usize = 64;

#[derive(Default)]
pub(crate) struct ThreadReuse {
    sessions: HashMap<String, KeptThread>,
}

/// A session's thread before `before_seq`: its length in finished `run_*`
/// turns and its newest finished turns, newest first, each the bounded
/// exchange of an agent run or `None` for another turn. It holds exactly what
/// the indexed reads return, so replay's recomputation matches it.
struct KeptThread {
    before_seq: i64,
    len: usize,
    newest_first: VecDeque<Option<HistoryExchange>>,
}

impl DittoKernel {
    /// The version-6 history window of the thread before `before_seq`.
    pub(super) fn conversation_history(
        &self,
        session: &str,
        before_seq: i64,
    ) -> Result<Vec<HistoryExchange>, KernelError> {
        // A cache: a panic mid-update at worst drops one session's entry.
        let mut reuse = self
            .inner
            .thread_reuse
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let kept = match reuse.sessions.remove(session) {
            Some(kept) => self.advance_thread(session, kept, before_seq)?,
            None => None,
        };
        let thread = match kept {
            Some(thread) => thread,
            None => self.load_thread(session, before_seq)?,
        };
        let history =
            select_history_stepped(thread.len, thread.newest_first.iter().flatten().cloned());
        if reuse.sessions.len() >= MAX_KEPT_THREADS {
            reuse.sessions.clear();
        }
        reuse.sessions.insert(session.to_owned(), thread);
        Ok(history)
    }

    /// Finished agent-run exchanges of the session's current thread before
    /// `before_seq`, newest first and unabridged. At most `candidates`
    /// finished turns are examined; other turns in the thread are skipped.
    pub(crate) fn thread_exchanges(
        &self,
        session: &str,
        before_seq: i64,
        candidates: usize,
    ) -> Result<Vec<ThreadExchange>, KernelError> {
        let finished = self
            .inner
            .events
            .conversation_finished_turns(session, before_seq, candidates)?;
        let mut exchanges = Vec::with_capacity(finished.len());
        for event in finished {
            exchanges.extend(self.finished_exchange(session, event)?);
        }
        Ok(exchanges)
    }

    /// Bounded indexed reads of the thread, as before reuse.
    fn load_thread(&self, session: &str, before_seq: i64) -> Result<KeptThread, KernelError> {
        let len = self
            .inner
            .events
            .conversation_finished_count(session, before_seq)?;
        let newest_first = self
            .inner
            .events
            .conversation_finished_turns(session, before_seq, MAX_HISTORY_CANDIDATES)?
            .into_iter()
            .map(|finished| Ok(self.finished_exchange(session, finished)?.map(bounded)))
            .collect::<Result<_, KernelError>>()?;
        Ok(KeptThread {
            before_seq,
            len,
            newest_first,
        })
    }

    /// Apply the conversation events committed since the kept thread, or
    /// return `None` when the thread must be reloaded instead.
    fn advance_thread(
        &self,
        session: &str,
        mut thread: KeptThread,
        before_seq: i64,
    ) -> Result<Option<KeptThread>, KernelError> {
        if before_seq < thread.before_seq {
            return Ok(None);
        }
        let events = self.inner.events.conversation_events_between(
            session,
            thread.before_seq - 1,
            before_seq,
            MAX_THREAD_ADVANCE + 1,
        )?;
        if events.len() > MAX_THREAD_ADVANCE {
            return Ok(None);
        }
        for event in events {
            if event.kind == event_kind::CONVERSATION_RESET {
                thread.len = 0;
                thread.newest_first.clear();
                continue;
            }
            if event
                .task_id
                .as_deref()
                .is_some_and(|task| task.starts_with("run_"))
            {
                thread.len += 1;
            }
            let exchange = self.finished_exchange(session, event)?.map(bounded);
            thread.newest_first.push_front(exchange);
            thread.newest_first.truncate(MAX_HISTORY_CANDIDATES);
        }
        thread.before_seq = before_seq;
        Ok(Some(thread))
    }

    /// The exchange of one `turn.finished` event, if its input was an agent
    /// run's.
    fn finished_exchange(
        &self,
        session: &str,
        finished: EventRecord,
    ) -> Result<Option<ThreadExchange>, KernelError> {
        let payload: TurnFinishedPayload = serde_json::from_value(finished.payload)?;
        let task = finished
            .task_id
            .ok_or_else(|| KernelError::InvalidCommand("finished turn has no task".into()))?;
        let input = self
            .inner
            .events
            .turn_input(session, &task, &payload.turn_id)?
            .ok_or_else(|| KernelError::InvalidCommand("finished turn has no input".into()))?;
        Ok(agent_run_text(&input).map(|user| ThreadExchange {
            task_id: task,
            finished_seq: finished.seq,
            exchange: HistoryExchange {
                turn_id: payload.turn_id,
                user: user.to_owned(),
                assistant: payload.outcome.response,
            },
        }))
    }
}

/// Kept exchanges are stored bounded; bounding is idempotent, so the window
/// rule sees the same text as from a fresh read.
fn bounded(thread: ThreadExchange) -> HistoryExchange {
    HistoryExchange {
        user: bounded_history_text(&thread.exchange.user),
        assistant: bounded_history_text(&thread.exchange.assistant),
        ..thread.exchange
    }
}

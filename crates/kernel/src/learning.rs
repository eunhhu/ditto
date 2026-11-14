//! Retrieval keys learned from the model's own searches (ADR 0050).
//!
//! The first learning loop. When a compilation leaves a memory out and the
//! model's search then finds it, the user's words for that memory become a
//! key, and a later compilation those words match includes the memory without
//! a search. The detector makes no model call and reads only journaled events.
//! Every decision is an event the ledger shows, and the user can undo any key.

use std::collections::{BTreeSet, HashMap, HashSet};

use chrono::{DateTime, Duration, Utc};
use ditto_context::{
    CompiledContext, ContextCandidate, ContextDirective, ContextExclusionReason, ContextNode,
    ContextOrigin, EpistemicStatus, PREFETCH_ATTACHMENT_REASON, search_form_set, search_vocabulary,
};
use ditto_protocol::{
    EventActor, EventQuery, EventRecord, LearnedKey, LearnedState, LearningLedger, LearningQuery,
    NewEvent, RetireLearnedCommand, event_kind,
};
use serde::{Deserialize, Serialize};

use crate::{DittoKernel, KernelError};

/// The only learned kind so far.
pub(crate) const KIND: &str = "retrieval_key";
/// Words one key keeps.
pub(crate) const MAX_KEY_WORDS: usize = 6;
/// Memories learned keys may bring into one compilation.
pub(crate) const MAX_KEYS_PER_TURN: usize = 2;
/// A word is rare enough to learn when at most this many active memories
/// contain it, the same bound the pointer closure uses.
const RARE_WITHIN: usize = 2;
const MAX_EVIDENCE: usize = 8;
const MAX_EPISODE_WORDS: usize = 32;
const PROMOTION_WINDOW_DAYS: i64 = 30;
const KEY_LIFETIME_DAYS: i64 = 90;
const RENEW_AFTER_DAYS: i64 = 30;
const BREADTH_WINDOW: usize = 50;
const BREADTH_LIMIT: usize = 2;
const MAX_LEARNING_EVENTS: usize = 10_000;
const MAX_TURN_PAGES: usize = 16;
/// The receipt reason of a memory a learned key brought in, after
/// `policy-required: `, followed by the promotion event's id.
pub(crate) const RECEIPT_REASON: &str = "learned key ";
const POLICY_PREFIX: &str = "policy-required: ";
const MEMORY_TOOL: &str = "memory.manage";
const SKILL_TOOL: &str = "skill.read";

/// One episode a key was learned from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Evidence {
    pub turn_id: String,
    pub input_event_id: String,
    /// The request's search words, which tell one episode from another.
    pub request_words: Vec<String>,
}

/// The payload of every `improvement.*` event of a retrieval key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct KeyPayload {
    pub event_version: u16,
    pub kind: String,
    pub target: String,
    #[serde(default)]
    pub words: Vec<String>,
    #[serde(default)]
    pub evidence: Vec<Evidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    /// Why a promotion was refused; candidates only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked: Option<String>,
    /// Why it was retired; retirements only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct KeyEvent {
    pub record: EventRecord,
    pub payload: KeyPayload,
}

/// One key's events, oldest first, linked by `supersedes`.
#[derive(Debug, Clone)]
pub(crate) struct Chain {
    pub events: Vec<KeyEvent>,
}

impl Chain {
    fn id(&self) -> &str {
        &self.events[0].record.event_id
    }

    fn latest(&self) -> &KeyEvent {
        self.events.last().expect("a chain has its first event")
    }

    fn promotion_ids(&self) -> impl Iterator<Item = &str> {
        self.events
            .iter()
            .filter(|event| event.record.kind == event_kind::IMPROVEMENT_PROMOTED)
            .map(|event| event.record.event_id.as_str())
    }
}

fn is_learning_kind(kind: &str) -> bool {
    kind == event_kind::IMPROVEMENT_CANDIDATE_CREATED
        || kind == event_kind::IMPROVEMENT_PROMOTED
        || kind == event_kind::IMPROVEMENT_RETIRED
}

/// The session's retrieval-key events among `records`, in sequence order.
fn key_events<'a>(
    records: impl IntoIterator<Item = &'a EventRecord>,
    session: &str,
) -> Vec<KeyEvent> {
    let mut events = records
        .into_iter()
        .filter(|record| {
            is_learning_kind(&record.kind) && record.session_id.as_deref() == Some(session)
        })
        .filter_map(|record| {
            let payload: KeyPayload = serde_json::from_value(record.payload.clone()).ok()?;
            (payload.kind == KIND && payload.event_version == 1).then(|| KeyEvent {
                record: record.clone(),
                payload,
            })
        })
        .collect::<Vec<_>>();
    events.sort_by_key(|event| event.record.seq);
    events
}

/// Link events into chains. An event that supersedes something other than
/// its chain's latest event is dropped: the gate makes that impossible, so
/// such an event is not trusted.
fn chains(events: Vec<KeyEvent>) -> Vec<Chain> {
    let mut chains: Vec<Chain> = Vec::new();
    for event in events {
        match event.payload.supersedes.clone() {
            None => chains.push(Chain {
                events: vec![event],
            }),
            Some(previous) => {
                if let Some(chain) = chains
                    .iter_mut()
                    .find(|chain| chain.latest().record.event_id == previous)
                {
                    chain.events.push(event);
                }
            }
        }
    }
    chains
}

/// How many of a key's words the request shares, with the memory-search
/// folding.
fn shared_words(words: &[String], request: &HashSet<String>) -> usize {
    words
        .iter()
        .filter(|word| {
            search_vocabulary(word)
                .iter()
                .any(|(_, forms)| forms.iter().any(|form| request.contains(form)))
        })
        .count()
}

/// A key matches a request that shares at least two of its words, or its only
/// word.
pub(crate) fn applies(words: &[String], request: &HashSet<String>) -> bool {
    !words.is_empty() && shared_words(words, request) >= words.len().min(2)
}

/// Longest first, then alphabetical, without duplicates.
fn rank(words: &mut Vec<String>) {
    words.sort_by(|left, right| {
        right
            .chars()
            .count()
            .cmp(&left.chars().count())
            .then_with(|| left.cmp(right))
    });
    words.dedup();
}

fn shares(left: &HashSet<String>, right: &HashSet<String>) -> bool {
    left.iter().any(|form| right.contains(form))
}

/// A key in use: the promotion a receipt cites.
#[derive(Debug, Clone)]
pub(crate) struct ActiveKey {
    pub event_id: String,
    pub seq: i64,
    pub target: String,
    pub words: Vec<String>,
}

fn active_keys(chains: &[Chain], at: DateTime<Utc>) -> Vec<ActiveKey> {
    chains
        .iter()
        .filter_map(|chain| {
            let latest = chain.latest();
            (latest.record.kind == event_kind::IMPROVEMENT_PROMOTED
                && latest
                    .payload
                    .expires_at
                    .is_some_and(|expires| expires > at))
            .then(|| ActiveKey {
                event_id: latest.record.event_id.clone(),
                seq: latest.record.seq,
                target: latest.payload.target.clone(),
                words: latest.payload.words.clone(),
            })
        })
        .collect()
}

/// The memories learned keys bring into this compilation, as `(target,
/// receipt reason)`. A key qualifies when it matches the request and its
/// target is a candidate the first compilation left out. The most shared
/// words win, then the oldest key, one key per memory, at most
/// [`MAX_KEYS_PER_TURN`].
pub(crate) fn inclusions(
    keys: &[ActiveKey],
    request: &str,
    compiled: &CompiledContext,
    candidates: &[ContextCandidate],
) -> Vec<(String, String)> {
    if keys.is_empty() {
        return Vec::new();
    }
    let request = search_form_set(request);
    let included = compiled
        .nodes
        .iter()
        .map(|node| node.id.as_str())
        .collect::<HashSet<_>>();
    let ranked = candidates
        .iter()
        .filter(|candidate| candidate.directive == ContextDirective::Ranked)
        .map(|candidate| candidate.node.id.as_str())
        .collect::<HashSet<_>>();
    let mut matched = keys
        .iter()
        .filter(|key| {
            ranked.contains(key.target.as_str())
                && !included.contains(key.target.as_str())
                && applies(&key.words, &request)
        })
        .map(|key| (shared_words(&key.words, &request), key))
        .collect::<Vec<_>>();
    matched.sort_by(|(left_shared, left), (right_shared, right)| {
        right_shared
            .cmp(left_shared)
            .then_with(|| left.seq.cmp(&right.seq))
    });
    let mut targets = HashSet::new();
    matched
        .into_iter()
        .filter(|(_, key)| targets.insert(key.target.clone()))
        .take(MAX_KEYS_PER_TURN)
        .map(|(_, key)| {
            (
                key.target.clone(),
                format!("{RECEIPT_REASON}{}", key.event_id),
            )
        })
        .collect()
}

/// The candidates with each chosen target required under its key's reason.
pub(crate) fn mark(
    candidates: Vec<ContextCandidate>,
    picks: &[(String, String)],
) -> Vec<ContextCandidate> {
    candidates
        .into_iter()
        .map(|candidate| {
            match picks
                .iter()
                .find(|(target, _)| *target == candidate.node.id)
            {
                Some((_, reason)) => ContextCandidate {
                    node: candidate.node,
                    directive: ContextDirective::PolicyRequired {
                        reason: reason.clone(),
                    },
                },
                None => candidate,
            }
        })
        .collect()
}

/// Replay's check of every memory a learned key brought into a turn
/// (ADR 0050). The key must be a promotion of this session, recorded at or
/// before the turn's input, not superseded by then, unexpired at acceptance,
/// aimed at the included node, and matched by the request.
pub(crate) fn validate_cited_keys(
    snapshot: &[EventRecord],
    session: &str,
    compiled: &CompiledContext,
    request: &str,
    input_seq: i64,
    accepted_at: DateTime<Utc>,
) -> Result<(), String> {
    let cited = compiled
        .receipt
        .included
        .iter()
        .filter_map(|receipt| {
            let id = receipt
                .reason
                .strip_prefix(POLICY_PREFIX)?
                .strip_prefix(RECEIPT_REASON)?;
            Some((receipt.node_id.as_str(), id))
        })
        .collect::<Vec<_>>();
    if cited.is_empty() {
        return Ok(());
    }
    if cited.len() > MAX_KEYS_PER_TURN {
        return Err(format!(
            "a turn may cite at most {MAX_KEYS_PER_TURN} learned keys"
        ));
    }
    let request = search_form_set(request);
    let events = key_events(
        snapshot.iter().filter(|record| record.seq <= input_seq),
        session,
    );
    for (node, id) in cited {
        let event = events
            .iter()
            .find(|event| event.record.event_id == id)
            .ok_or_else(|| {
                format!("learned key {id} is not in the session's journal before the turn")
            })?;
        if event.record.kind != event_kind::IMPROVEMENT_PROMOTED {
            return Err(format!("learned key {id} was never promoted"));
        }
        if events
            .iter()
            .any(|later| later.payload.supersedes.as_deref() == Some(id))
        {
            return Err(format!("learned key {id} was superseded before the turn"));
        }
        if event
            .payload
            .expires_at
            .is_none_or(|expires| expires <= accepted_at)
        {
            return Err(format!("learned key {id} had expired"));
        }
        if event.payload.target != node {
            return Err(format!("learned key {id} names another memory"));
        }
        if !applies(&event.payload.words, &request) {
            return Err(format!("learned key {id} does not match the request"));
        }
    }
    Ok(())
}

/// A memory a search in the turn returned.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct Found {
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub inferred: bool,
}

/// What one finished turn says about retrieval, read from its events.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Episode {
    pub turn_id: String,
    pub input_event_id: String,
    pub finished_event_id: String,
    pub request: String,
    pub answer: String,
    /// Memories the compilation left out as irrelevant or over budget.
    pub excluded: BTreeSet<String>,
    pub found: Vec<Found>,
    /// The turn read a page, a file or an attachment.
    pub external: bool,
    /// Promotions the turn's receipts cite.
    pub used: Vec<String>,
}

/// The episode of one turn's events, or `None` when the turn did not finish.
pub(crate) fn episode(events: &[EventRecord]) -> Option<Episode> {
    let input = events
        .iter()
        .find(|event| event.kind == event_kind::INPUT_RECEIVED)?;
    let finished = events
        .iter()
        .find(|event| event.kind == event_kind::TURN_FINISHED)?;
    let mut episode = Episode {
        turn_id: input.correlation_id.clone()?,
        input_event_id: input.event_id.clone(),
        finished_event_id: finished.event_id.clone(),
        request: input.payload.get("text")?.as_str()?.to_owned(),
        answer: finished
            .payload
            .pointer("/outcome/response")?
            .as_str()?
            .to_owned(),
        ..Episode::default()
    };
    for event in events {
        match event.kind.as_str() {
            event_kind::CONTEXT_COMPILED => {
                let compiled: CompiledContext =
                    serde_json::from_value(event.payload.get("compiled")?.clone()).ok()?;
                episode.excluded = compiled
                    .receipt
                    .excluded
                    .iter()
                    .filter(|exclusion| {
                        matches!(
                            exclusion.reason,
                            ContextExclusionReason::Irrelevant
                                | ContextExclusionReason::TokenBudget
                        )
                    })
                    .map(|exclusion| exclusion.node_id.clone())
                    .collect();
                for receipt in &compiled.receipt.included {
                    if receipt.reason == PREFETCH_ATTACHMENT_REASON {
                        episode.external = true;
                    }
                    if let Some(id) = receipt
                        .reason
                        .strip_prefix(POLICY_PREFIX)
                        .and_then(|reason| reason.strip_prefix(RECEIPT_REASON))
                    {
                        episode.used.push(id.to_owned());
                    }
                }
            }
            event_kind::CAPABILITY_REQUESTED => episode.external = true,
            event_kind::TOOL_REQUESTED => {
                let tool = event
                    .payload
                    .get("capability_id")
                    .and_then(|value| value.as_str());
                if tool != Some(MEMORY_TOOL) && tool != Some(SKILL_TOOL) {
                    episode.external = true;
                }
            }
            event_kind::TOOL_OUTPUT
                if event
                    .payload
                    .get("capability_id")
                    .and_then(|value| value.as_str())
                    == Some(MEMORY_TOOL) =>
            {
                let result = event.payload.get("result");
                if result
                    .and_then(|result| result.get("outcome"))
                    .and_then(|value| value.as_str())
                    == Some("found")
                    && let Some(memories) = result.and_then(|result| result.get("memories"))
                {
                    let found: Vec<Found> =
                        serde_json::from_value(memories.clone()).unwrap_or_default();
                    episode.found.extend(found);
                }
            }
            _ => {}
        }
    }
    Some(episode)
}

/// One event the detector would append.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Proposal {
    pub kind: &'static str,
    pub payload: KeyPayload,
}

pub(crate) struct Inputs<'a> {
    pub episode: &'a Episode,
    /// The session's active memories now.
    pub memories: &'a [ContextNode],
    pub chains: &'a [Chain],
    /// Earlier finished agent-run requests of the session, newest first, as
    /// `(input event id, text)`.
    pub recent: &'a [(String, String)],
    pub now: DateTime<Utc>,
}

/// What one finished turn teaches: renewals of the keys it used, and at most
/// one candidate or promotion from a miss its search repaired (ADR 0050).
pub(crate) fn propose(inputs: &Inputs<'_>) -> Vec<Proposal> {
    let mut proposals = renewals(inputs);
    if let Some(proposal) = learn(inputs) {
        proposals.push(proposal);
    }
    proposals
}

fn payload(
    target: &str,
    words: Vec<String>,
    evidence: Vec<Evidence>,
    supersedes: Option<String>,
) -> KeyPayload {
    KeyPayload {
        event_version: 1,
        kind: KIND.to_owned(),
        target: target.to_owned(),
        words,
        evidence,
        supersedes,
        expires_at: None,
        blocked: None,
        reason: None,
    }
}

fn renewals(inputs: &Inputs<'_>) -> Vec<Proposal> {
    let mut used = inputs.episode.used.clone();
    used.sort();
    used.dedup();
    used.iter()
        .filter_map(|id| {
            let chain = inputs
                .chains
                .iter()
                .find(|chain| chain.latest().record.event_id == *id)?;
            let latest = chain.latest();
            if latest.record.kind != event_kind::IMPROVEMENT_PROMOTED
                || latest.record.recorded_at > inputs.now - Duration::days(RENEW_AFTER_DAYS)
            {
                return None;
            }
            let mut renewed = payload(
                &latest.payload.target,
                latest.payload.words.clone(),
                latest.payload.evidence.clone(),
                Some(id.clone()),
            );
            renewed.expires_at = Some(inputs.now + Duration::days(KEY_LIFETIME_DAYS));
            Some(Proposal {
                kind: event_kind::IMPROVEMENT_PROMOTED,
                payload: renewed,
            })
        })
        .collect()
}

fn learn(inputs: &Inputs<'_>) -> Option<Proposal> {
    let episode = inputs.episode;
    if episode.external {
        return None;
    }
    let active = inputs
        .memories
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect::<HashMap<_, _>>();
    let answer = search_form_set(&episode.answer);
    let mut targets = episode
        .found
        .iter()
        .filter(|found| {
            !found.inferred
                && episode.excluded.contains(&found.id)
                && active.contains_key(found.id.as_str())
                && shares(&search_form_set(&found.text), &answer)
        })
        .map(|found| found.id.as_str())
        .collect::<Vec<_>>();
    targets.sort_unstable();
    targets.dedup();
    let [target] = targets[..] else {
        return None;
    };
    let node = active[target];
    if node.origin != ContextOrigin::User || node.epistemic != EpistemicStatus::Asserted {
        return None;
    }
    let target_forms = search_form_set(&node.summary);
    let memory_forms = inputs
        .memories
        .iter()
        .map(|memory| search_form_set(&memory.summary))
        .collect::<Vec<_>>();
    let vocabulary = search_vocabulary(&episode.request);
    let request_words = vocabulary
        .iter()
        .map(|(word, _)| word.clone())
        .take(MAX_EPISODE_WORDS)
        .collect::<Vec<_>>();
    let mut words = vocabulary
        .iter()
        .filter(|(_, forms)| {
            let forms = forms.iter().cloned().collect::<HashSet<_>>();
            !shares(&forms, &target_forms)
                && memory_forms
                    .iter()
                    .filter(|memory| shares(&forms, memory))
                    .count()
                    <= RARE_WITHIN
        })
        .map(|(word, _)| word.clone())
        .collect::<Vec<_>>();
    rank(&mut words);
    words.truncate(MAX_KEY_WORDS);
    if words.is_empty() {
        return None;
    }
    let evidence = Evidence {
        turn_id: episode.turn_id.clone(),
        input_event_id: episode.input_event_id.clone(),
        request_words,
    };
    let for_target = inputs
        .chains
        .iter()
        .filter(|chain| chain.latest().payload.target == target)
        .collect::<Vec<_>>();
    // The user undid a key for this memory: do not learn it back.
    if for_target
        .iter()
        .any(|chain| chain.latest().record.kind == event_kind::IMPROVEMENT_RETIRED)
    {
        return None;
    }
    let Some(chain) = for_target.last() else {
        return Some(Proposal {
            kind: event_kind::IMPROVEMENT_CANDIDATE_CREATED,
            payload: payload(target, words, vec![evidence], None),
        });
    };
    let latest = chain.latest();
    let previous = &latest.payload;
    if previous
        .evidence
        .iter()
        .any(|old| old.turn_id == evidence.turn_id || old.request_words == evidence.request_words)
    {
        return None;
    }
    let kind = latest.record.kind.as_str();
    let lapsed = (kind == event_kind::IMPROVEMENT_PROMOTED
        && previous
            .expires_at
            .is_none_or(|expires| expires <= inputs.now))
        || (kind == event_kind::IMPROVEMENT_CANDIDATE_CREATED
            && latest.record.recorded_at < inputs.now - Duration::days(PROMOTION_WINDOW_DAYS));
    if lapsed {
        return Some(Proposal {
            kind: event_kind::IMPROVEMENT_CANDIDATE_CREATED,
            payload: payload(
                target,
                words,
                vec![evidence],
                Some(latest.record.event_id.clone()),
            ),
        });
    }
    let mut union = previous.words.clone();
    union.extend(words);
    rank(&mut union);
    union.truncate(MAX_KEY_WORDS);
    let mut history = previous.evidence.clone();
    history.push(evidence);
    if history.len() > MAX_EVIDENCE {
        history.drain(..history.len() - MAX_EVIDENCE);
    }
    let broad = breadth(&union, inputs.recent, &history);
    if broad > BREADTH_LIMIT {
        if kind == event_kind::IMPROVEMENT_PROMOTED {
            return None;
        }
        let mut blocked = payload(target, union, history, Some(latest.record.event_id.clone()));
        blocked.blocked = Some(format!(
            "would match {broad} of the last {} requests",
            inputs.recent.len().min(BREADTH_WINDOW)
        ));
        return Some(Proposal {
            kind: event_kind::IMPROVEMENT_CANDIDATE_CREATED,
            payload: blocked,
        });
    }
    let mut promoted = payload(target, union, history, Some(latest.record.event_id.clone()));
    promoted.expires_at = Some(inputs.now + Duration::days(KEY_LIFETIME_DAYS));
    Some(Proposal {
        kind: event_kind::IMPROVEMENT_PROMOTED,
        payload: promoted,
    })
}

/// How many recent requests outside the evidence the words would match.
fn breadth(words: &[String], recent: &[(String, String)], evidence: &[Evidence]) -> usize {
    recent
        .iter()
        .take(BREADTH_WINDOW)
        .filter(|(input, _)| !evidence.iter().any(|item| item.input_event_id == *input))
        .filter(|(_, text)| applies(words, &search_form_set(text)))
        .count()
}

impl DittoKernel {
    pub(crate) fn learning_enabled(&self) -> bool {
        self.inner.learning
    }

    fn session_chains(&self, session: &str, through_seq: i64) -> Result<Vec<Chain>, KernelError> {
        let mut records = Vec::new();
        for kind in [
            event_kind::IMPROVEMENT_CANDIDATE_CREATED,
            event_kind::IMPROVEMENT_PROMOTED,
            event_kind::IMPROVEMENT_RETIRED,
        ] {
            records.extend(self.inner.events.list_kind_through(
                kind,
                0,
                through_seq,
                MAX_LEARNING_EVENTS,
            )?);
        }
        Ok(chains(key_events(records.iter(), session)))
    }

    /// The keys a turn accepted at `at` may use: promoted through the turn's
    /// input and unexpired.
    pub(crate) fn active_learned_keys(
        &self,
        session: &str,
        input_seq: i64,
        at: DateTime<Utc>,
    ) -> Result<Vec<ActiveKey>, KernelError> {
        if !self.learning_enabled() {
            return Ok(Vec::new());
        }
        Ok(active_keys(&self.session_chains(session, input_seq)?, at))
    }

    /// Record what a finished agent turn taught, if anything. The caller
    /// ignores an error: a failure here means nothing was learned this turn,
    /// never a failed turn.
    pub(crate) fn learn_from_turn(
        &self,
        session: &str,
        task: &str,
        turn_id: &str,
    ) -> Result<(), KernelError> {
        if !self.learning_enabled() {
            return Ok(());
        }
        let Some(input) = self.inner.events.turn_input(session, task, turn_id)? else {
            return Ok(());
        };
        let input_seq = input.seq;
        let events = self.turn_events(session, task, input_seq)?;
        let Some(episode) = episode(&events) else {
            return Ok(());
        };
        let repaired = episode
            .found
            .iter()
            .any(|found| episode.excluded.contains(&found.id));
        if episode.used.is_empty() && (episode.external || !repaired) {
            return Ok(());
        }
        let _gate = self
            .inner
            .learning_gate
            .lock()
            .map_err(|_| KernelError::InvalidCommand("learning gate is poisoned".into()))?;
        let high_water = self.inner.events.latest_seq()?;
        let chains = self.session_chains(session, high_water)?;
        let memories = {
            let _context = self
                .inner
                .context_admission_gate
                .lock()
                .map_err(|_| KernelError::ContextAdmissionGatePoisoned)?;
            self.memory_snapshot_locked(session, high_water)?
                .candidates()
                .to_vec()
        };
        let recent = self.recent_requests(session, input_seq)?;
        let proposals = propose(&Inputs {
            episode: &episode,
            memories: &memories,
            chains: &chains,
            recent: &recent,
            now: chrono::Utc::now(),
        });
        for proposal in proposals {
            self.append_and_publish(NewEvent {
                session_id: Some(session.to_owned()),
                task_id: None,
                actor: EventActor::System,
                kind: proposal.kind.to_owned(),
                payload: serde_json::to_value(&proposal.payload)?,
                causation_id: Some(episode.finished_event_id.clone()),
                correlation_id: Some(session.to_owned()),
                span_id: None,
            })?;
        }
        Ok(())
    }

    /// One turn's events from its input on, until its terminal event.
    fn turn_events(
        &self,
        session: &str,
        task: &str,
        input_seq: i64,
    ) -> Result<Vec<EventRecord>, KernelError> {
        let mut events: Vec<EventRecord> = Vec::new();
        let mut after = input_seq - 1;
        for _ in 0..MAX_TURN_PAGES {
            let page = self.inner.events.list(&EventQuery {
                after_seq: Some(after),
                limit: Some(1_000),
                session_id: Some(session.to_owned()),
                task_id: Some(task.to_owned()),
            })?;
            let Some(last) = page.last() else { break };
            after = last.seq;
            let done = page.iter().any(|event| {
                event.kind == event_kind::TURN_FINISHED || event.kind == event_kind::TURN_FAILED
            });
            events.extend(page);
            if done {
                break;
            }
        }
        Ok(events)
    }

    /// The session's latest finished agent-run requests before `before_seq`,
    /// newest first.
    fn recent_requests(
        &self,
        session: &str,
        before_seq: i64,
    ) -> Result<Vec<(String, String)>, KernelError> {
        let finished =
            self.inner
                .events
                .recent_finished_agent_turns(session, before_seq, BREADTH_WINDOW)?;
        let mut requests = Vec::with_capacity(finished.len());
        for event in finished {
            let (Some(task), Some(turn)) =
                (event.task_id.as_deref(), event.correlation_id.as_deref())
            else {
                continue;
            };
            if let Some(input) = self.inner.events.turn_input(session, task, turn)?
                && let Some(text) = input.payload.get("text").and_then(|text| text.as_str())
            {
                requests.push((input.event_id.clone(), text.to_owned()));
            }
        }
        Ok(requests)
    }

    /// Every learned key of one session and where it stands (ADR 0050).
    pub fn learning_ledger(&self, query: LearningQuery) -> Result<LearningLedger, KernelError> {
        crate::memory::validate_session(&query.session_id)?;
        let session = query.session_id.as_str();
        let high_water = self.inner.events.latest_seq()?;
        let chains = self.session_chains(session, high_water)?;
        let memories = {
            let _context = self
                .inner
                .context_admission_gate
                .lock()
                .map_err(|_| KernelError::ContextAdmissionGatePoisoned)?;
            self.memory_snapshot_locked(session, high_water)?
                .candidates()
                .iter()
                .map(|node| (node.id.clone(), node.summary.clone()))
                .collect::<HashMap<_, _>>()
        };
        let uses = self.key_uses(session, high_water)?;
        let now = chrono::Utc::now();
        Ok(LearningLedger {
            session_id: query.session_id.clone(),
            keys: chains
                .iter()
                .map(|chain| view(chain, &memories, &uses, now))
                .collect(),
        })
    }

    /// Undo one learned key: an `improvement.retired` by the user that
    /// supersedes its latest event. Nothing is edited.
    pub fn retire_learned_key(
        &self,
        command: RetireLearnedCommand,
    ) -> Result<LearnedKey, KernelError> {
        crate::memory::validate_session(&command.session_id)?;
        let session = command.session_id.as_str();
        let _gate = self
            .inner
            .learning_gate
            .lock()
            .map_err(|_| KernelError::InvalidCommand("learning gate is poisoned".into()))?;
        let high_water = self.inner.events.latest_seq()?;
        let chains = self.session_chains(session, high_water)?;
        let chain = chains
            .iter()
            .find(|chain| chain.id() == command.key_id)
            .ok_or_else(|| {
                KernelError::InvalidCommand("no such learned key in this session".into())
            })?;
        let latest = chain.latest();
        if latest.record.kind == event_kind::IMPROVEMENT_RETIRED {
            return Err(KernelError::InvalidCommand(
                "the learned key is already retired".into(),
            ));
        }
        let mut retired = payload(
            &latest.payload.target,
            latest.payload.words.clone(),
            latest.payload.evidence.clone(),
            Some(latest.record.event_id.clone()),
        );
        retired.reason = Some("retired by the user".into());
        self.append_and_publish(NewEvent {
            session_id: Some(session.to_owned()),
            task_id: None,
            actor: EventActor::User,
            kind: event_kind::IMPROVEMENT_RETIRED.to_owned(),
            payload: serde_json::to_value(&retired)?,
            causation_id: None,
            correlation_id: Some(session.to_owned()),
            span_id: None,
        })?;
        drop(_gate);
        let ledger = self.learning_ledger(LearningQuery {
            session_id: command.session_id.clone(),
        })?;
        ledger
            .keys
            .into_iter()
            .find(|key| key.key_id == command.key_id)
            .ok_or_else(|| {
                KernelError::InvalidCommand("the retired key is not in the ledger".into())
            })
    }

    /// How many turns each promotion brought its memory into, by promotion id.
    fn key_uses(
        &self,
        session: &str,
        through_seq: i64,
    ) -> Result<HashMap<String, u32>, KernelError> {
        let mut uses = HashMap::new();
        for event in self.inner.events.list_kind_through(
            event_kind::CONTEXT_COMPILED,
            0,
            through_seq,
            MAX_LEARNING_EVENTS,
        )? {
            if event.session_id.as_deref() != Some(session) {
                continue;
            }
            let Some(included) = event.payload.pointer("/compiled/receipt/included") else {
                continue;
            };
            for receipt in included.as_array().into_iter().flatten() {
                if let Some(id) = receipt
                    .get("reason")
                    .and_then(|reason| reason.as_str())
                    .and_then(|reason| reason.strip_prefix(POLICY_PREFIX))
                    .and_then(|reason| reason.strip_prefix(RECEIPT_REASON))
                {
                    *uses.entry(id.to_owned()).or_insert(0) += 1;
                }
            }
        }
        Ok(uses)
    }
}

fn view(
    chain: &Chain,
    memories: &HashMap<String, String>,
    uses: &HashMap<String, u32>,
    now: DateTime<Utc>,
) -> LearnedKey {
    let latest = chain.latest();
    let target_text = memories.get(&latest.payload.target).cloned();
    let state = match latest.record.kind.as_str() {
        event_kind::IMPROVEMENT_RETIRED => LearnedState::Retired,
        _ if target_text.is_none() => LearnedState::Inert,
        event_kind::IMPROVEMENT_PROMOTED
            if latest
                .payload
                .expires_at
                .is_some_and(|expires| expires > now) =>
        {
            LearnedState::Active
        }
        event_kind::IMPROVEMENT_PROMOTED => LearnedState::Expired,
        _ if latest.payload.blocked.is_some() => LearnedState::Blocked,
        _ => LearnedState::Candidate,
    };
    LearnedKey {
        key_id: chain.id().to_owned(),
        event_id: latest.record.event_id.clone(),
        state,
        target: latest.payload.target.clone(),
        target_text,
        words: latest.payload.words.clone(),
        evidence: latest
            .payload
            .evidence
            .iter()
            .map(|item| item.turn_id.clone())
            .collect(),
        expires_at: latest.payload.expires_at,
        blocked: latest.payload.blocked.clone(),
        uses: chain
            .promotion_ids()
            .map(|id| uses.get(id).copied().unwrap_or(0))
            .sum(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ditto_context::{ContextLens, ContextNodeKind, ContextScope};

    fn memory(id: &str, text: &str, origin: ContextOrigin) -> ContextNode {
        ContextNode {
            id: id.into(),
            kind: ContextNodeKind::Claim,
            summary: text.into(),
            origin,
            epistemic: if origin == ContextOrigin::User {
                EpistemicStatus::Asserted
            } else {
                EpistemicStatus::Inferred
            },
            scope: ContextScope::Session,
            lens: ContextLens::Personal,
            confidence: 1.0,
            source_event_ids: vec!["01J00000000000000000000000".into()],
            supersedes: Vec::new(),
            valid_from: None,
            valid_until: None,
        }
    }

    fn found(id: &str, text: &str) -> Found {
        Found {
            id: id.into(),
            text: text.into(),
            inferred: false,
        }
    }

    fn episode(turn: &str, request: &str, answer: &str, found: Vec<Found>) -> Episode {
        Episode {
            turn_id: turn.into(),
            input_event_id: format!("input-{turn}"),
            finished_event_id: format!("finished-{turn}"),
            request: request.into(),
            answer: answer.into(),
            excluded: found.iter().map(|item| item.id.clone()).collect(),
            found,
            external: false,
            used: Vec::new(),
        }
    }

    fn memories() -> Vec<ContextNode> {
        vec![
            memory(
                "birch",
                "Our book club now gathers in Birch Room",
                ContextOrigin::User,
            ),
            memory(
                "dentist",
                "My dentist is Dr. Alder on Tuesdays",
                ContextOrigin::User,
            ),
            memory(
                "moss-1",
                "filler about pebbles and moss",
                ContextOrigin::User,
            ),
            memory(
                "moss-2",
                "another filler about pebbles and moss",
                ContextOrigin::User,
            ),
            memory(
                "moss-3",
                "more filler about pebbles and moss",
                ContextOrigin::User,
            ),
        ]
    }

    fn proposals(
        episode: &Episode,
        chains: &[Chain],
        recent: &[(String, String)],
    ) -> Vec<Proposal> {
        propose(&Inputs {
            episode,
            memories: &memories(),
            chains,
            recent,
            now: Utc::now(),
        })
    }

    fn chain_of(proposal: &Proposal, id: &str, seq: i64) -> Chain {
        Chain {
            events: vec![KeyEvent {
                record: EventRecord {
                    seq,
                    event_id: id.into(),
                    recorded_at: Utc::now(),
                    session_id: Some("personal".into()),
                    task_id: None,
                    actor: EventActor::System,
                    kind: proposal.kind.into(),
                    payload: serde_json::to_value(&proposal.payload).unwrap(),
                    causation_id: None,
                    correlation_id: None,
                    span_id: None,
                },
                payload: proposal.payload.clone(),
            }],
        }
    }

    #[test]
    fn one_repaired_miss_records_a_candidate_with_the_users_rare_words() {
        let first = episode(
            "turn-1",
            "Where should I go for the reading group get-together?",
            "Birch Room.",
            vec![found("birch", "Our book club now gathers in Birch Room")],
        );
        let [proposal] = &proposals(&first, &[], &[])[..] else {
            panic!("one candidate");
        };
        assert_eq!(proposal.kind, event_kind::IMPROVEMENT_CANDIDATE_CREATED);
        assert_eq!(proposal.payload.target, "birch");
        // Longest first, at most six, none the memory already holds.
        assert_eq!(
            proposal.payload.words,
            ["together", "reading", "group", "get", "go"]
        );
    }

    #[test]
    fn no_target_or_two_targets_teach_nothing() {
        // The answer names neither found memory.
        let silent = episode(
            "turn-1",
            "what about the reading group",
            "I could not tell.",
            vec![found("birch", "Our book club now gathers in Birch Room")],
        );
        assert!(proposals(&silent, &[], &[]).is_empty());
        // Both found memories share a word with the answer.
        let ambiguous = episode(
            "turn-1",
            "where and when do I go",
            "Birch Room, and Dr. Alder on Tuesdays.",
            vec![
                found("birch", "Our book club now gathers in Birch Room"),
                found("dentist", "My dentist is Dr. Alder on Tuesdays"),
            ],
        );
        assert!(proposals(&ambiguous, &[], &[]).is_empty());
    }

    #[test]
    fn an_external_read_or_an_inferred_memory_teaches_nothing() {
        let mut external = episode(
            "turn-1",
            "reading group venue",
            "Birch Room.",
            vec![found("birch", "Our book club now gathers in Birch Room")],
        );
        external.external = true;
        assert!(proposals(&external, &[], &[]).is_empty());

        let mut inferred = episode(
            "turn-1",
            "reading group venue",
            "Birch Room.",
            vec![found("birch", "Our book club now gathers in Birch Room")],
        );
        inferred.found[0].inferred = true;
        assert!(proposals(&inferred, &[], &[]).is_empty());
    }

    #[test]
    fn common_words_are_not_learned() {
        // "moss" and "pebbles" sit in three memories each.
        let first = episode(
            "turn-1",
            "reading group by the moss and pebbles",
            "Birch Room.",
            vec![found("birch", "Our book club now gathers in Birch Room")],
        );
        let proposal = &proposals(&first, &[], &[])[0];
        assert!(!proposal.payload.words.contains(&"moss".to_owned()));
        assert!(!proposal.payload.words.contains(&"pebbles".to_owned()));
    }

    #[test]
    fn a_second_distinct_episode_promotes_unless_the_key_is_too_broad() {
        let birch = vec![found("birch", "Our book club now gathers in Birch Room")];
        let first = episode(
            "turn-1",
            "reading group get-together venue",
            "Birch Room.",
            birch.clone(),
        );
        let candidate = proposals(&first, &[], &[]).remove(0);
        let chains = [chain_of(&candidate, "candidate-1", 10)];

        // The same words are the same episode.
        let again = episode(
            "turn-2",
            "reading group get-together venue",
            "Birch Room.",
            birch.clone(),
        );
        assert!(proposals(&again, &chains, &[]).is_empty());

        let second = episode(
            "turn-3",
            "where does the reading group meet",
            "Birch Room.",
            birch.clone(),
        );
        let promoted = proposals(&second, &chains, &[]).remove(0);
        assert_eq!(promoted.kind, event_kind::IMPROVEMENT_PROMOTED);
        assert_eq!(promoted.payload.supersedes.as_deref(), Some("candidate-1"));
        assert_eq!(promoted.payload.evidence.len(), 2);
        assert!(promoted.payload.expires_at.is_some());

        // Three other recent requests would match: the promotion is refused.
        let recent = (0..3)
            .map(|index| {
                (
                    format!("other-{index}"),
                    "reading group snacks list".to_owned(),
                )
            })
            .collect::<Vec<_>>();
        let blocked = proposals(&second, &chains, &recent).remove(0);
        assert_eq!(blocked.kind, event_kind::IMPROVEMENT_CANDIDATE_CREATED);
        assert!(
            blocked
                .payload
                .blocked
                .as_deref()
                .unwrap()
                .contains("3 of the last 3")
        );
    }

    #[test]
    fn a_key_matches_two_of_its_words_with_the_search_folding() {
        let words = vec!["reading".to_owned(), "group".to_owned(), "venue".to_owned()];
        assert!(applies(&words, &search_form_set("Reading groups tonight?")));
        assert!(!applies(&words, &search_form_set("a good reading lamp")));
        let korean = vec!["독서모임".to_owned(), "장소".to_owned()];
        assert!(applies(
            &korean,
            &search_form_set("독서모임은 장소가 어디야")
        ));
    }
}

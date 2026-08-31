use ditto_context_projection::{ContextProjectionError, ValidatedContextNodeDraft};
use ditto_protocol::{EventActor, EventRecord, NewEvent, event_kind};
use thiserror::Error;

use super::{DittoKernel, KernelError};

const MAX_PROJECTION_FAILURE_DETAIL_BYTES: usize = 4_096;
const PROJECTION_FAILURE_DETAIL_OVERFLOW: &str = "projection error detail exceeds 4096 bytes";

pub const COMMITTED_BUT_PROJECTION_UNAVAILABLE: &str = "committed_but_projection_unavailable";

/// Kernel-trusted context admission input. It deliberately has no event-envelope
/// fields and does not implement `Deserialize`.
///
/// ```compile_fail
/// use ditto_kernel::TrustedContextNodeDraft;
///
/// // Trusted drafts expose only the session/task constructors. A caller cannot
/// // construct actor, kind, causation, correlation, or span authority.
/// let _ = TrustedContextNodeDraft {
///     actor: "system",
///     kind: "context.node.recorded",
///     causation_id: None,
///     correlation_id: None,
///     span_id: None,
/// };
/// ```
///
/// ```compile_fail
/// use ditto_kernel::TrustedContextNodeDraft;
///
/// fn require_deserialize<T: serde::de::DeserializeOwned>() {}
/// require_deserialize::<TrustedContextNodeDraft>();
/// ```
pub use ditto_context_projection::ContextNodeDraft as TrustedContextNodeDraft;

/// Redacted, path-free diagnostic for a post-append projection failure.
#[derive(Debug, Error)]
#[error("{detail}")]
pub struct ContextProjectionUnavailable {
    detail: String,
}

impl ContextProjectionUnavailable {
    pub fn detail(&self) -> &str {
        &self.detail
    }

    fn from_projection_error(error: &ContextProjectionError) -> Self {
        // Decide overflow against the original rendered UTF-8 bytes before
        // discarding that potentially sensitive text.
        if error.to_string().len() > MAX_PROJECTION_FAILURE_DETAIL_BYTES {
            return Self {
                detail: PROJECTION_FAILURE_DETAIL_OVERFLOW.to_owned(),
            };
        }
        // Never copy dynamic SQLite, I/O, identifier, or path material into an
        // accepted outcome or its public error source chain.
        let detail = match error {
            ContextProjectionError::Io(_) => {
                "context projection I/O failed after the durable append"
            }
            ContextProjectionError::Sqlite(_) => {
                "context projection SQLite transaction failed after the durable append"
            }
            ContextProjectionError::EventStore(_) => {
                "event-spine access failed during post-append projection catch-up"
            }
            ContextProjectionError::Poisoned => {
                "context projection mutex was poisoned after the durable append"
            }
            _ => "context projection rejected post-append catch-up",
        };
        Self {
            detail: bounded_projection_failure_detail(detail),
        }
    }
}

impl DittoKernel {
    /// Admit one kernel-trusted durable context node.
    ///
    /// The clone-shared gate orders admissions within this `KernelInner`. It is
    /// intentionally not a cross-process or separately-opened-kernel lock.
    pub fn admit_context_node(
        &self,
        draft: TrustedContextNodeDraft,
    ) -> Result<EventRecord, KernelError> {
        let _gate = self
            .inner
            .context_admission_gate
            .lock()
            .map_err(|_| KernelError::ContextAdmissionGatePoisoned)?;

        let high_water = self.inner.events.latest_seq()?;
        self.inner
            .context_projection
            .synchronize_through(&self.inner.events, high_water)?;
        let validated = self
            .inner
            .context_projection
            .validate_draft(&self.inner.events, high_water, &draft)
            .map_err(map_context_admission_error)?;
        let committed = self.append_without_publish(context_node_event(&validated)?)?;
        let projection_result = self
            .inner
            .context_projection
            .synchronize_through_event(&self.inner.events, &committed);

        // Durable append is acceptance. Both post-append branches make one
        // publication attempt; only failures before append are silent.
        self.publish(&committed);
        #[cfg(test)]
        run_after_publication_hook(&committed);
        match projection_result {
            Ok(_) => Ok(committed),
            Err(error) => Err(KernelError::CommittedButProjectionUnavailable {
                event: Box::new(committed),
                source: ContextProjectionUnavailable::from_projection_error(&error),
            }),
        }
    }
}

#[cfg(test)]
type AfterPublicationHook = Box<dyn FnOnce(&EventRecord)>;

#[cfg(test)]
thread_local! {
    static AFTER_PUBLICATION_HOOK: std::cell::RefCell<Option<AfterPublicationHook>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn set_after_publication_hook(hook: impl FnOnce(&EventRecord) + 'static) {
    AFTER_PUBLICATION_HOOK.with(|slot| {
        let previous = slot.replace(Some(Box::new(hook)));
        assert!(previous.is_none(), "publication hook already installed");
    });
}

#[cfg(test)]
fn run_after_publication_hook(event: &EventRecord) {
    AFTER_PUBLICATION_HOOK.with(|slot| {
        if let Some(hook) = slot.take() {
            hook(event);
        }
    });
}

fn context_node_event(validated: &ValidatedContextNodeDraft) -> Result<NewEvent, KernelError> {
    Ok(NewEvent {
        session_id: Some(validated.session_id().to_owned()),
        task_id: validated.task_id().map(str::to_owned),
        actor: EventActor::System,
        kind: event_kind::CONTEXT_NODE_RECORDED.to_owned(),
        payload: serde_json::to_value(validated.payload())?,
        causation_id: Some(validated.causation_id().to_owned()),
        correlation_id: Some(validated.correlation_id().to_owned()),
        span_id: None,
    })
}

fn map_context_admission_error(error: ContextProjectionError) -> KernelError {
    match error {
        ContextProjectionError::DuplicateNodeIdentity {
            session_id,
            node_id,
            event_id,
            seq,
        } => KernelError::DuplicateContextNodeIdentity {
            session_id,
            node_id,
            event_id,
            event_seq: seq,
        },
        error => KernelError::ContextProjection(error),
    }
}

fn bounded_projection_failure_detail(detail: &str) -> String {
    if detail.len() > MAX_PROJECTION_FAILURE_DETAIL_BYTES {
        PROJECTION_FAILURE_DETAIL_OVERFLOW.to_owned()
    } else {
        detail.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind},
        sync::{Arc, mpsc},
        thread,
    };

    use ditto_context::{
        ContextLens, ContextNode, ContextNodeKind, ContextOrigin, ContextScope, EpistemicStatus,
    };
    use ditto_event_store::EventStore;
    use ditto_protocol::{EventActor, NewEvent};
    use serde_json::json;
    use tempfile::tempdir;
    use tokio::sync::broadcast::error::TryRecvError;

    use super::{
        ContextProjectionError, ContextProjectionUnavailable, PROJECTION_FAILURE_DETAIL_OVERFLOW,
        TrustedContextNodeDraft, bounded_projection_failure_detail, set_after_publication_hook,
    };
    use crate::{DittoKernel, KernelConfig, KernelError};

    fn kernel() -> (tempfile::TempDir, DittoKernel) {
        let directory = tempdir().expect("temporary directory");
        let kernel = DittoKernel::open(KernelConfig::new(
            directory.path().join("data"),
            directory.path().join("capabilities"),
        ))
        .expect("open kernel");
        (directory, kernel)
    }

    fn node(source_event_id: String) -> ContextNode {
        ContextNode {
            id: "publication-order".into(),
            kind: ContextNodeKind::Claim,
            summary: "projection precedes publication".into(),
            origin: ContextOrigin::User,
            epistemic: EpistemicStatus::Asserted,
            scope: ContextScope::Session,
            lens: ContextLens::Task,
            confidence: 1.0,
            source_event_ids: vec![source_event_id],
            supersedes: Vec::new(),
            valid_from: None,
            valid_until: None,
        }
    }

    fn projection_error_with_rendered_bytes(target: usize) -> ContextProjectionError {
        let base = ContextProjectionError::InvalidNode {
            node_id: "n".into(),
            reason: String::new(),
        };
        let base_len = base.to_string().len();
        let remaining = target.checked_sub(base_len).expect("target fits prefix");
        let mut reason = "é".repeat(remaining / 2);
        if reason.len() < remaining {
            reason.push('x');
        }
        let error = ContextProjectionError::InvalidNode {
            node_id: "n".into(),
            reason,
        };
        assert_eq!(error.to_string().len(), target);
        error
    }

    #[test]
    fn projection_failure_detail_bound_is_utf8_safe() {
        let exact = "é".repeat(2_048);
        assert_eq!(exact.len(), 4_096);
        assert_eq!(bounded_projection_failure_detail(&exact), exact);
        assert_eq!(
            bounded_projection_failure_detail(&"é".repeat(2_049)),
            PROJECTION_FAILURE_DETAIL_OVERFLOW
        );

        let exact_error = projection_error_with_rendered_bytes(4_096);
        assert_eq!(
            ContextProjectionUnavailable::from_projection_error(&exact_error).detail(),
            "context projection rejected post-append catch-up"
        );
        let over_error = projection_error_with_rendered_bytes(4_097);
        assert_eq!(
            ContextProjectionUnavailable::from_projection_error(&over_error).detail(),
            PROJECTION_FAILURE_DETAIL_OVERFLOW
        );
    }

    #[test]
    fn publication_is_observed_after_exact_projection_and_before_admission_returns() {
        let (directory, kernel) = kernel();
        let store = EventStore::open(directory.path().join("data/state.db"))
            .expect("open source event store");
        let source = store
            .append(NewEvent {
                session_id: Some("session".into()),
                task_id: None,
                actor: EventActor::User,
                kind: "fixture.source".into(),
                payload: json!({"source": true}),
                causation_id: None,
                correlation_id: Some("session".into()),
                span_id: None,
            })
            .expect("append source");
        let mut receiver = kernel.subscribe();
        let (published_tx, published_rx) = mpsc::sync_channel(0);
        let (release_tx, release_rx) = mpsc::sync_channel(0);
        let (result_tx, result_rx) = mpsc::channel();
        let admission_kernel = kernel.clone();
        let handle = thread::spawn(move || {
            set_after_publication_hook(move |_| {
                published_tx.send(()).expect("signal publication boundary");
                release_rx.recv().expect("release admission return");
            });
            let result = admission_kernel.admit_context_node(TrustedContextNodeDraft::session(
                "session",
                node(source.event_id),
            ));
            result_tx.send(result).expect("send admission result");
        });

        published_rx.recv().expect("publication boundary reached");
        let observed = receiver.try_recv().expect("broadcast before return");
        assert!(matches!(
            result_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        let checkpoint = kernel
            .inner
            .context_projection
            .checkpoint()
            .expect("projection checkpoint at receipt");
        assert_eq!(checkpoint.through_seq, observed.seq);
        assert_eq!(
            checkpoint.through_event_id.as_deref(),
            Some(observed.event_id.as_str())
        );
        let snapshot = kernel
            .inner
            .context_projection
            .capture_snapshot("session", None)
            .expect("projection snapshot at receipt");
        assert!(
            snapshot
                .candidates()
                .iter()
                .any(|candidate| candidate.id == "publication-order")
        );

        release_tx.send(()).expect("release admission");
        let returned = result_rx
            .recv()
            .expect("admission result")
            .expect("successful admission");
        assert_eq!(
            serde_json::to_value(&observed).expect("serialize observed"),
            serde_json::to_value(&returned).expect("serialize returned")
        );
        assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));
        handle.join().expect("admission thread");
    }

    #[test]
    fn poisoned_context_admission_gate_is_typed() {
        let (_directory, kernel) = kernel();
        let before = kernel.event_count().expect("event count before poison");
        let mut receiver = kernel.subscribe();
        let inner = Arc::clone(&kernel.inner);
        let poisoned = catch_unwind(AssertUnwindSafe(move || {
            let _guard = inner
                .context_admission_gate
                .lock()
                .expect("initial gate lock");
            panic!("poison context admission gate");
        }));
        assert!(poisoned.is_err());

        let error = kernel
            .admit_context_node(TrustedContextNodeDraft::session(
                "session",
                node("unresolved".into()),
            ))
            .expect_err("poisoned gate must fail closed");
        assert!(matches!(error, KernelError::ContextAdmissionGatePoisoned));
        assert_eq!(
            kernel.event_count().expect("event count after poison"),
            before
        );
        assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));
    }
}

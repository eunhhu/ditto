//! Event-driven wakeups. Ditto intentionally has no periodic LLM heartbeat.

use ditto_protocol::new_id;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WakeEvent {
    ProcessExited {
        resource_handle: String,
        exit_code: Option<i32>,
    },
    FileChanged {
        path: String,
    },
    TimerFired {
        timer_id: String,
    },
    WebhookReceived {
        webhook_id: String,
    },
    DeviceOnline {
        device_id: String,
    },
    ApprovalGranted {
        lease_id: String,
    },
    UserInput {
        session_id: String,
    },
}

#[derive(Clone, Debug)]
pub struct WakeSender {
    sender: mpsc::Sender<WakeEvent>,
}

#[derive(Debug)]
pub struct EventScheduler {
    id: String,
    receiver: mpsc::Receiver<WakeEvent>,
}

impl EventScheduler {
    pub fn channel(capacity: usize) -> (WakeSender, Self) {
        let (sender, receiver) = mpsc::channel(capacity);
        (
            WakeSender { sender },
            Self {
                id: new_id("scheduler"),
                receiver,
            },
        )
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub async fn next(&mut self) -> Option<WakeEvent> {
        self.receiver.recv().await
    }
}

impl WakeSender {
    /// Queues a concrete event that may require new model judgment.
    ///
    /// # Errors
    ///
    /// Returns the unsent event when the scheduler has been dropped.
    pub async fn wake(&self, event: WakeEvent) -> Result<(), WakeEvent> {
        self.sender.send(event).await.map_err(|error| error.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn wakes_only_from_explicit_event() {
        let (sender, mut scheduler) = EventScheduler::channel(1);
        sender
            .wake(WakeEvent::TimerFired {
                timer_id: "deploy-timeout".to_owned(),
            })
            .await
            .unwrap();
        assert!(matches!(
            scheduler.next().await,
            Some(WakeEvent::TimerFired { .. })
        ));
    }
}

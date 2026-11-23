use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

/// Structured real-time events dispatched across the Dex-Harness ecosystem.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "data")]
pub enum DexEvent {
    /// A single thought or reasoning token streamed from the LLM.
    ThoughtToken { token: String },
    /// A tool execution has started.
    ToolStarted {
        tool_name: String,
        arguments: serde_json::Value,
    },
    /// A tool execution has completed.
    ToolFinished {
        tool_name: String,
        output: String,
        is_error: bool,
    },
    /// A concurrent subagent has been spawned.
    SubagentSpawned { role: String, task: String },
    /// A concurrent subagent has finished its task.
    SubagentCompleted { role: String, result: String },
    /// A project task status has been updated on the TaskBoard.
    TaskUpdated { task_id: String, status: String },
    /// Context compaction was performed.
    ContextCompacted { estimated_tokens: usize },
    /// A tool call was blocked or challenged by a security hook.
    SecurityBlocked { tool_name: String, reason: String },
    /// Progress update during a local model download.
    ModelDownloadProgress {
        model_id: String,
        file_name: String,
        downloaded_bytes: u64,
        total_bytes: u64,
        speed_mbps: f64,
    },
    /// A model download has finished and verified.
    ModelDownloadCompleted {
        model_id: String,
        file_name: String,
        total_bytes: u64,
    },
    /// A model download failed or was cancelled.
    ModelDownloadFailed {
        model_id: String,
        file_name: String,
        error: String,
    },
}

/// Central publish-subscribe asynchronous event bus based on `tokio::sync::broadcast`.
#[derive(Clone)]
pub struct EventBus {
    sender: broadcast::Sender<DexEvent>,
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(512)
    }
}

impl EventBus {
    pub const DEFAULT_CAPACITY: usize = 512;

    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self { sender }
    }

    /// Publishes an event to all active subscribers. Returns false if no subscribers are active.
    pub fn publish(&self, event: DexEvent) -> bool {
        self.sender.send(event).is_ok()
    }

    /// Subscribes to the event stream.
    pub fn subscribe(&self) -> broadcast::Receiver<DexEvent> {
        self.sender.subscribe()
    }

    /// Number of active receivers.
    pub fn receiver_count(&self) -> usize {
        self.sender.receiver_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_event_bus_pub_sub() {
        let bus = EventBus::new(16);
        let mut rx = bus.subscribe();

        let event = DexEvent::SubagentSpawned {
            role: "Explorer".to_string(),
            task: "Find all endpoints".to_string(),
        };

        bus.publish(event.clone());

        let received = rx.recv().await.expect("Failed to receive event");
        assert_eq!(received, event);
    }
}

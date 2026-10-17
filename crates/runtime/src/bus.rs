//! The event bus: persisted events fan out to subscribers in seq order.

use orc_domain::Event;
use tokio::sync::broadcast;

/// Capacity for the live broadcast; slow consumers get `Lagged`, never
/// dropped silently.
const BUS_CAPACITY: usize = 4096;

#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<Event>,
}

pub struct LaggedCount(pub u64);

impl EventBus {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(BUS_CAPACITY);
        Self { tx }
    }

    /// Publish a persisted event. Errors mean "no subscribers" — fine.
    pub fn publish(&self, event: Event) {
        let _ = self.tx.send(event);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

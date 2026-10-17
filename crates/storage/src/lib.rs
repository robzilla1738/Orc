//! `orc-storage` — durable state.
//!
//! SQLite in WAL mode. Every meaningful lifecycle change is an event row;
//! current-state tables are projections updated in the same transaction.
//! Readers never block the writer; the writer is serialized inside the
//! store so checkpointing stays predictable.
//!
//! Text deltas are batched — raw output lives in rotated transcript
//! files, `OutputAppended` events carry spans + previews.

mod chat;
mod error;
mod migrate;
mod store;
mod transcripts;

pub use chat::ChatMessage;
pub use error::{StorageError, StorageResult};
pub use store::*;
pub use transcripts::TranscriptStore;

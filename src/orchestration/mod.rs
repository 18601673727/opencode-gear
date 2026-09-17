//! Orchestration artifacts: currently the phase checkpoint store.
//!
//! This module owns durable, inspectable hand-offs between phases. It does not
//! own the conversation or the model loop — OpenCode does. A checkpoint is a
//! local JSON artifact under `.opencode-gear/checkpoints/`.

pub mod checkpoint;

pub use checkpoint::{Checkpoint, CheckpointSummary, LoadedCheckpoint, Phase, Staleness};

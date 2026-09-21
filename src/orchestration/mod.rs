//! Orchestration: typed role hand-offs, a local controller and the generated
//! OpenCode plugin adapter.
//!
//! This module owns durable, inspectable hand-offs between phases. It does not
//! own the conversation or the model loop — OpenCode does. The Rust controller
//! is the authority; the JavaScript adapter generated at launch is a thin
//! transport that only carries bytes to and from `ocg __bridge`.
//!
//! State lives under `.opencode-gear/orchestration/` (ignored local state) and
//! checkpoints stay under `.opencode-gear/checkpoints/`.

pub mod bridge;
pub mod checkpoint;
pub mod config;
pub mod controller;
pub mod handoff;
pub mod plugin;
pub mod projection;
pub mod state;

pub use checkpoint::{Checkpoint, CheckpointSummary, LoadedCheckpoint, Phase, Staleness};
pub use config::OrchestrationConfig;
pub use controller::{
    BuildDecision, BuildOutcome, Controller, ExploreDigest, HandoffOutcome, LeadContext,
};
pub use handoff::{
    HandoffFinding, HandoffVerification, ModelHandoffCapsule, ProjectionInput, Role, Severity,
    Transition,
};
pub use state::{
    Attempts, OrchestrationPhase, OrchestrationState, RepositoryBaseline, SessionState,
    STATE_SCHEMA_VERSION,
};

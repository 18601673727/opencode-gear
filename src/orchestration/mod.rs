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
//!
//! Two different lifetimes share that directory:
//!
//! - **Sessions are disposable execution state** (`state.json`): bounded,
//!   evicted, keyed by the OpenCode session id, recoverable-to-empty on
//!   corruption.
//! - **Missions are durable product state** (`missions/<mission_id>.json`):
//!   the versioned record of one admitted task, independent of any session,
//!   strictly versioned and quarantined on corruption.

pub mod bridge;
pub mod checkpoint;
pub mod config;
pub mod context_governor;
pub mod controller;
pub mod handoff;
pub mod mission;
pub mod plugin;
pub mod projection;
pub mod rollover;
pub mod state;

pub use checkpoint::{Checkpoint, CheckpointSummary, LoadedCheckpoint, Phase, Staleness};
pub use config::OrchestrationConfig;
pub use context_governor::{
    ContextGovernorConfig, ContextObservation, GovernorAction, GovernorDecision, GovernorState,
    ModelMetadata, TelemetryProvenance, TokenUsage,
};
pub use controller::{
    BuildDecision, BuildOutcome, ContextGovernanceResult, Controller, ExploreDigest,
    HandoffOutcome, LeadContext,
};
pub use handoff::{
    HandoffFinding, HandoffVerification, ModelHandoffCapsule, ProjectionInput, Role, Severity,
    Transition,
};
pub use mission::{
    Mission, MissionEvent, MissionEventKind, MissionRolloverState, MissionRolloverStatus,
    MissionStatus, MissionSummary, NextAction, MISSION_SCHEMA_VERSION,
};
pub use rollover::{
    ContinuationPacket, LeadBinding, RolloverArtifact, RolloverStatus, ROLLOVER_SCHEMA_VERSION,
};
pub use state::{
    Attempts, OrchestrationPhase, OrchestrationState, RepositoryBaseline, SessionState,
    STATE_SCHEMA_VERSION,
};

//! Durable Mission state: the product-state boundary above sessions.
//!
//! A **session is disposable execution state**. It lives in
//! `.opencode-gear/orchestration/state.json`, keyed by the OpenCode session
//! id, bounded and evicted; it can be dropped, compacted, rolled over or
//! replaced at any time without losing work.
//!
//! A **Mission is durable product state**: the versioned record of one
//! admitted task, keyed by `mission_id` (the deterministic task id derived
//! from the admitted task text — never from a session). It survives the
//! death, failure, compaction, replacement or rollover of any individual
//! OpenCode/model session:
//!
//! - progress is written per mission under
//!   `.opencode-gear/orchestration/missions/<mission_id>.json`, atomically
//!   (temp sibling + rename), alongside — never instead of — the session
//!   view;
//! - the current OpenCode session is recorded as `session_id`, a
//!   **replaceable execution binding**, not part of Mission identity;
//! - a fresh session that admits the same task binds to the same Mission
//!   and is seeded from it, so committed work (findings, attempts,
//!   checkpoints, verification) is never replayed from zero;
//! - `completed`, `failed` and `cancelled` are typed terminal states;
//! - a bounded history of consequential transitions (admission, session
//!   binding, checkpointed hand-offs, terminal states) carries a
//!   deterministic event identity each, so replaying the same transition
//!   identity is a no-op instead of a duplicate effect.
//!
//! Corruption is handled strictly, unlike the disposable session state: a
//! corrupt or unsupported Mission record is **quarantined** (renamed to
//! `<mission_id>.corrupt.json`, preserving the bytes for inspection) and the
//! load fails explicitly. It never silently reads as "no Mission", which
//! would restart committed work from scratch.
//!
//! The controller keeps the session view and the Mission in sync at every
//! mutation point; between them the Mission is authoritative. Single-owner
//! semantics still apply: reconciling two *concurrently live* sessions bound
//! to one Mission is deferred to the reconciliation work that builds on this
//! foundation.

use crate::error::{GearError, Result};
use crate::orchestration::handoff::{HandoffFinding, HandoffVerification, Role, Transition};
use crate::orchestration::state::{Attempts, OrchestrationPhase, SessionState};
use crate::verification::result::VerificationReport;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// The Mission schema version. Bumping it requires explicit migration code;
/// unknown versions are quarantined, never silently adopted.
pub const MISSION_SCHEMA_VERSION: u32 = 1;
/// The Mission directory under `.opencode-gear/orchestration/`.
pub const MISSIONS_DIR: &str = "missions";
/// The maximum number of transition events retained per Mission.
pub const MAX_MISSION_HISTORY: usize = 64;

/// The Mission lifecycle state. Terminal states are unambiguous and durable:
/// once a generation reaches one, the only way forward is a new generation
/// (an explicit re-admission), never a silent reset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissionStatus {
    /// Admitted and not yet terminal. Work may be in any phase.
    #[default]
    Active,
    /// Verification passed; the task completed cleanly.
    Completed,
    /// The task was explicitly failed.
    Failed,
    /// The task was explicitly cancelled.
    Cancelled,
}

impl MissionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            MissionStatus::Active => "active",
            MissionStatus::Completed => "completed",
            MissionStatus::Failed => "failed",
            MissionStatus::Cancelled => "cancelled",
        }
    }

    /// Whether this is a durable terminal state.
    pub fn is_terminal(self) -> bool {
        !matches!(self, MissionStatus::Active)
    }
}

/// The consequential transitions recorded in the Mission history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissionEventKind {
    /// The Mission was admitted (generation start).
    Admitted,
    /// The execution binding was set, replaced or released.
    SessionBound,
    /// Explore findings were consumed and checkpointed.
    ExploreToBuild,
    /// A post-Build verification was run and checkpointed.
    BuildToVerify,
    /// The Build budget was exhausted; Debug was recommended.
    VerifyToDebug,
    /// Work returned from Debug to Build.
    DebugToBuild,
    /// The generation completed cleanly.
    Completed,
    /// The generation was explicitly failed.
    Failed,
    /// The generation was explicitly cancelled.
    Cancelled,
}

impl MissionEventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MissionEventKind::Admitted => "admitted",
            MissionEventKind::SessionBound => "session_bound",
            MissionEventKind::ExploreToBuild => "explore_to_build",
            MissionEventKind::BuildToVerify => "build_to_verify",
            MissionEventKind::VerifyToDebug => "verify_to_debug",
            MissionEventKind::DebugToBuild => "debug_to_build",
            MissionEventKind::Completed => "completed",
            MissionEventKind::Failed => "failed",
            MissionEventKind::Cancelled => "cancelled",
        }
    }
}

/// One durable, idempotent transition record. `id` is deterministic over
/// `(mission_id, kind, generation, key)`, so re-recording the same logical
/// transition (for example after a crash/replay) collides with the existing
/// entry and is skipped instead of producing a duplicate effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MissionEvent {
    pub id: String,
    pub kind: MissionEventKind,
    pub generation: u32,
    /// The session that performed the transition, when one was involved.
    pub session_id: Option<String>,
    /// The checkpoint committing the transition's evidence, when one exists.
    pub checkpoint_id: Option<String>,
    /// A bounded, privacy-safe reason for consequential transitions.
    pub note: Option<String>,
    pub created_at: i64,
}

impl Default for MissionEvent {
    fn default() -> Self {
        Self {
            id: String::new(),
            kind: MissionEventKind::Admitted,
            generation: 1,
            session_id: None,
            checkpoint_id: None,
            note: None,
            created_at: 0,
        }
    }
}

/// The exact recoverable next semantic action of a non-terminal Mission,
/// derived from durable state alone — no conversation history required.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NextAction {
    /// The Lead owns the next move (fresh or reset admission).
    Lead,
    /// An Explore delegation is in flight or its result must be consumed.
    Explore,
    /// A Build (or a bounded Build retry) is next.
    Build,
    /// Verification is next or in flight.
    Verify,
    /// A Debug delegation is next.
    Debug,
    /// The Debug budget is exhausted: the user must decide.
    Escalate,
    /// Durable terminal state: completed.
    Complete,
    /// Durable terminal state: failed.
    Failed,
    /// Durable terminal state: cancelled.
    Cancelled,
}

impl NextAction {
    pub fn as_str(self) -> &'static str {
        match self {
            NextAction::Lead => "lead",
            NextAction::Explore => "explore",
            NextAction::Build => "build",
            NextAction::Verify => "verify",
            NextAction::Debug => "debug",
            NextAction::Escalate => "escalate",
            NextAction::Complete => "complete",
            NextAction::Failed => "failed",
            NextAction::Cancelled => "cancelled",
        }
    }
}

/// The durable Mission record.
///
/// The task-scoped fields mirror the session's live execution view
/// ([`SessionState`]); the controller syncs them at every mutation point.
/// Runtime/session-specific data (the repository baseline, the OpenCode
/// session entry itself) deliberately stays out of this record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Mission {
    pub schema_version: u32,
    /// Durable identity: the deterministic task id of the admitted task
    /// text. It never changes because a session does.
    pub mission_id: String,
    /// The admission generation. Re-admitting a terminal Mission starts the
    /// next generation with fresh progress; history is retained across
    /// generations.
    pub generation: u32,
    /// Desired state: the stored (bounded, secret-redacted) task text and
    /// the goal/constraints refined by exploration.
    pub task: Option<String>,
    pub goal: Option<String>,
    pub constraints: Vec<String>,
    /// Observed lifecycle state.
    pub status: MissionStatus,
    /// The operational sub-state (who owns the next move).
    pub phase: OrchestrationPhase,
    pub source: Role,
    pub destination: Option<Role>,
    pub last_transition: Option<Transition>,
    /// Attempt identity consumed so far.
    pub attempts: Attempts,
    /// The current execution binding: replaceable metadata, NOT identity.
    pub session_id: Option<String>,
    pub findings: Vec<HandoffFinding>,
    pub files: Vec<String>,
    pub symbols: Vec<String>,
    pub failures: Vec<String>,
    pub evidence: Vec<String>,
    pub debug_reason: Option<String>,
    pub last_verification: Option<HandoffVerification>,
    pub last_report: Option<VerificationReport>,
    pub last_rich_bytes: usize,
    pub last_diff_context: String,
    /// References to the meaningful-progress checkpoints (newest last).
    pub checkpoints: Vec<String>,
    /// The bounded durable transition history.
    pub history: Vec<MissionEvent>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Default for Mission {
    fn default() -> Self {
        Self {
            schema_version: MISSION_SCHEMA_VERSION,
            mission_id: String::new(),
            generation: 1,
            task: None,
            goal: None,
            constraints: Vec::new(),
            status: MissionStatus::Active,
            phase: OrchestrationPhase::Idle,
            source: Role::Lead,
            destination: Some(Role::Lead),
            last_transition: None,
            attempts: Attempts::default(),
            session_id: None,
            findings: Vec::new(),
            files: Vec::new(),
            symbols: Vec::new(),
            failures: Vec::new(),
            evidence: Vec::new(),
            debug_reason: None,
            last_verification: None,
            last_report: None,
            last_rich_bytes: 0,
            last_diff_context: String::new(),
            checkpoints: Vec::new(),
            history: Vec::new(),
            created_at: 0,
            updated_at: 0,
        }
    }
}

impl Mission {
    /// Admit a new Mission (generation 1, active, bound to `session_key`).
    pub fn admit(mission_id: &str, task_text: &str, session_key: &str, now: i64) -> Self {
        let mut mission = Self {
            mission_id: mission_id.to_string(),
            task: if task_text.is_empty() {
                None
            } else {
                Some(task_text.to_string())
            },
            session_id: Some(session_key.to_string()),
            created_at: now,
            updated_at: now,
            ..Self::default()
        };
        let event = mission.event(
            MissionEventKind::Admitted,
            "",
            None,
            Some(session_key.to_string()),
            None,
            now,
        );
        mission.record(event);
        mission
    }

    /// Whether the Mission reached a durable terminal state.
    pub fn is_terminal(&self) -> bool {
        self.status.is_terminal()
    }

    /// The exact recoverable next semantic action, from durable state alone.
    pub fn next_action(&self, max_debug_retries: usize) -> NextAction {
        match self.status {
            MissionStatus::Completed => NextAction::Complete,
            MissionStatus::Failed => NextAction::Failed,
            MissionStatus::Cancelled => NextAction::Cancelled,
            MissionStatus::Active => match self.phase {
                OrchestrationPhase::Idle => NextAction::Lead,
                OrchestrationPhase::Explore => NextAction::Explore,
                OrchestrationPhase::Build => NextAction::Build,
                OrchestrationPhase::Verify => NextAction::Verify,
                OrchestrationPhase::Debug => {
                    if self.attempts.debug > max_debug_retries {
                        NextAction::Escalate
                    } else {
                        NextAction::Debug
                    }
                }
                // A passed build marks the Mission completed in the same
                // transition; an active Mission in `Done` is defensive only.
                OrchestrationPhase::Done => NextAction::Complete,
            },
        }
    }

    /// Build a transition event with its deterministic identity. `key` is the
    /// transition-specific identity input: the checkpoint id for checkpointed
    /// hand-offs, the session id for bindings, empty for generation-scoped
    /// events (admission, terminal transitions).
    pub fn event(
        &self,
        kind: MissionEventKind,
        key: &str,
        checkpoint_id: Option<String>,
        session_id: Option<String>,
        note: Option<String>,
        created_at: i64,
    ) -> MissionEvent {
        MissionEvent {
            id: event_id(&self.mission_id, kind, self.generation, key),
            kind,
            generation: self.generation,
            session_id,
            checkpoint_id,
            note,
            created_at,
        }
    }

    /// Append an event, skipping an already-recorded identity. Returns
    /// whether the event was appended. This is the idempotent-replay
    /// mechanism: the same transition identity never applies twice.
    pub fn record(&mut self, event: MissionEvent) -> bool {
        if self.history.iter().any(|existing| existing.id == event.id) {
            return false;
        }
        self.history.push(event);
        if self.history.len() > MAX_MISSION_HISTORY {
            let excess = self.history.len() - MAX_MISSION_HISTORY;
            self.history.drain(0..excess);
        }
        true
    }

    /// Bind a session as the Mission's current execution state. A changed
    /// binding is recorded; rebinding the same session is a no-op.
    pub fn bind_session(&mut self, session_key: &str, now: i64) -> bool {
        if self.session_id.as_deref() == Some(session_key) {
            return false;
        }
        self.session_id = Some(session_key.to_string());
        let event = self.event(
            MissionEventKind::SessionBound,
            session_key,
            None,
            Some(session_key.to_string()),
            None,
            now,
        );
        self.record(event);
        self.updated_at = now;
        true
    }

    /// Release the current binding (the session moved to another Mission).
    pub fn release_session(&mut self, now: i64) -> bool {
        if self.session_id.is_none() {
            return false;
        }
        self.session_id = None;
        let event = self.event(
            MissionEventKind::SessionBound,
            "released",
            None,
            None,
            None,
            now,
        );
        self.record(event);
        self.updated_at = now;
        true
    }

    /// Start the next generation after a terminal state: progress resets,
    /// history is retained, identity is unchanged.
    pub fn begin_new_generation(&mut self, now: i64) {
        debug_assert!(self.is_terminal());
        self.generation += 1;
        self.status = MissionStatus::Active;
        self.phase = OrchestrationPhase::Idle;
        self.source = Role::Lead;
        self.destination = Some(Role::Lead);
        self.last_transition = None;
        self.attempts = Attempts::default();
        self.goal = None;
        self.constraints.clear();
        self.findings.clear();
        self.files.clear();
        self.symbols.clear();
        self.failures.clear();
        self.evidence.clear();
        self.debug_reason = None;
        self.last_verification = None;
        self.last_report = None;
        self.last_rich_bytes = 0;
        self.last_diff_context = String::new();
        self.checkpoints.clear();
        let event = self.event(
            MissionEventKind::Admitted,
            "",
            None,
            self.session_id.clone(),
            None,
            now,
        );
        self.record(event);
        self.updated_at = now;
    }

    /// Durable completion. Replaying the same completion (same generation) is
    /// a no-op; completing after another terminal state fails explicitly.
    pub fn complete(&mut self, session_key: &str, note: Option<String>, now: i64) -> Result<bool> {
        self.transition_to_terminal(MissionStatus::Completed, session_key, note, now)
    }

    /// Durable failure, with an inspectable reason.
    pub fn fail(&mut self, session_key: &str, note: Option<String>, now: i64) -> Result<bool> {
        self.transition_to_terminal(MissionStatus::Failed, session_key, note, now)
    }

    /// Durable cancellation, with an inspectable reason.
    pub fn cancel(&mut self, session_key: &str, note: Option<String>, now: i64) -> Result<bool> {
        self.transition_to_terminal(MissionStatus::Cancelled, session_key, note, now)
    }

    fn transition_to_terminal(
        &mut self,
        status: MissionStatus,
        session_key: &str,
        note: Option<String>,
        now: i64,
    ) -> Result<bool> {
        debug_assert!(status.is_terminal());
        let kind = match status {
            MissionStatus::Completed => MissionEventKind::Completed,
            MissionStatus::Failed => MissionEventKind::Failed,
            MissionStatus::Cancelled => MissionEventKind::Cancelled,
            MissionStatus::Active => unreachable!("terminal transition from active"),
        };
        let event = self.event(kind, "", None, Some(session_key.to_string()), note, now);
        if self.history.iter().any(|existing| existing.id == event.id) {
            // Idempotent replay of the same terminal transition identity.
            return Ok(false);
        }
        if self.status.is_terminal() {
            return Err(GearError::config(format!(
                "mission {} already reached a terminal state ({}) in generation {}; \
                 cannot transition to {} — re-admit the task to start a new generation",
                self.mission_id,
                self.status.as_str(),
                self.generation,
                status.as_str()
            )));
        }
        self.status = status;
        self.record(event);
        self.updated_at = now;
        Ok(true)
    }

    /// Sync the task-scoped durable fields from the session's live view.
    /// Called by the controller after every mutation, before persisting.
    ///
    /// A terminal generation is frozen: its record stays an inspectable
    /// snapshot of how the work ended. Only an explicit re-admission, which
    /// starts the next generation, can carry new progress.
    pub fn sync_from_session(&mut self, session: &SessionState) {
        debug_assert_eq!(session.task_id, self.mission_id);
        if self.is_terminal() {
            return;
        }
        self.task = session.task.clone();
        self.goal = session.goal.clone();
        self.constraints = session.constraints.clone();
        self.phase = session.phase;
        self.source = session.source;
        self.destination = session.destination;
        self.last_transition = session.last_transition;
        self.attempts = session.attempts;
        self.findings = session.findings.clone();
        self.files = session.files.clone();
        self.symbols = session.symbols.clone();
        self.failures = session.failures.clone();
        self.evidence = session.evidence.clone();
        self.debug_reason = session.debug_reason.clone();
        self.last_verification = session.last_verification.clone();
        self.last_report = session.last_report.clone();
        self.last_rich_bytes = session.last_rich_bytes;
        self.last_diff_context = session.last_diff_context.clone();
        self.checkpoints = session.checkpoints.clone();
        self.updated_at = session.updated_at;
    }

    /// Seed a fresh session's live view from this Mission. The repository
    /// baseline stays out: it is session-scoped and derived from the indexed
    /// repository generation, so the caller carries or re-renders it.
    pub fn seed_session(&self, session_key: &str, now: i64) -> SessionState {
        let mut session = SessionState::new(session_key, &self.mission_id, now);
        session.task = self.task.clone();
        session.goal = self.goal.clone();
        session.constraints = self.constraints.clone();
        session.phase = self.phase;
        session.source = self.source;
        session.destination = self.destination;
        session.last_transition = self.last_transition;
        session.attempts = self.attempts;
        session.findings = self.findings.clone();
        session.files = self.files.clone();
        session.symbols = self.symbols.clone();
        session.failures = self.failures.clone();
        session.evidence = self.evidence.clone();
        session.debug_reason = self.debug_reason.clone();
        session.last_verification = self.last_verification.clone();
        session.last_report = self.last_report.clone();
        session.last_rich_bytes = self.last_rich_bytes;
        session.last_diff_context = self.last_diff_context.clone();
        session.checkpoints = self.checkpoints.clone();
        session
    }
}

/// A deterministic, safe event identity. Replays of the same logical
/// transition produce the same id and are deduplicated by [`Mission::record`].
fn event_id(mission_id: &str, kind: MissionEventKind, generation: u32, key: &str) -> String {
    let digest = crate::runtime::hash::sha256_hex(
        format!(
            "mission-v1|{mission_id}|{}|{generation}|{key}",
            kind.as_str()
        )
        .as_bytes(),
    );
    let short = digest.get(..16).unwrap_or(&digest);
    format!("ev-{}-{short}", kind.as_str().replace('_', "-"))
}

/// The Mission directory.
pub fn missions_dir(root: &Path) -> PathBuf {
    crate::orchestration::state::state_dir(root).join(MISSIONS_DIR)
}

/// The path for one Mission id. Unsafe ids are rejected, never hashed into
/// place: Mission identity must be used verbatim.
pub fn mission_path(root: &Path, mission_id: &str) -> Result<PathBuf> {
    if !crate::orchestration::checkpoint::is_safe_id(mission_id) {
        return Err(GearError::config(format!(
            "unsafe mission id '{mission_id}' (expected lowercase letters, digits and '-')"
        )));
    }
    Ok(missions_dir(root).join(format!("{mission_id}.json")))
}

/// Load one Mission by id.
///
/// - `Ok(None)`: no Mission record exists (that is not corruption).
/// - `Ok(Some(_))`: the durable record, verified against the schema version.
/// - `Err(_)`: the record is corrupt or from an unsupported schema version.
///   It was quarantined to `<mission_id>.corrupt.json` (bytes preserved) and
///   must never be silently treated as "no Mission".
pub fn load(root: &Path, mission_id: &str) -> Result<Option<Mission>> {
    let path = mission_path(root, mission_id)?;
    if !path.is_file() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path).map_err(|error| GearError::read(&path, error))?;
    match serde_json::from_str::<Mission>(&text) {
        Ok(mission) if mission.schema_version == MISSION_SCHEMA_VERSION => Ok(Some(mission)),
        Ok(mission) => Err(quarantine(
            &path,
            mission_id,
            &format!(
                "unsupported schema_version {} (expected {MISSION_SCHEMA_VERSION})",
                mission.schema_version
            ),
        )),
        Err(error) => Err(quarantine(
            &path,
            mission_id,
            &format!("not valid JSON: {error}"),
        )),
    }
}

/// Persist a Mission atomically. Ensures the state tree stays git-ignored.
pub fn save(root: &Path, mission: &Mission) -> Result<PathBuf> {
    crate::runtime::install::ensure_gitignore(root)?;
    let path = mission_path(root, &mission.mission_id)?;
    let value = serde_json::to_value(mission).map_err(|error| {
        GearError::config(format!(
            "cannot serialize mission {}: {error}",
            mission.mission_id
        ))
    })?;
    crate::runtime::install::write_json_atomic(&path, &value)?;
    Ok(path)
}

/// A short summary used by `ocg doctor` and tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissionSummary {
    pub mission_id: String,
    pub status: MissionStatus,
    pub phase: OrchestrationPhase,
    pub generation: u32,
    pub session_id: Option<String>,
    pub updated_at: i64,
    pub file: String,
}

/// List every readable Mission, most recently updated first. Read-only:
/// corrupt files are counted, never returned, never quarantined here, and
/// already-quarantined `.corrupt.json` records are skipped.
pub fn list(root: &Path) -> (Vec<MissionSummary>, usize) {
    let dir = missions_dir(root);
    let Ok(entries) = fs::read_dir(&dir) else {
        return (Vec::new(), 0);
    };
    let mut summaries = Vec::new();
    let mut corrupt = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        if name.ends_with(".corrupt.json") {
            continue;
        }
        if path
            .extension()
            .map(|extension| extension != "json")
            .unwrap_or(true)
        {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            corrupt += 1;
            continue;
        };
        match serde_json::from_str::<Mission>(&text) {
            Ok(mission) if mission.schema_version == MISSION_SCHEMA_VERSION => {
                summaries.push(MissionSummary {
                    mission_id: mission.mission_id,
                    status: mission.status,
                    phase: mission.phase,
                    generation: mission.generation,
                    session_id: mission.session_id,
                    updated_at: mission.updated_at,
                    file: name,
                });
            }
            _ => corrupt += 1,
        }
    }
    summaries.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| a.mission_id.cmp(&b.mission_id))
    });
    (summaries, corrupt)
}

/// Quarantine a corrupt record (preserving its bytes) and build the explicit
/// error. The corrupt file is renamed aside, never deleted and never silently
/// replaced by an empty Mission.
fn quarantine(path: &Path, mission_id: &str, reason: &str) -> GearError {
    let quarantined = path.with_extension("corrupt.json");
    // One quarantine slot per Mission: the newest corrupt record wins, so the
    // recoverable data preserved is always the most recent state.
    let _ = fs::remove_file(&quarantined);
    let note = match fs::rename(path, &quarantined) {
        Ok(()) => format!("the record was quarantined to {}", quarantined.display()),
        Err(_) => "the record could not be quarantined".to_string(),
    };
    GearError::config(format!(
        "mission {mission_id} is corrupt ({reason}); {note}. \
         It was NOT silently reset — inspect the quarantined record to recover it"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const TASK_ID: &str = "task-0123456789abcdef";

    fn admitted() -> Mission {
        Mission::admit(TASK_ID, "update the parser", "session-1", 7)
    }

    #[test]
    fn mission_serializes_with_a_stable_versioned_shape() {
        let mission = admitted();
        let value = serde_json::to_value(&mission).unwrap();
        assert_eq!(value["schema_version"], json!(MISSION_SCHEMA_VERSION));
        assert_eq!(value["mission_id"], json!(TASK_ID));
        assert_eq!(value["generation"], json!(1));
        assert_eq!(value["status"], json!("active"));
        assert_eq!(value["phase"], json!("idle"));
        assert_eq!(value["session_id"], json!("session-1"));
        assert_eq!(value["task"], json!("update the parser"));
        assert!(value["history"].is_array());
        let parsed: Mission = serde_json::from_value(value).unwrap();
        assert_eq!(parsed, mission);
    }

    #[test]
    fn mission_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let mut mission = admitted();
        mission.attempts.build = 2;
        mission.phase = OrchestrationPhase::Build;
        mission.checkpoints.push("cp-1".to_string());
        let path = save(dir.path(), &mission).unwrap();
        assert!(path.is_file());
        let loaded = load(dir.path(), TASK_ID).unwrap().unwrap();
        assert_eq!(loaded, mission);
        assert_eq!(loaded.attempts.build, 2);
        // A different mission id is a clean miss, not corruption.
        assert_eq!(load(dir.path(), "task-ffffffffffffffff").unwrap(), None);
    }

    #[test]
    fn unsafe_mission_ids_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        assert!(mission_path(dir.path(), "../escape").is_err());
        assert!(mission_path(dir.path(), "task/x").is_err());
        assert!(mission_path(dir.path(), TASK_ID).is_ok());
        assert!(load(dir.path(), "../escape").is_err());
    }

    #[test]
    fn corrupt_mission_is_quarantined_and_reported_never_silently_reset() {
        let dir = tempfile::tempdir().unwrap();
        let mission = admitted();
        let path = save(dir.path(), &mission).unwrap();
        fs::write(&path, "{ this is not json").unwrap();
        let error = load(dir.path(), TASK_ID).unwrap_err();
        let text = error.to_string();
        assert!(text.contains("corrupt"), "{text}");
        assert!(text.contains("quarantined"), "{text}");
        // The live path was moved aside; the corrupt bytes are preserved.
        assert!(!path.exists());
        let quarantined = path.with_extension("corrupt.json");
        assert_eq!(
            fs::read_to_string(&quarantined).unwrap(),
            "{ this is not json"
        );
        // After quarantine the id is a clean miss — but only because the
        // explicit error already surfaced and the data was preserved.
        assert_eq!(load(dir.path(), TASK_ID).unwrap(), None);
        // Listing skips the quarantined record.
        let (summaries, corrupt) = list(dir.path());
        assert!(summaries.is_empty());
        assert_eq!(corrupt, 0);
    }

    #[test]
    fn unknown_schema_version_is_quarantined() {
        let dir = tempfile::tempdir().unwrap();
        let path = mission_path(dir.path(), TASK_ID).unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"{"schema_version":99,"mission_id":"task-0123456789abcdef"}"#,
        )
        .unwrap();
        let error = load(dir.path(), TASK_ID).unwrap_err();
        assert!(error.to_string().contains("schema_version 99"), "{error}");
        assert!(path.with_extension("corrupt.json").is_file());
    }

    #[test]
    fn terminal_states_are_typed_and_invalid_transitions_fail() {
        let mut mission = admitted();
        assert!(!mission.is_terminal());
        // Complete: active -> completed.
        assert!(mission.complete("session-1", None, 10).unwrap());
        assert_eq!(mission.status, MissionStatus::Completed);
        assert!(mission.is_terminal());
        // Replaying the same completion identity is a no-op, not an error.
        assert!(!mission.complete("session-1", None, 11).unwrap());
        // A different terminal transition is invalid and fails explicitly.
        let error = mission.fail("session-1", Some("too late".to_string()), 12);
        assert!(error.is_err());
        assert!(error
            .unwrap_err()
            .to_string()
            .contains("already reached a terminal state"));
        let error = mission.cancel("session-1", None, 13);
        assert!(error.is_err());

        // Cancel and fail behave the same way from active.
        let mut other = admitted();
        assert!(other.cancel("session-1", None, 10).unwrap());
        assert!(!other.cancel("session-1", None, 11).unwrap());
        assert!(other.complete("session-1", None, 12).is_err());
        let mut third = admitted();
        assert!(third.fail("session-1", None, 10).unwrap());
        assert!(!third.fail("session-1", None, 11).unwrap());
        assert!(third.cancel("session-1", None, 12).is_err());
    }

    #[test]
    fn terminal_states_persist_with_reasons() {
        let dir = tempfile::tempdir().unwrap();
        let mut failed = admitted();
        failed
            .fail("session-1", Some("abandoned by the user".to_string()), 10)
            .unwrap();
        save(dir.path(), &failed).unwrap();
        let loaded = load(dir.path(), TASK_ID).unwrap().unwrap();
        assert_eq!(loaded.status, MissionStatus::Failed);
        let event = loaded
            .history
            .iter()
            .find(|event| event.kind == MissionEventKind::Failed)
            .unwrap();
        assert_eq!(event.note.as_deref(), Some("abandoned by the user"));
        assert_eq!(event.session_id.as_deref(), Some("session-1"));
    }

    #[test]
    fn event_recording_is_idempotent_and_bounded() {
        let mut mission = admitted();
        let event = mission.event(
            MissionEventKind::BuildToVerify,
            "cp-build-to-verify-abc",
            Some("cp-build-to-verify-abc".to_string()),
            Some("session-1".to_string()),
            None,
            10,
        );
        assert!(event.id.starts_with("ev-build-to-verify-"));
        let before = mission.history.len();
        assert!(mission.record(event.clone()));
        // The exact same transition identity is a no-op.
        assert!(!mission.record(event));
        assert_eq!(mission.history.len(), before + 1);
        // Event ids are deterministic.
        let again = mission.event(
            MissionEventKind::BuildToVerify,
            "cp-build-to-verify-abc",
            None,
            None,
            None,
            11,
        );
        let first = mission
            .history
            .iter()
            .find(|event| event.kind == MissionEventKind::BuildToVerify)
            .unwrap();
        assert_eq!(again.id, first.id);
        // History is bounded.
        for index in 0..(MAX_MISSION_HISTORY + 8) {
            let event = mission.event(
                MissionEventKind::BuildToVerify,
                &format!("cp-{index}"),
                None,
                None,
                None,
                index as i64,
            );
            mission.record(event);
        }
        assert!(mission.history.len() <= MAX_MISSION_HISTORY);
    }

    #[test]
    fn new_generation_resets_progress_but_keeps_identity_and_history() {
        let mut mission = admitted();
        mission.attempts.build = 3;
        mission.phase = OrchestrationPhase::Done;
        mission
            .findings
            .push(crate::orchestration::handoff::HandoffFinding {
                summary: "a finding".to_string(),
                detail: None,
                source: None,
                severity: crate::orchestration::handoff::Severity::Info,
            });
        mission.checkpoints.push("cp-1".to_string());
        mission.complete("session-1", None, 10).unwrap();
        let history_before = mission.history.len();

        mission.begin_new_generation(20);
        assert_eq!(mission.mission_id, TASK_ID);
        assert_eq!(mission.generation, 2);
        assert_eq!(mission.status, MissionStatus::Active);
        assert_eq!(mission.phase, OrchestrationPhase::Idle);
        assert_eq!(mission.attempts, Attempts::default());
        assert!(mission.findings.is_empty());
        assert!(mission.checkpoints.is_empty());
        // The terminal event and the new admission are both in the history.
        assert!(mission.history.len() > history_before);
        assert!(mission
            .history
            .iter()
            .any(|event| event.kind == MissionEventKind::Completed && event.generation == 1));
        assert!(mission
            .history
            .iter()
            .any(|event| event.kind == MissionEventKind::Admitted && event.generation == 2));
        // A new generation can complete again: the identity differs.
        assert!(mission.complete("session-1", None, 30).unwrap());
    }

    #[test]
    fn session_binding_is_replaceable_and_recorded() {
        let mut mission = admitted();
        assert_eq!(mission.session_id.as_deref(), Some("session-1"));
        // Rebinding the same session is a no-op.
        assert!(!mission.bind_session("session-1", 10));
        // Replacing the binding keeps identity and is recorded.
        assert!(mission.bind_session("session-2", 11));
        assert_eq!(mission.mission_id, TASK_ID);
        assert_eq!(mission.session_id.as_deref(), Some("session-2"));
        assert!(mission.history.iter().any(|event| {
            event.kind == MissionEventKind::SessionBound
                && event.session_id.as_deref() == Some("session-2")
        }));
        // Releasing the binding clears it without touching identity.
        assert!(mission.release_session(12));
        assert_eq!(mission.session_id, None);
        assert!(!mission.release_session(13));
    }

    #[test]
    fn next_action_is_derived_from_durable_state() {
        let mut mission = admitted();
        assert_eq!(mission.next_action(1), NextAction::Lead);
        mission.phase = OrchestrationPhase::Explore;
        assert_eq!(mission.next_action(1), NextAction::Explore);
        mission.phase = OrchestrationPhase::Build;
        assert_eq!(mission.next_action(1), NextAction::Build);
        mission.phase = OrchestrationPhase::Verify;
        assert_eq!(mission.next_action(1), NextAction::Verify);
        mission.phase = OrchestrationPhase::Debug;
        mission.attempts.debug = 1;
        assert_eq!(mission.next_action(1), NextAction::Debug);
        mission.attempts.debug = 2;
        assert_eq!(mission.next_action(1), NextAction::Escalate);
        mission.complete("session-1", None, 10).unwrap();
        assert_eq!(mission.next_action(1), NextAction::Complete);
    }

    #[test]
    fn list_reports_live_missions_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let mut older = Mission::admit("task-aaaaaaaaaaaaaaaa", "one", "s", 1);
        older.updated_at = 1;
        let mut newer = Mission::admit("task-bbbbbbbbbbbbbbbb", "two", "s", 2);
        newer.updated_at = 2;
        save(dir.path(), &older).unwrap();
        save(dir.path(), &newer).unwrap();
        // A corrupt sibling is counted, never listed, never quarantined here.
        fs::write(
            missions_dir(dir.path()).join("task-cccccccccccccccc.json"),
            "{bad",
        )
        .unwrap();
        let (summaries, corrupt) = list(dir.path());
        assert_eq!(corrupt, 1);
        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].mission_id, "task-bbbbbbbbbbbbbbbb");
        assert_eq!(summaries[1].mission_id, "task-aaaaaaaaaaaaaaaa");
        assert_eq!(summaries[0].status, MissionStatus::Active);
        // The corrupt file is untouched by listing.
        assert!(missions_dir(dir.path())
            .join("task-cccccccccccccccc.json")
            .is_file());
    }

    #[test]
    fn seed_and_sync_preserve_the_task_scoped_view() {
        let mut mission = admitted();
        mission.goal = Some("goal".to_string());
        mission.attempts.build = 2;
        mission.phase = OrchestrationPhase::Build;
        mission.checkpoints.push("cp-1".to_string());
        let session = mission.seed_session("session-9", 42);
        assert_eq!(session.session_id, "session-9");
        assert_eq!(session.task_id, TASK_ID);
        assert_eq!(session.attempts.build, 2);
        assert_eq!(session.phase, OrchestrationPhase::Build);
        assert_eq!(session.goal.as_deref(), Some("goal"));
        assert_eq!(session.checkpoints, vec!["cp-1".to_string()]);

        let mut synced = Mission::admit(TASK_ID, "", "session-9", 1);
        synced.sync_from_session(&session);
        assert_eq!(synced.goal.as_deref(), Some("goal"));
        assert_eq!(synced.attempts.build, 2);
        assert_eq!(synced.phase, OrchestrationPhase::Build);
        assert_eq!(synced.checkpoints, vec!["cp-1".to_string()]);
    }

    #[test]
    fn a_terminal_generation_is_frozen_against_later_session_activity() {
        let mut mission = admitted();
        mission.phase = OrchestrationPhase::Done;
        mission.attempts.build = 2;
        mission.checkpoints.push("cp-1".to_string());
        mission.complete("session-1", None, 10).unwrap();
        let frozen = mission.clone();

        // A session that keeps running after the Mission ended cannot rewrite
        // how the work ended.
        let mut session = mission.seed_session("session-2", 20);
        session.phase = OrchestrationPhase::Build;
        session.attempts.build = 9;
        mission.sync_from_session(&session);
        assert_eq!(mission, frozen);
        assert_eq!(mission.next_action(1), NextAction::Complete);
        assert_eq!(mission.next_action(1).as_str(), "complete");
    }
}

//! Local orchestration state.
//!
//! State lives under `<project>/.opencode-gear/orchestration/state.json`. It is
//! intentionally small, inspectable and **fail-soft**:
//!
//! - a corrupt or unsupported state file is reported and replaced with a fresh
//!   empty state, never an error;
//! - ids are hashed to safe `[a-z0-9-]` names before they become keys or paths,
//!   so a hostile session id cannot traverse the filesystem;
//! - a bounded number of sessions is retained (newest first) so the file cannot
//!   grow without limit.
//!
//! This module never contacts a model, a network or OpenCode. It stores the
//! controller's phase, retry budgets and the latest bounded findings so a later
//! bridge call can resume the same task.

use crate::orchestration::handoff::{
    HandoffFinding, HandoffVerification, ModelHandoffCapsule, Role, Transition,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// The state directory under `.opencode-gear/`.
pub const ORCHESTRATION_DIR: &str = "orchestration";
/// The state file name.
pub const STATE_FILE: &str = "state.json";
/// The state schema version.
pub const STATE_SCHEMA_VERSION: u32 = 1;
/// The maximum number of sessions retained.
pub const MAX_SESSIONS: usize = 16;
/// The maximum number of bounded findings retained per session.
pub const MAX_SESSION_FINDINGS: usize = 32;

/// The controller phase. `Idle` is a fresh session; `Done` means the task
/// completed cleanly; `Debug` means a Debug hand-off is recommended or active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestrationPhase {
    #[default]
    Idle,
    Explore,
    Build,
    Verify,
    Debug,
    Done,
}

impl OrchestrationPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            OrchestrationPhase::Idle => "idle",
            OrchestrationPhase::Explore => "explore",
            OrchestrationPhase::Build => "build",
            OrchestrationPhase::Verify => "verify",
            OrchestrationPhase::Debug => "debug",
            OrchestrationPhase::Done => "done",
        }
    }
}

/// The retry budget actually consumed by one session.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Attempts {
    pub build: usize,
    /// Verification runs that were executed after Build. Retained for metrics
    /// and post-hoc analysis; the retry *policy* is driven by `build` and
    /// `debug`.
    pub verify: usize,
    pub debug: usize,
}

/// One task session's bounded state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionState {
    pub session_id: String,
    pub task_id: String,
    pub phase: OrchestrationPhase,
    pub source: Role,
    pub destination: Option<Role>,
    pub last_transition: Option<Transition>,
    pub task: Option<String>,
    pub goal: Option<String>,
    pub constraints: Vec<String>,
    pub findings: Vec<HandoffFinding>,
    pub files: Vec<String>,
    pub symbols: Vec<String>,
    pub failures: Vec<String>,
    pub evidence: Vec<String>,
    pub debug_reason: Option<String>,
    pub last_verification: Option<HandoffVerification>,
    /// The most recent structured verification report. Kept so a later
    /// Debug→Build checkpoint can record the actual verification result.
    pub last_report: Option<crate::verification::result::VerificationReport>,
    pub checkpoints: Vec<String>,
    pub attempts: Attempts,
    /// The byte size of the most recent full rich task context. Used as the
    /// ratio reference for deliberately narrow hand-offs such as Debug.
    pub last_rich_bytes: usize,
    /// The most recent bounded, real diff rendering. Reused by Debug hand-offs
    /// so they carry a relevant diff without re-planning.
    pub last_diff_context: String,
    /// The identity of the last repository context snapshot injected into this
    /// session. The bridge compares the freshly prepared identity against it so
    /// an unchanged snapshot is not appended again on a later turn. `None`
    /// means nothing has been injected yet.
    #[serde(default)]
    pub last_snapshot_id: Option<String>,
    pub updated_at: i64,
}

impl Default for SessionState {
    fn default() -> Self {
        Self {
            session_id: String::new(),
            task_id: String::new(),
            phase: OrchestrationPhase::Idle,
            source: Role::Lead,
            destination: None,
            last_transition: None,
            task: None,
            goal: None,
            constraints: Vec::new(),
            findings: Vec::new(),
            files: Vec::new(),
            symbols: Vec::new(),
            failures: Vec::new(),
            evidence: Vec::new(),
            debug_reason: None,
            last_verification: None,
            last_report: None,
            checkpoints: Vec::new(),
            attempts: Attempts::default(),
            last_rich_bytes: 0,
            last_diff_context: String::new(),
            last_snapshot_id: None,
            updated_at: 0,
        }
    }
}

impl SessionState {
    pub fn new(session_id: &str, task_id: &str, now: i64) -> Self {
        Self {
            session_id: session_id.to_string(),
            task_id: task_id.to_string(),
            updated_at: now,
            ..Self::default()
        }
    }

    /// Add a bounded finding, dropping the oldest beyond the cap.
    pub fn push_finding(&mut self, finding: HandoffFinding) {
        if let Some(existing) = self
            .findings
            .iter_mut()
            .find(|existing| existing.summary == finding.summary)
        {
            if finding.severity > existing.severity {
                existing.severity = finding.severity;
            }
            return;
        }
        self.findings.push(finding);
        if self.findings.len() > MAX_SESSION_FINDINGS {
            let excess = self.findings.len() - MAX_SESSION_FINDINGS;
            self.findings.drain(0..excess);
        }
    }

    /// Record a checkpoint id, deduplicated and bounded.
    pub fn push_checkpoint(&mut self, id: &str) {
        if !self.checkpoints.iter().any(|existing| existing == id) {
            self.checkpoints.push(id.to_string());
        }
        if self.checkpoints.len() > 32 {
            let excess = self.checkpoints.len() - 32;
            self.checkpoints.drain(0..excess);
        }
    }
}

/// The whole state document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OrchestrationState {
    pub schema_version: u32,
    pub updated_at: i64,
    pub sessions: BTreeMap<String, SessionState>,
}

impl Default for OrchestrationState {
    fn default() -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            updated_at: 0,
            sessions: BTreeMap::new(),
        }
    }
}

impl OrchestrationState {
    pub fn session(&self, session_id: &str) -> Option<&SessionState> {
        self.sessions.get(&safe_id(session_id))
    }

    /// Insert or replace a session and enforce the retention bound. Newest
    /// `updated_at` wins; ties fall back to the key so ordering is stable.
    pub fn upsert(&mut self, mut session: SessionState, now: i64) {
        let key = safe_id(&session.session_id);
        session.session_id = key.clone();
        session.updated_at = now;
        self.sessions.insert(key, session);
        self.updated_at = now;
        if self.sessions.len() > MAX_SESSIONS {
            let mut entries: Vec<(String, i64)> = self
                .sessions
                .iter()
                .map(|(key, session)| (key.clone(), session.updated_at))
                .collect();
            entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.0.cmp(&a.0)));
            for (key, _) in entries.into_iter().skip(MAX_SESSIONS) {
                self.sessions.remove(&key);
            }
        }
    }
}

/// A safe, deterministic id. Unsafe ids are hashed so raw text never becomes a
/// key or a path component.
pub fn safe_id(seed: &str) -> String {
    let trimmed = seed.trim();
    if !trimmed.is_empty()
        && trimmed.len() <= 96
        && trimmed
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return trimmed.to_string();
    }
    let digest = crate::runtime::hash::sha256_hex(seed.as_bytes());
    format!("s-{}", digest.get(..16).unwrap_or(&digest))
}

/// The orchestration state directory.
pub fn state_dir(root: &Path) -> PathBuf {
    root.join(crate::context::repomap::GEAR_DIR)
        .join(ORCHESTRATION_DIR)
}

/// The state file path.
pub fn state_path(root: &Path) -> PathBuf {
    state_dir(root).join(STATE_FILE)
}

/// The result of a load, including corruption accounting for diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedState {
    pub state: OrchestrationState,
    pub corrupt: bool,
    pub exists: bool,
}

/// Load state, recovering to an empty state on a missing, corrupt or
/// unsupported file. Never creates state and never fails.
pub fn load(root: &Path) -> LoadedState {
    let path = state_path(root);
    if !path.is_file() {
        return LoadedState {
            state: OrchestrationState::default(),
            corrupt: false,
            exists: false,
        };
    }
    match fs::read_to_string(&path) {
        Ok(text) => match serde_json::from_str::<OrchestrationState>(&text) {
            Ok(state) if state.schema_version == STATE_SCHEMA_VERSION => LoadedState {
                state,
                corrupt: false,
                exists: true,
            },
            _ => LoadedState {
                state: OrchestrationState::default(),
                corrupt: true,
                exists: true,
            },
        },
        Err(_) => LoadedState {
            state: OrchestrationState::default(),
            corrupt: true,
            exists: true,
        },
    }
}

/// Persist state atomically. Ensures the state tree stays git-ignored.
pub fn save(root: &Path, state: &OrchestrationState) -> crate::error::Result<PathBuf> {
    crate::runtime::install::ensure_gitignore(root)?;
    let path = state_path(root);
    let value = serde_json::to_value(state).map_err(|error| {
        crate::error::GearError::config(format!("cannot serialize orchestration state: {error}"))
    })?;
    crate::runtime::install::write_json_atomic(&path, &value)?;
    Ok(path)
}

/// Whether a hand-off capsule's session is still present and fresh. A missing
/// session is *not* stale; it simply has no prior state.
pub fn matches_session(state: &OrchestrationState, capsule: &ModelHandoffCapsule) -> bool {
    state
        .session(&capsule.session_id)
        .map(|session| session.task_id == capsule.task_id)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::handoff::{HandoffFinding, Role, Severity};

    #[test]
    fn safe_ids_hash_unsafe_input() {
        assert_eq!(safe_id("session-1"), "session-1");
        let hashed = safe_id("../etc/passwd");
        assert!(hashed.starts_with("s-"));
        assert!(!hashed.contains('/'));
        assert!(!hashed.contains(".."));
        assert_eq!(hashed, safe_id("../etc/passwd"));
    }

    #[test]
    fn state_round_trips_and_bounds_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = OrchestrationState::default();
        for index in 0..(MAX_SESSIONS + 4) {
            state.upsert(
                SessionState::new(&format!("session-{index}"), "task", index as i64),
                index as i64,
            );
        }
        assert_eq!(state.sessions.len(), MAX_SESSIONS);
        // The newest survive.
        assert!(state.session("session-0").is_none());
        assert!(state
            .session(&format!("session-{}", MAX_SESSIONS + 3))
            .is_some());
        save(dir.path(), &state).unwrap();
        let loaded = load(dir.path());
        assert!(!loaded.corrupt);
        assert_eq!(loaded.state.sessions.len(), MAX_SESSIONS);
    }

    #[test]
    fn corrupt_state_recovers_to_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = state_path(dir.path());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "{not json").unwrap();
        let loaded = load(dir.path());
        assert!(loaded.corrupt);
        assert!(loaded.exists);
        assert!(loaded.state.sessions.is_empty());
        // A later save overwrites the corruption.
        save(dir.path(), &OrchestrationState::default()).unwrap();
        assert!(!load(dir.path()).corrupt);
    }

    #[test]
    fn unknown_schema_is_corrupt_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let path = state_path(dir.path());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, r#"{"schema_version":99,"sessions":{}}"#).unwrap();
        let loaded = load(dir.path());
        assert!(loaded.corrupt);
        assert!(loaded.state.sessions.is_empty());
    }

    #[test]
    fn findings_are_bounded_and_deduplicated() {
        let mut session = SessionState::new("s", "t", 1);
        for index in 0..(MAX_SESSION_FINDINGS + 5) {
            session.push_finding(HandoffFinding {
                summary: format!("finding {index}"),
                detail: None,
                source: None,
                severity: Severity::Info,
            });
        }
        assert_eq!(session.findings.len(), MAX_SESSION_FINDINGS);
        // Re-adding an existing summary does not duplicate it.
        let before = session.findings.len();
        session.push_finding(HandoffFinding {
            summary: "finding 10".to_string(),
            detail: None,
            source: None,
            severity: Severity::Critical,
        });
        assert_eq!(session.findings.len(), before);
        assert!(session.findings.iter().any(
            |finding| finding.summary == "finding 10" && finding.severity == Severity::Critical
        ));
        assert_eq!(session.source, Role::Lead);
    }
}

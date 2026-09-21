//! The Rust orchestration controller.
//!
//! This is where every orchestration decision is made. The generated JavaScript
//! adapter is intentionally inert: it only carries bytes between OpenCode and
//! this controller through `ocg __bridge`. Context ranking, projection, policy,
//! freshness, retry budgets, checkpointing and telemetry all live here.
//!
//! ```text
//! chat.message      -> prepare_lead_context   (dynamic context suffix)
//! task before       -> prepare_handoff        (typed role projection)
//! task after        -> consume_explore_result (bounded findings + checkpoint)
//!                   -> after_build            (verification + retry/debug policy)
//! ```
//!
//! Invariants:
//!
//! - No arbitrary shell. Verification uses the existing trusted, structured
//!   `verification` configuration and the shared [`CaptureRunner`].
//! - Fail-soft state. A corrupt state, checkpoint or cache is skipped and
//!   reported, never fatal, and never silently reused.
//! - No learned router. The only inputs are explicit config and deterministic
//!   local evidence.

use crate::capabilities::{CapabilityConfig, CapabilityEvidence, CapabilityPlan};
use crate::clock::Clock;
use crate::context::capsule::{CapsuleFile, Finding as CapsuleFinding, TaskCapsule};
use crate::context::config::ContextConfig;
use crate::context::engine::{ContextEngine, PlanOutcome};
use crate::context::freshness::{Provenance, ENGINE_VERSION, SCHEMA_VERSION};
use crate::context::gitdiff::{snapshot_fingerprint, GitSnapshot};
use crate::error::Result;
use crate::orchestration::checkpoint::{self, Checkpoint, Phase};
use crate::orchestration::config::OrchestrationConfig;
use crate::orchestration::handoff::{
    HandoffFinding, HandoffVerification, ModelHandoffCapsule, ProjectionInput, Role, Severity,
};
use crate::orchestration::projection::{self, ProjectionLimits};
use crate::orchestration::state::{self, Attempts, OrchestrationPhase, SessionState};
use crate::process::{CaptureRunner, GitHost};
use crate::telemetry::OrchestrationMetrics;
use crate::verification::config::VerificationConfig;
use crate::verification::distill;
use crate::verification::result::VerificationReport;
use crate::verification::runner::{execute, VerifyRequest};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// A compact, machine-followable response contract appended to Explore
/// hand-offs. It is advisory: [`parse_explore_output`] still falls back to a
/// deterministic line parser when the model ignores it.
pub const EXPLORE_RESPONSE_CONTRACT: &str = "\nExplore response contract (advisory): end your reply with one JSON object and no prose after it:\n{\"goal\":\"...\",\"constraints\":[\"...\"],\"findings\":[{\"summary\":\"...\",\"source\":\"path\",\"severity\":\"info|warning|critical\"}],\"files\":[\"path\"],\"symbols\":[\"name\"]}\n";

/// The dynamic context prepared for an ordinary user message.
///
/// `snapshot_id` is a deterministic identity of the *repository* snapshot
/// (everything in `dynamic_context` except the current user message). It is
/// what the bridge uses to deduplicate across turns; the metadata fields are
/// conservative estimates for presentation only, never provider billing.
#[derive(Debug, Clone)]
pub struct LeadContext {
    pub session_id: String,
    pub task_id: String,
    pub dynamic_context: String,
    pub snapshot_id: String,
    /// Estimated tokens for the injected dynamic context
    /// (`dynamic_context.len() / 4`). Clearly an estimate; never presented as
    /// exact provider tokens.
    pub estimated_tokens: usize,
    /// Byte length of the injected dynamic context.
    pub bytes: usize,
    /// Number of relevant files in the prepared input.
    pub file_count: usize,
    /// Number of relevant symbols in the prepared input.
    pub symbol_count: usize,
    pub goal: Option<String>,
    pub metrics: OrchestrationMetrics,
}

/// A role hand-off projected for a delegation.
#[derive(Debug, Clone)]
pub struct HandoffOutcome {
    pub session_id: String,
    pub task_id: String,
    pub source: Role,
    pub destination: Role,
    pub agent: Option<String>,
    pub capsule: ModelHandoffCapsule,
    pub dynamic_context: String,
    /// A conservative, explicitly advisory capability view. This is never
    /// applied to OpenCode's agent permissions in this version.
    pub advisory_permissions: Value,
    pub stale: bool,
    pub stale_reasons: Vec<String>,
    pub metrics: OrchestrationMetrics,
}

/// The bounded result of consuming an Explore output.
#[derive(Debug, Clone)]
pub struct ExploreDigest {
    pub session_id: String,
    pub task_id: String,
    pub structured: bool,
    pub findings: Vec<HandoffFinding>,
    pub locations: Vec<crate::verification::distill::SourceLocation>,
    pub checkpoint_id: Option<String>,
    pub metrics: OrchestrationMetrics,
}

/// What the controller decided after Build verification.
#[derive(Debug, Clone)]
pub enum BuildDecision {
    /// Verification passed. No Debug is recommended.
    Passed {
        report: VerificationReport,
        verification: HandoffVerification,
    },
    /// Verification failed and a bounded Build retry is allowed.
    RetryBuild {
        attempt: usize,
        report: VerificationReport,
        verification: HandoffVerification,
    },
    /// The Build retry budget is exhausted: Debug is recommended.
    Debug {
        reason: String,
        report: VerificationReport,
        handoff: Box<HandoffOutcome>,
    },
    /// No trusted verification command was configured; nothing was run.
    NotConfigured { note: String },
}

/// The full Build outcome, including the checkpoint, the Build→Verify hand-off
/// capsule and telemetry.
#[derive(Debug, Clone)]
pub struct BuildOutcome {
    pub session_id: String,
    pub task_id: String,
    pub stage: String,
    pub decision: BuildDecision,
    pub checkpoint_id: Option<String>,
    /// The typed Build→Verify projection. `None` only when no verification ran.
    pub verify_handoff: Option<ModelHandoffCapsule>,
    pub metrics: OrchestrationMetrics,
}

/// The controller for one project root.
pub struct Controller<'a> {
    root: PathBuf,
    config: OrchestrationConfig,
    context: ContextConfig,
    capabilities: CapabilityConfig,
    verification: VerificationConfig,
    git: &'a dyn GitHost,
    clock: &'a dyn Clock,
}

impl<'a> Controller<'a> {
    pub fn new(
        root: impl Into<PathBuf>,
        config: OrchestrationConfig,
        context: ContextConfig,
        capabilities: CapabilityConfig,
        verification: VerificationConfig,
        git: &'a dyn GitHost,
        clock: &'a dyn Clock,
    ) -> Self {
        Self {
            root: root.into(),
            config,
            context,
            capabilities,
            verification,
            git,
            clock,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn config(&self) -> &OrchestrationConfig {
        &self.config
    }

    pub fn verification(&self) -> &VerificationConfig {
        &self.verification
    }

    pub fn load_state(&self) -> state::LoadedState {
        state::load(&self.root)
    }

    /// The controller clock's current instant. Exposed so the bridge records a
    /// deterministic timestamp under a fixed clock in tests.
    pub fn now_unix(&self) -> i64 {
        self.clock.now_unix()
    }

    fn now(&self) -> i64 {
        self.clock.now_unix()
    }

    fn engine(&self) -> ContextEngine<'a> {
        ContextEngine::new(
            self.root.clone(),
            self.context.clone(),
            self.git,
            self.clock,
        )
        .with_capabilities(self.capabilities.clone())
        .with_verification(self.verification.clone())
    }

    fn context_enabled(&self) -> bool {
        self.config.enabled && self.context.enabled
    }

    /// A stable, non-reversible task id. Raw task text is never part of it.
    pub fn task_id(task: &str) -> String {
        crate::telemetry::Event::hashed_task_id(&format!("orchestration|{task}"))
    }

    /// The task representation persisted in state and checkpoints. It is bounded
    /// and passed through the existing secret detector: a secret-shaped task is
    /// redacted entirely, an ordinary task is stored verbatim (bounded). The
    /// in-memory prompt is still used for ranking; only the persisted copy is
    /// sanitized.
    pub fn stored_task_text(task: &str) -> String {
        let bounded: String = task.chars().take(4096).collect();
        crate::telemetry::task::redact(&bounded)
    }

    fn plan(&self, task: &str, role: Option<&str>) -> (Option<PlanOutcome>, Vec<String>) {
        if !self.context_enabled() {
            return (None, Vec::new());
        }
        match self.engine().plan(task, role) {
            Ok(outcome) => {
                let warnings = outcome.warnings.clone();
                (Some(outcome), warnings)
            }
            Err(error) => (
                None,
                vec![format!("context preparation was skipped: {error}")],
            ),
        }
    }

    /// Build the rich projection input from a plan plus bounded session state.
    fn build_input(
        &self,
        session: &SessionState,
        plan: Option<&PlanOutcome>,
        task: &str,
    ) -> ProjectionInput {
        let mut input = ProjectionInput {
            task: task.to_string(),
            ..ProjectionInput::default()
        };
        if let Some(outcome) = plan {
            let context_plan = &outcome.plan;
            if let Some(capsule) = &context_plan.capsule {
                input.goal = capsule.goal.clone();
                input.constraints = capsule.constraints.clone();
                input.findings = capsule
                    .findings
                    .iter()
                    .map(|finding| HandoffFinding {
                        summary: finding.summary.clone(),
                        detail: finding.detail.clone(),
                        source: finding.source.clone(),
                        severity: Severity::Info,
                    })
                    .collect();
                input.files = capsule.files.clone();
                input.symbols = capsule.symbols.clone();
                input.decisions = capsule
                    .decisions
                    .iter()
                    .map(|decision| decision.decision.clone())
                    .collect();
            }
            input.slices = context_plan.slices.clone();
            input.diff_context = render_diff_context(context_plan);
            input.diff_ref = diff_reference(&input.diff_context);
            input.git = context_plan.git.clone();
            input.provenance = context_plan.provenance.clone();
        }
        // Explicit session state always wins over derived plan state.
        if session.goal.is_some() {
            input.goal = session.goal.clone();
        }
        if !session.constraints.is_empty() {
            input.constraints = session.constraints.clone();
        }
        input.findings.extend(session.findings.clone());
        for path in &session.files {
            if !input.files.iter().any(|file| &file.path == path) {
                input.files.push(CapsuleFile {
                    path: path.clone(),
                    reason: Some("explore".to_string()),
                    changed: true,
                });
            }
        }
        for name in &session.symbols {
            if !input.symbols.iter().any(|symbol| &symbol.name == name) {
                input.symbols.push(crate::context::symbols::SymbolRef {
                    path: session
                        .files
                        .first()
                        .cloned()
                        .unwrap_or_else(|| "unknown".to_string()),
                    name: name.clone(),
                    kind: crate::context::symbols::SymbolKind::Reference,
                    start_line: 1,
                    end_line: 1,
                });
            }
        }
        input.failures = session.failures.clone();
        input.evidence = session.evidence.clone();
        input.verification = session.last_verification.clone();
        input.raw_log_refs = session
            .last_verification
            .as_ref()
            .map(|verification| verification.raw_log_refs.clone())
            .unwrap_or_default();
        input
    }

    /// `chat.message`: prepare bounded, deterministic dynamic context for the
    /// Lead on an ordinary user message.
    ///
    /// This is the *only* place the overall task is defined or reset. When the
    /// user message hashes to a different task id, prior findings, retry
    /// counters and checkpoint references are cleared; a repeated message keeps
    /// the running session. Delegated subagent prompts never reset it.
    pub fn prepare_lead_context(&self, session_id: &str, message: &str) -> Result<LeadContext> {
        let now = self.now();
        let session_key = state::safe_id(session_id);
        let task_id = Self::task_id(message);
        let mut loaded = state::load(&self.root);
        let existing = loaded.state.session(&session_key).cloned();
        let mut session = match existing {
            Some(session) if session.task_id == task_id => session,
            previous => {
                // A new task resets task-scoped state (findings, retries,
                // checkpoints), but the repository snapshot identity is
                // per-session: an unchanged snapshot must not be re-injected
                // just because the user phrased a follow-up differently.
                let last_snapshot_id = previous.and_then(|session| session.last_snapshot_id);
                let mut fresh = SessionState::new(&session_key, &task_id, now);
                fresh.task = Some(Self::stored_task_text(message));
                fresh.last_snapshot_id = last_snapshot_id;
                fresh
            }
        };
        session.session_id = session_key.clone();
        session.task_id = task_id.clone();
        session.phase = OrchestrationPhase::Idle;
        session.source = Role::Lead;
        session.destination = Some(Role::Lead);
        if session.task.is_none() {
            session.task = Some(Self::stored_task_text(message));
        }

        let (plan, warnings) = self.plan(message, Some("lead"));
        let input = self.build_input(&session, plan.as_ref(), message);
        let (input, omitted) = projection::sanitize(&input);
        let file_count = input.files.len();
        let symbol_count = input.symbols.len();
        // The repository snapshot excludes the current user message so the same
        // effective repository context yields the same identity across turns.
        // `render_lead_snapshot` renders everything except the `task:` line.
        let mut snapshot = self.render_lead_snapshot(&input);
        snapshot.push_str(&render_warnings(&warnings));
        snapshot.push_str(&render_warnings(&omitted));
        snapshot.push_str(&self.render_source_slices(&input.slices));
        let snapshot_id = snapshot_identity(&snapshot);
        let mut dynamic = self.render_lead_header(&input);
        dynamic.push_str(&snapshot);
        let bytes = dynamic.len();
        let estimated_tokens = bytes / 4;

        session.last_rich_bytes = input.rich_bytes();
        session.updated_at = now;
        loaded.state.upsert(session.clone(), now);
        let _ = state::save(&self.root, &loaded.state);

        let metrics = OrchestrationMetrics {
            phase: Some(OrchestrationPhase::Idle.as_str().to_string()),
            source: Some(Role::Lead.as_str().to_string()),
            destination: Some(Role::Lead.as_str().to_string()),
            rich_capsule_bytes: input.rich_bytes() as u64,
            handoff_capsule_bytes: 0,
            selected_source_bytes: input.selected_source_bytes() as u64,
            diff_context_bytes: input.diff_context_bytes() as u64,
            verification_context_bytes: input.verification_context_bytes() as u64,
            model_dynamic_context_bytes: dynamic.len() as u64,
            cache_hits: usize::from(
                plan.as_ref()
                    .map(|outcome| outcome.from_cache)
                    .unwrap_or(false),
            ),
            index_hits: plan
                .as_ref()
                .map(|outcome| outcome.index_report.metrics.reused)
                .unwrap_or(0),
            ..OrchestrationMetrics::default()
        };
        Ok(LeadContext {
            session_id: session_key,
            task_id,
            dynamic_context: dynamic,
            snapshot_id,
            estimated_tokens,
            bytes,
            file_count,
            symbol_count,
            goal: input.goal,
            metrics,
        })
    }

    /// `tool.execute.before` for `task`: project the typed role hand-off.
    ///
    /// The overall task identity comes from the session that `chat.message`
    /// established; the delegated prompt is used only for the context plan and
    /// the capsule's `task` field. A different delegated prompt must **not**
    /// reset findings or retry counters. A `Debug` delegation uses the narrow
    /// debug projection and never appends selected source.
    pub fn prepare_handoff(
        &self,
        session_id: &str,
        role: Role,
        task: &str,
    ) -> Result<HandoffOutcome> {
        let now = self.now();
        let session_key = state::safe_id(session_id);
        let mut loaded = state::load(&self.root);
        let delegated_task_id = Self::task_id(task);
        let mut session = match loaded.state.session(&session_key).cloned() {
            Some(session) => session,
            None => {
                let mut fresh = SessionState::new(&session_key, &delegated_task_id, now);
                fresh.task = Some(Self::stored_task_text(task));
                fresh
            }
        };
        if session.task_id.is_empty() {
            session.task_id = delegated_task_id;
        }
        if session.task.is_none() {
            session.task = Some(Self::stored_task_text(task));
        }
        let overall_task_id = session.task_id.clone();

        let previous = session.source;
        session.phase = phase_for(role);
        session.destination = Some(role);
        session.last_transition = Some(projection::transition_for(previous, role));

        let limits = ProjectionLimits::from_config(&self.config);
        let (stale, stale_reasons, _checkpoint_id) = self.checkpoint_freshness(&session);

        let rich_bytes;
        let verification_context_bytes;
        let mut selected_source_bytes = 0usize;
        let mut diff_context_bytes = 0usize;
        let mut cache_hits = 0usize;
        let mut index_hits = 0usize;
        let mut debug_attempt: Option<usize> = None;
        let mut capsule;
        let mut dynamic;
        let advisory;

        if role == Role::Debug {
            // Debug is a real budgeted transition. The first Debug delegation
            // consumes attempt 1; every delegation is counted, and once the
            // budget is exhausted the hand-off carries an explicit user
            // escalation instead of silently continuing.
            session.attempts.debug += 1;
            debug_attempt = Some(session.attempts.debug);
            // Narrow debug delegation: failures, evidence, verification and
            // raw-log references only. No repository plan, no source slices.
            let verification = session.last_verification.clone();
            let raw_log_refs = verification
                .as_ref()
                .map(|verification| verification.raw_log_refs.clone())
                .unwrap_or_default();
            let input = ProjectionInput {
                task: task.to_string(),
                failures: session.failures.clone(),
                evidence: session.evidence.clone(),
                diff_context: session.last_diff_context.clone(),
                diff_ref: diff_reference(&session.last_diff_context),
                raw_log_refs,
                verification,
                ..ProjectionInput::default()
            };
            let (clean, omitted) = projection::sanitize(&input);
            let rich_reference = session.last_rich_bytes.max(clean.rich_bytes());
            rich_bytes = rich_reference;
            verification_context_bytes = clean.verification_context_bytes();
            capsule = projection::project_with_rich(
                &clean,
                previous,
                role,
                &overall_task_id,
                &session_key,
                limits,
                rich_reference,
            );
            capsule.omitted.extend(omitted);
            if stale {
                capsule.omitted.push(format!(
                    "previous checkpoint is stale: {}",
                    stale_reasons.join("; ")
                ));
            }
            capsule.omitted.sort();
            capsule.omitted.dedup();
            advisory = json!({
                "advisory": true,
                "note": "debug delegation uses the narrow projection; OpenCode permissions remain authoritative",
            });
            dynamic = self.render_handoff_context(&capsule);
            dynamic.push_str(&render_advisory(&advisory));
            if let Some(escalation) = self.debug_escalation(session.attempts.debug) {
                capsule.omitted.push(escalation.clone());
                dynamic.push_str(&format!("\nescalation: {escalation}\n"));
            }
        } else {
            let (plan, warnings) = self.plan(task, Some(role.routing_role()));
            let input = self.build_input(&session, plan.as_ref(), task);
            let (input, omitted) = projection::sanitize(&input);
            rich_bytes = input.rich_bytes();
            selected_source_bytes = input.selected_source_bytes();
            diff_context_bytes = input.diff_context_bytes();
            verification_context_bytes = input.verification_context_bytes();
            cache_hits = usize::from(
                plan.as_ref()
                    .map(|outcome| outcome.from_cache)
                    .unwrap_or(false),
            );
            index_hits = plan
                .as_ref()
                .map(|outcome| outcome.index_report.metrics.reused)
                .unwrap_or(0);
            capsule = projection::project(
                &input,
                previous,
                role,
                &overall_task_id,
                &session_key,
                limits,
            );
            capsule.omitted.extend(omitted);
            if stale {
                capsule.omitted.push(format!(
                    "previous checkpoint is stale: {}",
                    stale_reasons.join("; ")
                ));
            }
            capsule.omitted.sort();
            capsule.omitted.dedup();

            advisory = self.advisory_permissions(task, &input);

            if session.goal.is_none() {
                session.goal = input.goal.clone();
            }
            if session.constraints.is_empty() {
                session.constraints = input.constraints.clone();
            }
            session.files = input
                .files
                .iter()
                .take(32)
                .map(|file| file.path.clone())
                .collect();
            session.symbols = input
                .symbols
                .iter()
                .take(32)
                .map(|symbol| symbol.name.clone())
                .collect();
            session.last_rich_bytes = input.rich_bytes();
            session.last_diff_context = input.diff_context.clone();

            dynamic = self.render_handoff_context(&capsule);
            dynamic.push_str(&render_warnings(&warnings));
            dynamic.push_str(&self.render_source_slices(&input.slices));
            if matches!(role, Role::Explore | Role::ExploreDeep) {
                dynamic.push_str(EXPLORE_RESPONSE_CONTRACT);
            }
            dynamic.push_str(&render_advisory(&advisory));
        }

        // Debug → Build is a real transition: when the previous owner was Debug
        // and the work returns to Build, checkpoint the hand-back with the
        // current session capsule and the last verification.
        if previous == Role::Debug && role == Role::Build {
            let checkpoint_id = self.save_checkpoint(
                Phase::DebugToBuild,
                self.session_capsule(&session),
                session.last_report.clone(),
            );
            if let Some(id) = &checkpoint_id {
                session.push_checkpoint(id);
            }
        }

        session.source = role;
        loaded.state.upsert(session.clone(), now);
        let _ = state::save(&self.root, &loaded.state);

        let mut metrics = OrchestrationMetrics {
            phase: Some(phase_for(role).as_str().to_string()),
            source: Some(previous.as_str().to_string()),
            destination: Some(role.as_str().to_string()),
            rich_capsule_bytes: rich_bytes as u64,
            handoff_capsule_bytes: capsule.measured_bytes() as u64,
            selected_source_bytes: selected_source_bytes as u64,
            diff_context_bytes: diff_context_bytes as u64,
            verification_context_bytes: verification_context_bytes as u64,
            model_dynamic_context_bytes: dynamic.len() as u64,
            cache_hits,
            index_hits,
            ..OrchestrationMetrics::default()
        };
        if let Some(attempt) = debug_attempt {
            metrics.attempt = Some(attempt);
            metrics.retry = Some(attempt.saturating_sub(1));
        }
        Ok(HandoffOutcome {
            session_id: session_key,
            task_id: overall_task_id,
            source: previous,
            destination: role,
            agent: role.agent(),
            capsule,
            dynamic_context: dynamic,
            advisory_permissions: advisory,
            stale,
            stale_reasons,
            metrics,
        })
    }

    /// `tool.execute.after` for an Explore task: consume the result into bounded
    /// typed findings and checkpoint Explore→Build.
    pub fn consume_explore_result(&self, session_id: &str, output: &str) -> Result<ExploreDigest> {
        let now = self.now();
        let session_key = state::safe_id(session_id);
        let mut loaded = state::load(&self.root);
        let mut session = loaded
            .state
            .session(&session_key)
            .cloned()
            .unwrap_or_else(|| SessionState::new(&session_key, "", now));
        let parsed = parse_explore_output(output);
        for finding in &parsed.findings {
            session.push_finding(finding.clone());
        }
        if let Some(goal) = &parsed.goal {
            session.goal = Some(goal.clone());
        }
        for constraint in &parsed.constraints {
            if !session.constraints.contains(constraint) {
                session.constraints.push(constraint.clone());
            }
        }
        for path in &parsed.files {
            if !session.files.contains(path) {
                session.files.push(path.clone());
            }
        }
        for symbol in &parsed.symbols {
            if !session.symbols.contains(symbol) {
                session.symbols.push(symbol.clone());
            }
        }
        if !parsed.locations.is_empty() {
            let text = parsed
                .locations
                .iter()
                .map(distill::SourceLocation::display)
                .collect::<Vec<_>>()
                .join(", ");
            if !session.evidence.contains(&text) {
                session.evidence.push(text);
            }
        }
        session.phase = OrchestrationPhase::Build;

        let capsule = self.session_capsule(&session);
        let checkpoint_id = self.save_checkpoint(Phase::ExploreToBuild, capsule, None);
        if let Some(id) = &checkpoint_id {
            session.push_checkpoint(id);
        }

        let task_id = session.task_id.clone();
        loaded.state.upsert(session.clone(), now);
        let _ = state::save(&self.root, &loaded.state);

        let metrics = OrchestrationMetrics {
            phase: Some(OrchestrationPhase::Build.as_str().to_string()),
            source: Some(Role::Explore.as_str().to_string()),
            destination: Some(Role::Build.as_str().to_string()),
            handoff_capsule_bytes: 0,
            verification_outcome: None,
            ..OrchestrationMetrics::default()
        };
        Ok(ExploreDigest {
            session_id: session_key,
            task_id,
            structured: parsed.structured,
            findings: parsed.findings,
            locations: parsed.locations,
            checkpoint_id,
            metrics,
        })
    }

    /// Run trusted configured verification after Build and apply the retry /
    /// Debug policy.
    pub fn after_build(
        &self,
        session_id: &str,
        runner: &dyn CaptureRunner,
        stage: Option<&str>,
    ) -> Result<BuildOutcome> {
        let now = self.now();
        let session_key = state::safe_id(session_id);
        let mut loaded = state::load(&self.root);
        let mut session = loaded
            .state
            .session(&session_key)
            .cloned()
            .unwrap_or_else(|| SessionState::new(&session_key, "", now));
        session.attempts.build += 1;
        let stage = stage
            .map(str::to_string)
            .unwrap_or_else(|| self.verification.default_stage.clone());

        if !self.verification.enabled || self.verification.command_count(&stage) == 0 {
            session.phase = OrchestrationPhase::Verify;
            let task_id = session.task_id.clone();
            let note = format!(
                "no trusted verification command is configured for stage '{stage}'; nothing was run"
            );
            loaded.state.upsert(session.clone(), now);
            let _ = state::save(&self.root, &loaded.state);
            return Ok(BuildOutcome {
                session_id: session_key,
                task_id,
                stage,
                decision: BuildDecision::NotConfigured { note },
                checkpoint_id: None,
                verify_handoff: None,
                metrics: OrchestrationMetrics {
                    phase: Some(OrchestrationPhase::Verify.as_str().to_string()),
                    source: Some(Role::Build.as_str().to_string()),
                    destination: Some(Role::Verify.as_str().to_string()),
                    attempt: Some(session.attempts.build),
                    ..OrchestrationMetrics::default()
                },
            });
        }

        let request = VerifyRequest {
            root: &self.root,
            config: &self.verification,
            stage: stage.clone(),
            runner,
            clock: self.clock,
            test_proposal: None,
        };
        let report = match execute(&request) {
            Ok(report) => report,
            Err(error) => {
                session.phase = OrchestrationPhase::Verify;
                let task_id = session.task_id.clone();
                let note = format!("verification could not run: {error}");
                loaded.state.upsert(session.clone(), now);
                let _ = state::save(&self.root, &loaded.state);
                return Ok(BuildOutcome {
                    session_id: session_key,
                    task_id,
                    stage,
                    decision: BuildDecision::NotConfigured { note },
                    checkpoint_id: None,
                    verify_handoff: None,
                    metrics: OrchestrationMetrics::default(),
                });
            }
        };

        let verification = handoff_verification(&report);
        session.last_verification = Some(verification.clone());
        session.last_report = Some(report.clone());
        // Refresh the context plan at the post-Build boundary so the Verify
        // hand-off reflects the current worktree (changed files, symbols, real
        // diff, freshness evidence) rather than the delegation-time plan.
        let session_task = session.task.clone().unwrap_or_default();
        let (fresh_plan, _fresh_warnings) = self.plan(&session_task, Some("verify"));
        if let Some(plan) = &fresh_plan {
            let fresh_diff = render_diff_context(&plan.plan);
            if !fresh_diff.is_empty() {
                session.last_diff_context = fresh_diff;
            }
        }
        let (verify_handoff, verify_rich_bytes) =
            self.verify_handoff_capsule(&session, &verification, fresh_plan.as_ref());
        record_verification_evidence(&mut session, &report);
        let capsule = self.session_capsule(&session);
        let checkpoint_id =
            self.save_checkpoint(Phase::BuildToVerify, capsule, Some(report.clone()));
        if let Some(id) = &checkpoint_id {
            session.push_checkpoint(id);
        }
        let task_id = session.task_id.clone();

        if report.passed() {
            session.attempts.verify += 1;
            session.phase = OrchestrationPhase::Done;
            loaded.state.upsert(session.clone(), now);
            let _ = state::save(&self.root, &loaded.state);
            return Ok(BuildOutcome {
                session_id: session_key,
                task_id,
                stage,
                decision: BuildDecision::Passed {
                    report,
                    verification,
                },
                checkpoint_id,
                verify_handoff: Some(verify_handoff.clone()),
                metrics: {
                    let mut metrics = orchestration_metrics(
                        OrchestrationPhase::Done,
                        Role::Build,
                        Role::Verify,
                        session.attempts,
                    );
                    metrics.rich_capsule_bytes = verify_rich_bytes as u64;
                    metrics.handoff_capsule_bytes = verify_handoff.measured_bytes() as u64;
                    metrics
                },
            });
        }

        session.attempts.verify += 1;
        if session.attempts.build <= self.config.max_build_retries {
            session.phase = OrchestrationPhase::Build;
            let attempt = session.attempts.build;
            loaded.state.upsert(session.clone(), now);
            let _ = state::save(&self.root, &loaded.state);
            return Ok(BuildOutcome {
                session_id: session_key,
                task_id,
                stage,
                decision: BuildDecision::RetryBuild {
                    attempt,
                    report,
                    verification,
                },
                checkpoint_id,
                verify_handoff: Some(verify_handoff.clone()),
                metrics: {
                    let mut metrics = orchestration_metrics(
                        OrchestrationPhase::Build,
                        Role::Build,
                        Role::Verify,
                        session.attempts,
                    );
                    metrics.rich_capsule_bytes = verify_rich_bytes as u64;
                    metrics.handoff_capsule_bytes = verify_handoff.measured_bytes() as u64;
                    metrics
                },
            });
        }

        // Budget exhausted: recommend Debug with an explainable reason.
        let reason = debug_reason(&stage, &session.attempts, &self.config, &report);
        session.debug_reason = Some(reason.clone());
        session.phase = OrchestrationPhase::Debug;
        let handoff = self.debug_handoff_from_session(&session, &session_key);
        let debug_checkpoint = self.save_checkpoint(
            Phase::VerifyToDebug,
            self.session_capsule(&session),
            Some(report.clone()),
        );
        if let Some(id) = &debug_checkpoint {
            session.push_checkpoint(id);
        }
        loaded.state.upsert(session.clone(), now);
        let _ = state::save(&self.root, &loaded.state);
        let mut metrics = orchestration_metrics(
            OrchestrationPhase::Debug,
            Role::Verify,
            Role::Debug,
            session.attempts,
        );
        metrics.rich_capsule_bytes = verify_rich_bytes as u64;
        metrics.handoff_capsule_bytes = verify_handoff.measured_bytes() as u64;
        metrics.debug_reason = Some(reason.clone());
        Ok(BuildOutcome {
            session_id: session_key,
            task_id,
            stage,
            decision: BuildDecision::Debug {
                reason,
                report,
                handoff: Box::new(handoff),
            },
            checkpoint_id: debug_checkpoint.or(checkpoint_id),
            verify_handoff: Some(verify_handoff.clone()),
            metrics,
        })
    }

    /// The user-escalation instruction once the Debug retry budget is exceeded.
    /// Shared by the public `debug_handoff` and the `prepare_handoff(Debug)`
    /// path so the policy cannot diverge.
    fn debug_escalation(&self, attempts: usize) -> Option<String> {
        if attempts > self.config.max_debug_retries {
            Some(format!(
                "user escalation required: debug retry budget exhausted ({attempts} attempt(s), max {}); ask the user for an explicit decision instead of continuing automatically",
                self.config.max_debug_retries
            ))
        } else {
            None
        }
    }

    /// Produce a Debug hand-off from the current session state. Delegates to the
    /// same builder and budget policy as `prepare_handoff(Role::Debug)`; kept as
    /// a public entry point for callers and tests.
    pub fn debug_handoff(&self, session_id: &str) -> Result<HandoffOutcome> {
        let now = self.now();
        let session_key = state::safe_id(session_id);
        let mut loaded = state::load(&self.root);
        let mut session = loaded
            .state
            .session(&session_key)
            .cloned()
            .unwrap_or_else(|| SessionState::new(&session_key, "", now));
        session.attempts.debug += 1;
        session.phase = OrchestrationPhase::Debug;
        let mut handoff = self.debug_handoff_from_session(&session, &session_key);
        if let Some(escalation) = self.debug_escalation(session.attempts.debug) {
            handoff.capsule.omitted.push(escalation.clone());
            handoff.stale_reasons.push(escalation.clone());
            handoff
                .dynamic_context
                .push_str(&format!("\nescalation: {escalation}\n"));
        }
        handoff.metrics.attempt = Some(session.attempts.debug);
        handoff.metrics.retry = Some(session.attempts.debug.saturating_sub(1));
        loaded.state.upsert(session.clone(), now);
        let _ = state::save(&self.root, &loaded.state);
        Ok(handoff)
    }

    fn debug_handoff_from_session(
        &self,
        session: &SessionState,
        session_key: &str,
    ) -> HandoffOutcome {
        let task = session.task.clone().unwrap_or_default();
        let limits = ProjectionLimits::from_config(&self.config);
        let verification = session.last_verification.clone();
        let raw_log_refs = verification
            .as_ref()
            .map(|verification| verification.raw_log_refs.clone())
            .unwrap_or_default();
        let input = ProjectionInput {
            task: task.clone(),
            failures: session.failures.clone(),
            evidence: session.evidence.clone(),
            diff_context: session.last_diff_context.clone(),
            diff_ref: diff_reference(&session.last_diff_context),
            raw_log_refs,
            verification,
            git: crate::context::gitdiff::GitState::default(),
            ..ProjectionInput::default()
        };
        let rich_reference = session.last_rich_bytes.max(input.rich_bytes());
        let capsule = projection::project_with_rich(
            &input,
            Role::Verify,
            Role::Debug,
            &session.task_id,
            session_key,
            limits,
            rich_reference,
        );
        let dynamic = self.render_handoff_context(&capsule);
        let metrics = OrchestrationMetrics {
            phase: Some(OrchestrationPhase::Debug.as_str().to_string()),
            source: Some(Role::Verify.as_str().to_string()),
            destination: Some(Role::Debug.as_str().to_string()),
            rich_capsule_bytes: rich_reference as u64,
            handoff_capsule_bytes: capsule.measured_bytes() as u64,
            model_dynamic_context_bytes: dynamic.len() as u64,
            verification_context_bytes: capsule
                .verification
                .as_ref()
                .and_then(|verification| serde_json::to_vec(verification).ok())
                .map(|bytes| bytes.len() as u64)
                .unwrap_or(0),
            debug_reason: session.debug_reason.clone(),
            ..OrchestrationMetrics::default()
        };
        HandoffOutcome {
            session_id: session_key.to_string(),
            task_id: session.task_id.clone(),
            source: Role::Verify,
            destination: Role::Debug,
            agent: Role::Debug.agent(),
            capsule,
            dynamic_context: dynamic,
            advisory_permissions: json!({"advisory": true}),
            stale: false,
            stale_reasons: Vec::new(),
            metrics,
        }
    }

    /// Project the typed Build→Verify hand-off for a completed build. It carries
    /// the goal, changed files, symbols, the real bounded diff and the distilled
    /// verification block (including failing locations).
    ///
    /// `fresh_plan` is a context plan recomputed at the post-Build boundary, so
    /// the hand-off reflects the current worktree rather than the state at
    /// delegation time.
    fn verify_handoff_capsule(
        &self,
        session: &SessionState,
        verification: &HandoffVerification,
        fresh_plan: Option<&PlanOutcome>,
    ) -> (ModelHandoffCapsule, usize) {
        let fallback_path = session
            .files
            .first()
            .cloned()
            .unwrap_or_else(|| "worktree".to_string());

        // Prefer the fresh plan's actual changed files and symbols.
        let changed_paths: Vec<String> = fresh_plan
            .map(|plan| plan.plan.changed_paths.clone())
            .filter(|paths| !paths.is_empty())
            .unwrap_or_else(|| session.files.clone());
        let symbols: Vec<crate::context::symbols::SymbolRef> = fresh_plan
            .and_then(|plan| plan.plan.capsule.as_ref())
            .map(|capsule| capsule.symbols.clone())
            .filter(|symbols| !symbols.is_empty())
            .unwrap_or_else(|| {
                session
                    .symbols
                    .iter()
                    .map(|name| crate::context::symbols::SymbolRef {
                        path: fallback_path.clone(),
                        name: name.clone(),
                        kind: crate::context::symbols::SymbolKind::Reference,
                        start_line: 1,
                        end_line: 1,
                    })
                    .collect()
            });
        let diff_context = fresh_plan
            .map(|plan| render_diff_context(&plan.plan))
            .unwrap_or_else(|| session.last_diff_context.clone());
        let git = fresh_plan
            .map(|plan| plan.plan.git.clone())
            .unwrap_or_default();

        let input = ProjectionInput {
            task: session.task.clone().unwrap_or_default(),
            goal: session.goal.clone(),
            constraints: session.constraints.clone(),
            files: changed_paths
                .iter()
                .map(|path| CapsuleFile {
                    path: path.clone(),
                    reason: Some("changed".to_string()),
                    changed: true,
                })
                .collect(),
            symbols,
            verification: Some(verification.clone()),
            evidence: session.evidence.clone(),
            diff_context: diff_context.clone(),
            diff_ref: diff_reference(&diff_context),
            git,
            ..ProjectionInput::default()
        };
        let rich_reference = session.last_rich_bytes.max(input.rich_bytes());
        let limits = ProjectionLimits::from_config(&self.config);
        let capsule = projection::project_with_rich(
            &input,
            Role::Build,
            Role::Verify,
            &session.task_id,
            &session.session_id,
            limits,
            rich_reference,
        );
        (capsule, rich_reference)
    }

    fn save_checkpoint(
        &self,
        phase: Phase,
        capsule: TaskCapsule,
        verification: Option<VerificationReport>,
    ) -> Option<String> {
        let snapshot = GitSnapshot::collect(&self.root, self.git);
        let fingerprint = snapshot_fingerprint(&snapshot);
        let provenance = Provenance {
            engine_version: ENGINE_VERSION.to_string(),
            schema_version: SCHEMA_VERSION,
            repo_id: capsule.provenance.repo_id.clone(),
            git_head: snapshot.state.head.clone(),
            git_dirty: snapshot.state.dirty,
            generated_at: self.now(),
            sources: capsule.provenance.sources.clone(),
            stale: false,
            validated: true,
            notes: Vec::new(),
        };
        let checkpoint = Checkpoint::build(
            phase,
            capsule,
            snapshot.state.clone(),
            fingerprint,
            verification,
            provenance,
            Vec::new(),
            self.now(),
        );
        let id = checkpoint.id.clone();
        match checkpoint.save(&self.root) {
            Ok(_) => Some(id),
            Err(error) => {
                eprintln!("ocg: warning: checkpoint was not saved: {error}");
                None
            }
        }
    }

    fn checkpoint_freshness(&self, session: &SessionState) -> (bool, Vec<String>, Option<String>) {
        let Some(id) = session.checkpoints.last() else {
            return (false, Vec::new(), None);
        };
        match checkpoint::load(&self.root, id, self.git) {
            Ok(loaded) if loaded.staleness.stale => {
                (true, loaded.staleness.reasons, Some(id.clone()))
            }
            Ok(_) => (false, Vec::new(), Some(id.clone())),
            Err(_) => (
                false,
                vec![format!("checkpoint {id} could not be read; it was ignored")],
                None,
            ),
        }
    }

    fn advisory_permissions(&self, task: &str, input: &ProjectionInput) -> Value {
        let evidence = CapabilityEvidence {
            changed_paths: input
                .files
                .iter()
                .filter(|file| file.changed)
                .map(|file| file.path.clone())
                .collect(),
            explicit: Vec::new(),
        };
        let plan = CapabilityPlan::plan_config(
            task,
            &evidence,
            &self.capabilities.custom,
            self.capabilities.enabled,
        );
        let allowed: Vec<String> = plan
            .capabilities
            .iter()
            .map(|entry| entry.capability.name())
            .collect();
        let denied: Vec<String> = plan
            .denied
            .iter()
            .map(|capability| capability.name())
            .collect();
        json!({
            "advisory": true,
            "note": "capability narrowing is advisory in this version; OpenCode agent permissions remain authoritative",
            "allowed": allowed,
            "denied": denied,
        })
    }

    /// The volatile header of the injected Lead context: the engine banner and
    /// the current user message. These are deliberately **excluded** from
    /// [`Self::render_lead_snapshot`] so the repository snapshot identity does
    /// not depend on the current message.
    fn render_lead_header(&self, input: &ProjectionInput) -> String {
        format!(
            "ocg orchestration context ({}):\ntask: {}\n",
            ENGINE_VERSION, input.task
        )
    }

    /// The repository-derived body of the Lead context. The current user
    /// message is not rendered here; it lives in [`Self::render_lead_header`].
    fn render_lead_snapshot(&self, input: &ProjectionInput) -> String {
        let mut out = String::new();
        if let Some(goal) = &input.goal {
            out.push_str(&format!("goal: {goal}\n"));
        }
        if !input.constraints.is_empty() {
            out.push_str("hard constraints:\n");
            for constraint in &input.constraints {
                out.push_str(&format!("- {constraint}\n"));
            }
        }
        if !input.files.is_empty() {
            out.push_str("relevant files:\n");
            for file in &input.files {
                let reason = file.reason.as_deref().unwrap_or("");
                out.push_str(&format!(
                    "- {}{}\n",
                    file.path,
                    if reason.is_empty() {
                        String::new()
                    } else {
                        format!(" ({reason})")
                    }
                ));
            }
        }
        if !input.symbols.is_empty() {
            out.push_str("relevant symbols:\n");
            for symbol in &input.symbols {
                out.push_str(&format!(
                    "- {} ({}:{})\n",
                    symbol.name, symbol.path, symbol.start_line
                ));
            }
        }
        if !input.findings.is_empty() {
            out.push_str("findings:\n");
            for finding in &input.findings {
                let severity = severity_label(finding.severity);
                out.push_str(&format!("- [{severity}] {}", finding.summary));
                if let Some(source) = &finding.source {
                    out.push_str(&format!(" ({source})"));
                }
                out.push('\n');
            }
        }
        out.push_str(&render_verification(input.verification.as_ref()));
        out
    }

    fn render_handoff_context(&self, capsule: &ModelHandoffCapsule) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "ocg role handoff: {} -> {} ({})\n",
            capsule.source.as_str(),
            capsule.destination.as_str(),
            capsule.transition.as_str()
        ));
        out.push_str(&format!("task: {}\n", capsule.task));
        if let Some(goal) = &capsule.goal {
            out.push_str(&format!("goal: {goal}\n"));
        }
        if !capsule.hard_constraints.is_empty() {
            out.push_str("hard constraints:\n");
            for constraint in &capsule.hard_constraints {
                out.push_str(&format!("- {constraint}\n"));
            }
        }
        if !capsule.files.is_empty() {
            out.push_str("files:\n");
            for file in &capsule.files {
                out.push_str(&format!(
                    "- {}{}\n",
                    file.path,
                    if file.changed { " (changed)" } else { "" }
                ));
            }
        }
        if !capsule.symbols.is_empty() {
            out.push_str("symbols:\n");
            for symbol in &capsule.symbols {
                out.push_str(&format!(
                    "- {} ({}:{})\n",
                    symbol.name, symbol.path, symbol.start_line
                ));
            }
        }
        if !capsule.findings.is_empty() {
            out.push_str("findings:\n");
            for finding in &capsule.findings {
                out.push_str(&format!(
                    "- [{}] {}",
                    severity_label(finding.severity),
                    finding.summary
                ));
                if let Some(source) = &finding.source {
                    out.push_str(&format!(" ({source})"));
                }
                out.push('\n');
            }
        }
        if !capsule.failures.is_empty() {
            out.push_str("failures:\n");
            for failure in &capsule.failures {
                out.push_str(&format!("- {failure}\n"));
            }
        }
        if !capsule.evidence.is_empty() {
            out.push_str("evidence:\n");
            for evidence in &capsule.evidence {
                out.push_str(&format!("- {evidence}\n"));
            }
        }
        out.push_str(&render_verification(capsule.verification.as_ref()));
        if let Some(diff_ref) = &capsule.diff_ref {
            out.push_str(&format!("diff: {diff_ref}\n"));
        }
        if let Some(diff_context) = &capsule.diff_context {
            out.push_str("diff context:\n");
            out.push_str(diff_context);
        }
        if !capsule.raw_log_refs.is_empty() {
            out.push_str("raw logs:\n");
            for reference in &capsule.raw_log_refs {
                out.push_str(&format!("- {reference}\n"));
            }
        }
        if !capsule.omitted.is_empty() {
            out.push_str("projection notes:\n");
            for note in &capsule.omitted {
                out.push_str(&format!("- {note}\n"));
            }
        }
        out
    }

    fn render_source_slices(&self, slices: &[crate::context::ranking::ContextSlice]) -> String {
        if slices.is_empty() {
            return String::new();
        }
        let mut out = String::new();
        let total: usize = slices.iter().map(|slice| slice.bytes).sum();
        out.push_str(&format!(
            "selected source ({} slice(s), {total} bytes):\n",
            slices.len()
        ));
        for slice in slices {
            out.push_str(&format!(
                "--- {}:{}-{}\n{}\n",
                slice.path, slice.start_line, slice.end_line, slice.content
            ));
        }
        out
    }

    /// Build the checkpoint capsule for a session and fingerprint every
    /// non-sensitive source it names, so a later `checkpoint::staleness` check is
    /// meaningful rather than vacuous.
    fn session_capsule(&self, session: &SessionState) -> TaskCapsule {
        let mut capsule = TaskCapsule::new(session.task.clone().unwrap_or_default());
        capsule.goal = session.goal.clone();
        capsule.constraints = session.constraints.clone();
        capsule.findings = session
            .findings
            .iter()
            .map(|finding| CapsuleFinding {
                summary: finding.summary.clone(),
                detail: finding.detail.clone(),
                source: finding.source.clone(),
            })
            .collect();
        capsule.files = session
            .files
            .iter()
            .map(|path| CapsuleFile {
                path: path.clone(),
                reason: Some("explore".to_string()),
                changed: true,
            })
            .collect();
        capsule.failures = session.failures.clone();
        let mut sources: Vec<crate::context::freshness::SourceFingerprint> = session
            .files
            .iter()
            .filter(|path| !crate::context::classify::classify(Path::new(path)).sensitive)
            .filter_map(|path| {
                let bytes = std::fs::read(self.root.join(path)).ok()?;
                Some(crate::context::freshness::SourceFingerprint {
                    path: path.clone(),
                    fingerprint: format!("sha256:{}", crate::runtime::hash::sha256_hex(&bytes)),
                    size: bytes.len() as u64,
                })
            })
            .collect();
        sources.sort_by(|a, b| a.path.cmp(&b.path));
        sources.dedup_by(|a, b| a.path == b.path);
        capsule.provenance.sources = sources;
        capsule.provenance.repo_id = String::new();
        capsule.recompute_size();
        capsule
    }
}

/// A deterministic identity for a rendered repository snapshot.
///
/// It hashes exactly the bytes of the message-independent snapshot text, so no
/// timestamp, secret or current user message can enter it. The same effective
/// repository context always yields the same identity; a materially different
/// snapshot yields a different one.
fn snapshot_identity(snapshot: &str) -> String {
    format!(
        "sha256:{}",
        crate::runtime::hash::sha256_hex(snapshot.as_bytes())
    )
}

fn phase_for(role: Role) -> OrchestrationPhase {
    match role {
        Role::Lead => OrchestrationPhase::Idle,
        Role::Explore | Role::ExploreDeep => OrchestrationPhase::Explore,
        Role::Build => OrchestrationPhase::Build,
        Role::Verify => OrchestrationPhase::Verify,
        Role::Debug => OrchestrationPhase::Debug,
        Role::Docs => OrchestrationPhase::Done,
    }
}

fn severity_label(severity: Severity) -> &'static str {
    match severity {
        Severity::Info => "info",
        Severity::Warning => "warning",
        Severity::Critical => "critical",
    }
}

fn render_verification(verification: Option<&HandoffVerification>) -> String {
    let Some(verification) = verification else {
        return String::new();
    };
    let mut out = format!(
        "verification: stage '{}' {}\n",
        verification.stage, verification.outcome
    );
    if !verification.failed_commands.is_empty() {
        out.push_str("failed commands:\n");
        for command in &verification.failed_commands {
            out.push_str(&format!("- {command}\n"));
        }
    }
    if !verification.failed_tests.is_empty() {
        out.push_str("failed tests:\n");
        for test in &verification.failed_tests {
            out.push_str(&format!("- {test}\n"));
        }
    }
    if !verification.locations.is_empty() {
        out.push_str("failing locations:\n");
        for location in &verification.locations {
            out.push_str(&format!("- {}\n", location.display()));
        }
    }
    if !verification.raw_log_refs.is_empty() {
        out.push_str("raw logs:\n");
        for reference in &verification.raw_log_refs {
            out.push_str(&format!("- {reference}\n"));
        }
    }
    out
}

fn render_warnings(warnings: &[String]) -> String {
    if warnings.is_empty() {
        return String::new();
    }
    let mut out = String::from("warnings:\n");
    for warning in warnings {
        out.push_str(&format!("- {warning}\n"));
    }
    out
}

fn render_advisory(advisory: &Value) -> String {
    let allowed = advisory
        .get("allowed")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    let denied = advisory
        .get("denied")
        .and_then(Value::as_array)
        .map(|values| values.len())
        .unwrap_or(0);
    format!(
        "capabilities (advisory; OpenCode permissions remain authoritative): allowed=[{allowed}] denied={denied}\n"
    )
}

fn orchestration_metrics(
    phase: OrchestrationPhase,
    source: Role,
    destination: Role,
    attempts: Attempts,
) -> OrchestrationMetrics {
    OrchestrationMetrics {
        phase: Some(phase.as_str().to_string()),
        source: Some(source.as_str().to_string()),
        destination: Some(destination.as_str().to_string()),
        attempt: Some(attempts.build.max(attempts.debug).max(1)),
        retry: Some(
            attempts
                .build
                .saturating_sub(1)
                .max(attempts.debug.saturating_sub(1)),
        ),
        ..OrchestrationMetrics::default()
    }
}

/// Build the compact verification block from a report. Public so the bridge can
/// render Debug feedback without duplicating the mapping.
pub fn handoff_verification(report: &VerificationReport) -> HandoffVerification {
    let mut failed_commands = Vec::new();
    let mut failed_tests = Vec::new();
    let mut locations = Vec::new();
    let mut raw_log_refs = Vec::new();
    let mut distilled = Vec::new();
    for result in &report.results {
        if !result.success {
            failed_commands.push(result.display());
        }
        for test in &result.failed_tests {
            if !failed_tests.contains(test) {
                failed_tests.push(test.clone());
            }
        }
        for location in &result.source_locations {
            if !locations.contains(location) {
                locations.push(location.clone());
            }
        }
        if let Some(reference) = &result.raw_log {
            if !raw_log_refs.contains(reference) {
                raw_log_refs.push(reference.clone());
            }
        }
        for line in result.output.summary.iter().take(4) {
            if !distilled.contains(line) {
                distilled.push(line.clone());
            }
        }
    }
    HandoffVerification {
        stage: report.stage.clone(),
        outcome: report.overall().as_str().to_string(),
        failed_commands,
        failed_tests,
        locations,
        raw_log_refs,
        distilled,
    }
}

fn record_verification_evidence(session: &mut SessionState, report: &VerificationReport) {
    let summary = format!(
        "verification stage '{}': {} ({} attempt(s))",
        report.stage,
        report.overall().as_str(),
        report.results.len()
    );
    if !session.evidence.contains(&summary) {
        session.evidence.push(summary);
    }
    for result in &report.results {
        if result.success {
            continue;
        }
        let failure = format!("{} ({})", result.display(), result.exit.label());
        if !session.failures.contains(&failure) {
            session.failures.push(failure);
        }
        for test in &result.failed_tests {
            let failure = format!("test failed: {test}");
            if !session.failures.contains(&failure) {
                session.failures.push(failure);
            }
        }
        for location in &result.source_locations {
            let failure = format!("failing at {}", location.display());
            if !session.failures.contains(&failure) {
                session.failures.push(failure);
            }
        }
    }
    if session.failures.len() > 64 {
        let excess = session.failures.len() - 64;
        session.failures.drain(0..excess);
    }
    if session.evidence.len() > 64 {
        let excess = session.evidence.len() - 64;
        session.evidence.drain(0..excess);
    }
}

/// An explainable, privacy-safe Debug reason. It deliberately contains **no**
/// configured command string and no raw output: only the stage, the outcome,
/// attempt/retry counts, the number of failing commands and the first distilled
/// source location.
fn debug_reason(
    stage: &str,
    attempts: &Attempts,
    config: &OrchestrationConfig,
    report: &VerificationReport,
) -> String {
    let failed = report
        .results
        .iter()
        .filter(|result| !result.success)
        .count();
    let location = report
        .results
        .iter()
        .flat_map(|result| result.source_locations.iter())
        .next()
        .map(distill::SourceLocation::display)
        .unwrap_or_else(|| "no location in the distilled output".to_string());
    format!(
        "verification stage '{stage}' {} after {n} build attempt(s) (retry budget {}); {failed} failing command(s); first location: {location}",
        report.overall().as_str(),
        config.max_build_retries,
        n = attempts.build,
    )
}

/// The deterministic result of parsing an Explore output.
#[derive(Debug, Clone, Default)]
struct ParsedExplore {
    structured: bool,
    goal: Option<String>,
    constraints: Vec<String>,
    findings: Vec<HandoffFinding>,
    files: Vec<String>,
    symbols: Vec<String>,
    locations: Vec<distill::SourceLocation>,
}

/// Parse an Explore output. A structured JSON object is honored when present;
/// otherwise a safe, deterministic line-based fallback is used. Never invents a
/// conclusion and never keeps secret-shaped content.
fn parse_explore_output(output: &str) -> ParsedExplore {
    if let Some(value) = extract_json(output) {
        if let Some(parsed) = parse_structured(&value) {
            return parsed;
        }
    }
    parse_fallback(output)
}

fn extract_json(output: &str) -> Option<Value> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        return Some(value);
    }
    let start = trimmed.find('{')?;
    let end = trimmed.rfind('}')?;
    if end <= start {
        return None;
    }
    serde_json::from_str::<Value>(&trimmed[start..=end]).ok()
}

fn parse_structured(value: &Value) -> Option<ParsedExplore> {
    let object = value.as_object()?;
    let has_shape = object.contains_key("findings")
        || object.contains_key("goal")
        || object.contains_key("constraints")
        || object.contains_key("files")
        || object.contains_key("symbols");
    if !has_shape {
        return None;
    }
    let mut parsed = ParsedExplore {
        structured: true,
        ..ParsedExplore::default()
    };
    if let Some(goal) = object.get("goal").and_then(Value::as_str) {
        let goal = goal.trim();
        if !goal.is_empty() && !crate::telemetry::task::is_secret_like(goal) {
            parsed.goal = Some(goal.to_string());
        }
    }
    for constraint in object
        .get("constraints")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(text) = constraint.as_str() {
            let text = text.trim();
            if !text.is_empty() && !crate::telemetry::task::is_secret_like(text) {
                parsed.constraints.push(text.to_string());
            }
        }
    }
    for finding in object
        .get("findings")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let summary = finding
            .get("summary")
            .and_then(Value::as_str)
            .or_else(|| finding.as_str())
            .unwrap_or("")
            .trim();
        if summary.is_empty() || crate::telemetry::task::is_secret_like(summary) {
            continue;
        }
        let detail = finding
            .get("detail")
            .and_then(Value::as_str)
            .map(str::to_string);
        let source = finding
            .get("source")
            .and_then(Value::as_str)
            .map(str::to_string);
        if source
            .as_deref()
            .map(|source| crate::context::classify::classify(Path::new(source)).sensitive)
            .unwrap_or(false)
        {
            continue;
        }
        let severity = finding
            .get("severity")
            .and_then(Value::as_str)
            .map(parse_severity)
            .unwrap_or(Severity::Info);
        parsed.findings.push(HandoffFinding {
            summary: truncate_text(summary, 300),
            detail,
            source,
            severity,
        });
    }
    for path in object
        .get("files")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(path) = path.as_str() {
            if !crate::context::classify::classify(Path::new(path)).sensitive {
                parsed.files.push(path.to_string());
            }
        }
    }
    for symbol in object
        .get("symbols")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let name = match symbol {
            Value::String(name) => Some(name.as_str()),
            Value::Object(map) => map.get("name").and_then(Value::as_str),
            _ => None,
        };
        if let Some(name) = name {
            if !name.trim().is_empty() && !crate::telemetry::task::is_secret_like(name) {
                parsed.symbols.push(name.to_string());
            }
        }
    }
    if parsed.findings.is_empty() && parsed.goal.is_none() && parsed.files.is_empty() {
        // A structured object with only symbols is still useful, so keep it.
        parsed.structured = !parsed.symbols.is_empty();
    }
    Some(parsed)
}

fn parse_severity(text: &str) -> Severity {
    match text.trim().to_ascii_lowercase().as_str() {
        "critical" | "error" | "fatal" => Severity::Critical,
        "warning" | "warn" => Severity::Warning,
        _ => Severity::Info,
    }
}

fn parse_fallback(output: &str) -> ParsedExplore {
    let mut parsed = ParsedExplore::default();
    for line in output.lines() {
        if parsed.findings.len() >= 12 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() || crate::telemetry::task::is_secret_like(trimmed) {
            continue;
        }
        let lower = trimmed.to_ascii_lowercase();
        let severity = if lower.contains("error")
            || lower.contains("failed")
            || lower.contains("panic")
            || lower.contains("fatal")
        {
            Severity::Critical
        } else if lower.contains("warn") {
            Severity::Warning
        } else {
            Severity::Info
        };
        if let Some(location) = distill::parse_location(trimmed) {
            if !crate::context::classify::classify(Path::new(&location.path)).sensitive
                && !parsed.locations.contains(&location)
            {
                parsed.locations.push(location);
            }
        }
        parsed.findings.push(HandoffFinding {
            summary: truncate_text(trimmed, 200),
            detail: None,
            source: None,
            severity,
        });
    }
    parsed
}

fn truncate_text(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push('…');
    out
}

/// The maximum bytes of real diff text carried in a hand-off. Bounded and
/// deterministic; the full diff stays in the context plan's own artifact.
pub const MAX_DIFF_CONTEXT_BYTES: usize = 8192;

/// Render a bounded, real diff block from the plan's [`DiffSummary`]. It names
/// the changed paths, keeps the actual hunks the context engine retained, drops
/// secret-shaped lines and sensitive paths, and states structural truncation.
pub fn render_diff_context(plan: &crate::context::ranking::ContextPlan) -> String {
    let mut out = String::new();
    let safe_paths: Vec<&String> = plan
        .changed_paths
        .iter()
        .filter(|path| !crate::context::classify::classify(Path::new(path)).sensitive)
        .collect();
    out.push_str(&format!("changed paths ({}):\n", safe_paths.len()));
    for path in safe_paths {
        out.push_str(&format!("- {path}\n"));
    }
    if let Some(diff) = &plan.diff {
        let block = render_bounded_diff(diff);
        if !block.is_empty() {
            out.push_str(&block);
        }
    }
    if plan.git.dirty {
        out.push_str("git: dirty\n");
    }
    if let Some(head) = &plan.git.head {
        out.push_str(&format!("git head: {head}\n"));
    }
    out
}

/// A bounded, deterministic rendering of the retained diff. Only files and
/// hunks the context engine actually kept are shown; nothing is invented.
pub fn render_bounded_diff(diff: &crate::context::gitdiff::DiffSummary) -> String {
    let mut out = String::new();
    let mut truncated = false;
    for entry in &diff.entries {
        if crate::context::classify::classify(Path::new(&entry.path)).sensitive {
            continue;
        }
        if out.len() >= MAX_DIFF_CONTEXT_BYTES {
            truncated = true;
            break;
        }
        out.push_str(&format!(
            "diff {} ({})\n",
            entry.path,
            entry.status.as_str()
        ));
        for hunk in diff.hunks.iter().filter(|hunk| hunk.path == entry.path) {
            if out.len() >= MAX_DIFF_CONTEXT_BYTES {
                truncated = true;
                break;
            }
            out.push_str(&format!("  {}\n", hunk.header));
            for line in &hunk.body {
                if crate::telemetry::task::is_secret_like(line) {
                    continue;
                }
                if out.len() >= MAX_DIFF_CONTEXT_BYTES {
                    truncated = true;
                    break;
                }
                out.push_str(&format!("  {line}\n"));
            }
        }
    }
    if let Some(structural) = &diff.structural {
        out.push_str(&format!(
            "diff structural: {} file(s), {} retained hunk(s), truncated={}\n",
            structural.files_changed, structural.hunks, structural.truncated
        ));
    }
    if truncated || diff.truncated || diff.capture_truncated {
        out.push_str("diff context truncated by the configured limits\n");
    }
    out
}

/// A content fingerprint of a rendered diff block, used as a real (not
/// fabricated) hand-off diff reference. `None` for an empty block.
pub fn diff_reference(block: &str) -> Option<String> {
    if block.trim().is_empty() {
        return None;
    }
    Some(format!(
        "sha256:{}",
        crate::runtime::hash::sha256_hex(block.as_bytes())
    ))
}

/// A public summary of an existing hand-off for diagnostics.
pub fn capsule_summary(capsule: &ModelHandoffCapsule) -> String {
    format!(
        "{} {} -> {} ({} bytes)",
        capsule.transition.as_str(),
        capsule.source.as_str(),
        capsule.destination.as_str(),
        capsule.measured_bytes()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_ids_are_deterministic_and_never_raw() {
        let id = Controller::task_id("fix the parser");
        assert!(id.starts_with("task-"));
        assert_eq!(id, Controller::task_id("fix the parser"));
        assert_ne!(id, Controller::task_id("fix the lexer"));
        assert!(!id.contains("parser"));
    }

    #[test]
    fn structured_explore_json_is_honored() {
        let output = r#"{
            "goal": "make the parser safe",
            "constraints": ["never break the API"],
            "findings": [
                {"summary": "parse_1 is recursive", "severity": "critical", "source": "src/module_1.rs"}
            ],
            "files": ["src/module_1.rs"],
            "symbols": [{"name": "parse_1"}]
        }"#;
        let parsed = parse_explore_output(output);
        assert!(parsed.structured);
        assert_eq!(parsed.goal.as_deref(), Some("make the parser safe"));
        assert_eq!(parsed.constraints, vec!["never break the API".to_string()]);
        assert_eq!(parsed.findings[0].severity, Severity::Critical);
        assert_eq!(parsed.files, vec!["src/module_1.rs".to_string()]);
        assert_eq!(parsed.symbols, vec!["parse_1".to_string()]);
    }

    #[test]
    fn fallback_explore_is_deterministic_and_bounded() {
        let mut output = String::new();
        output.push_str("line 0\nline 1\nline 2\n");
        output.push_str("error: boom at src/module_1.rs:4:5\n");
        for index in 3..40 {
            output.push_str(&format!("line {index}\n"));
        }
        let parsed = parse_explore_output(&output);
        assert!(!parsed.structured);
        assert_eq!(parsed.findings.len(), 12);
        assert_eq!(parsed.findings[0].summary, "line 0");
        assert!(parsed
            .locations
            .iter()
            .any(|location| location.display() == "src/module_1.rs:4:5"));
    }

    #[test]
    fn secret_shaped_explore_content_is_dropped() {
        let secret = format!("{}{}", concat!("sk", "-"), "A".repeat(40));
        let output =
            format!("{{\"goal\": \"{secret}\", \"findings\": [{{\"summary\": \"{secret}\"}}]}}");
        let parsed = parse_explore_output(&output);
        assert!(parsed.goal.is_none());
        assert!(parsed.findings.is_empty());
    }

    #[test]
    fn diff_context_is_stable() {
        let plan = crate::context::ranking::ContextPlan {
            schema_version: SCHEMA_VERSION,
            engine_version: ENGINE_VERSION.to_string(),
            task: "t".to_string(),
            role: None,
            repo_id: "r".to_string(),
            root: ".".to_string(),
            git: crate::context::gitdiff::GitState {
                head: Some("abc".to_string()),
                dirty: true,
                ..Default::default()
            },
            sections: Vec::new(),
            candidates: Vec::new(),
            selected_files: Vec::new(),
            changed_paths: vec!["a.rs".to_string(), "b.rs".to_string()],
            diff: None,
            slices: Vec::new(),
            candidate_bytes: 0,
            selected_bytes: 0,
            estimated_tokens: 0,
            limits: crate::context::ranking::ContextLimits {
                max_candidates: 1,
                max_files: 1,
                max_slices: 1,
                max_bytes: 1,
                max_diff_bytes: 1,
                max_hunks: 1,
                max_file_bytes: 1,
                max_symbols_per_file: 1,
                max_repository_files: 1,
            },
            provenance: Provenance::default(),
            sensitive_excluded: 0,
            truncated: false,
            notes: Vec::new(),
            instructions: Default::default(),
            policy: Default::default(),
            capabilities: Default::default(),
            capsule: None,
            test_proposal: None,
            verification: Default::default(),
        };
        let text = render_diff_context(&plan);
        assert!(text.contains("a.rs"));
        assert!(text.contains("git: dirty"));
    }

    #[test]
    fn snapshot_identity_is_deterministic_and_message_free() {
        let first = snapshot_identity("relevant files:\n- src/a.rs\n");
        let second = snapshot_identity("relevant files:\n- src/a.rs\n");
        let changed = snapshot_identity("relevant files:\n- src/b.rs\n");
        assert_eq!(first, second, "the same snapshot must hash identically");
        assert_ne!(first, changed, "a changed snapshot must hash differently");
        assert!(first.starts_with("sha256:"));
        assert_eq!(first.len(), "sha256:".len() + 64);
    }
}

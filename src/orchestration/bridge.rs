//! The hidden `ocg __bridge` surface.
//!
//! The generated JavaScript adapter spawns `ocg __bridge <event>` with a direct
//! argv and a JSON payload on stdin. This module is the *only* thing the
//! adapter can ask for. It is deliberately a thin, typed translation layer:
//!
//! - all decisions are delegated to [`crate::orchestration::controller`];
//! - errors are converted into `{ "ok": false }` and never panic or write raw
//!   content;
//! - telemetry is recorded here, with no prompt, source or log content.
//!
//! It is safe for tests to drive with fake payloads and a fake capture runner;
//! no model or network is involved.

use crate::orchestration::controller::{BuildDecision, Controller, HandoffOutcome, LeadContext};
use crate::orchestration::handoff::Role;
use crate::orchestration::state::{self, SessionState};
use crate::process::CaptureRunner;
use crate::telemetry::{self, Event, OrchestrationMetrics, Outcome, TelemetryConfig};
use serde_json::{json, Value};
use std::time::Instant;

/// One bridge request's result plus its telemetry accounting.
struct BridgeOutcome {
    value: Value,
    metrics: OrchestrationMetrics,
    outcome: Outcome,
    role: Option<String>,
    session_id: Option<String>,
    task_type: String,
}

/// The bridge context: the controller, the capture runner and the telemetry
/// policy. `runner` is the trusted verification runner; tests inject a fake.
pub struct BridgeContext<'a> {
    pub controller: &'a Controller<'a>,
    pub runner: &'a dyn CaptureRunner,
    pub telemetry: TelemetryConfig,
}

impl<'a> BridgeContext<'a> {
    pub fn new(
        controller: &'a Controller<'a>,
        runner: &'a dyn CaptureRunner,
        telemetry: TelemetryConfig,
    ) -> Self {
        Self {
            controller,
            runner,
            telemetry,
        }
    }

    /// Dispatch one event. Never fails: a bad payload or a controller error is
    /// reported as `{ "ok": false }`.
    pub fn dispatch(&self, event: &str, payload: &Value) -> Value {
        // A disabled policy is inert here too: no state, no telemetry, no
        // context. The CLI short-circuits earlier, but the bridge stays honest
        // when driven directly.
        if !self.controller.config().enabled {
            return json!({"ok": false, "disabled": true, "context": ""});
        }
        let started = Instant::now();
        let outcome = match event {
            "chat.message" | "chat-message" => self.chat_message(payload),
            "tool.execute.before" | "tool.execute.before/task" | "task-before" => {
                self.tool_before(payload)
            }
            "tool.execute.after" | "task-after" => self.tool_after(payload),
            other => BridgeOutcome {
                value: json!({"ok": false, "error": format!("unknown bridge event: {other}")}),
                metrics: OrchestrationMetrics::default(),
                outcome: Outcome::Unknown,
                role: None,
                session_id: None,
                task_type: "orchestration".to_string(),
            },
        };
        self.record(&outcome, started.elapsed().as_millis() as u64);
        outcome.value
    }

    fn chat_message(&self, payload: &Value) -> BridgeOutcome {
        let session_id = session_id(payload);
        let text = payload
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if text.trim().is_empty() {
            return BridgeOutcome {
                value: json!({"ok": false, "error": "empty chat.message payload"}),
                metrics: OrchestrationMetrics::default(),
                outcome: Outcome::Unknown,
                role: Some(Role::Lead.as_str().to_string()),
                session_id: Some(session_id),
                task_type: "orchestration".to_string(),
            };
        }
        match self.controller.prepare_lead_context(&session_id, &text) {
            Ok(context) => {
                let cached = self.snapshot_is_cached(&context);
                BridgeOutcome {
                    value: json!({
                        "ok": true,
                        "event": "chat.message",
                        "session_id": context.session_id,
                        "task_id": context.task_id,
                        // An unchanged repository snapshot is not appended again.
                        // The plugin treats an empty context as a no-op.
                        "context": if cached { String::new() } else { context.dynamic_context },
                        "snapshot_id": context.snapshot_id,
                        "cached": cached,
                        "estimated_tokens": context.estimated_tokens,
                        "bytes": context.bytes,
                        "file_count": context.file_count,
                        "symbol_count": context.symbol_count,
                    }),
                    metrics: context.metrics,
                    outcome: Outcome::Success,
                    role: Some(Role::Lead.as_str().to_string()),
                    session_id: Some(context.session_id),
                    task_type: "orchestration".to_string(),
                }
            }
            Err(error) => self.error_outcome(error.to_string(), Some(Role::Lead), Some(session_id)),
        }
    }

    /// Per-session snapshot deduplication.
    ///
    /// Returns `true` when the freshly prepared snapshot identity is already the
    /// session baseline, so the caller sends no context. Otherwise it records
    /// the new identity before returning `false`. The identity is persisted in
    /// the session state, so deduplication survives a plugin or process reload.
    fn snapshot_is_cached(&self, context: &LeadContext) -> bool {
        let mut loaded = self.controller.load_state();
        if loaded
            .state
            .session(&context.session_id)
            .and_then(|session| session.last_snapshot_id.as_deref())
            == Some(context.snapshot_id.as_str())
        {
            return true;
        }
        let now = self.controller.now_unix();
        let mut session = loaded
            .state
            .session(&context.session_id)
            .cloned()
            .unwrap_or_else(|| SessionState::new(&context.session_id, &context.task_id, now));
        session.last_snapshot_id = Some(context.snapshot_id.clone());
        loaded.state.upsert(session, now);
        if let Err(error) = state::save(self.controller.root(), &loaded.state) {
            eprintln!("ocg: warning: context snapshot state was not saved: {error}");
        }
        false
    }

    fn tool_before(&self, payload: &Value) -> BridgeOutcome {
        let session_id = session_id(payload);
        let args = payload.get("args").cloned().unwrap_or(Value::Null);
        let subagent = subagent(&args).unwrap_or_default();
        let Some(role) = Role::parse(&subagent) else {
            return BridgeOutcome {
                value: json!({
                    "ok": false,
                    "error": format!("unknown subagent role: {subagent}"),
                }),
                metrics: OrchestrationMetrics::default(),
                outcome: Outcome::Unknown,
                role: None,
                session_id: Some(session_id),
                task_type: "orchestration".to_string(),
            };
        };
        let task = task_text(&args);
        match self.controller.prepare_handoff(&session_id, role, &task) {
            Ok(handoff) => BridgeOutcome {
                value: handoff_value("tool.execute.before", &handoff),
                metrics: handoff.metrics.clone(),
                outcome: Outcome::Success,
                role: Some(role.as_str().to_string()),
                session_id: Some(handoff.session_id.clone()),
                task_type: "orchestration".to_string(),
            },
            Err(error) => self.error_outcome(error.to_string(), Some(role), Some(session_id)),
        }
    }

    fn tool_after(&self, payload: &Value) -> BridgeOutcome {
        let session_id = session_id(payload);
        let args = payload.get("args").cloned().unwrap_or(Value::Null);
        let subagent = subagent(&args).unwrap_or_default();
        let Some(role) = Role::parse(&subagent) else {
            return BridgeOutcome {
                value: json!({
                    "ok": false,
                    "error": format!("unknown subagent role: {subagent}"),
                }),
                metrics: OrchestrationMetrics::default(),
                outcome: Outcome::Unknown,
                role: None,
                session_id: Some(session_id),
                task_type: "orchestration".to_string(),
            };
        };
        match role {
            Role::Explore | Role::ExploreDeep => self.after_explore(&session_id, payload),
            Role::Build => self.after_build(&session_id),
            _ => BridgeOutcome {
                value: json!({
                    "ok": true,
                    "event": "tool.execute.after",
                    "context": "",
                    "note": format!("no orchestration action for {} completion", role.as_str()),
                }),
                metrics: OrchestrationMetrics::default(),
                outcome: Outcome::Unknown,
                role: Some(role.as_str().to_string()),
                session_id: Some(session_id),
                task_type: "orchestration".to_string(),
            },
        }
    }

    fn after_explore(&self, session_id: &str, payload: &Value) -> BridgeOutcome {
        let output = result_text(payload);
        match self.controller.consume_explore_result(session_id, &output) {
            Ok(digest) => {
                let context = format!(
                    "explore result captured ({}): {} finding(s), {} location(s){}",
                    if digest.structured {
                        "structured JSON"
                    } else {
                        "deterministic fallback"
                    },
                    digest.findings.len(),
                    digest.locations.len(),
                    digest
                        .checkpoint_id
                        .as_deref()
                        .map(|id| format!(", checkpoint {id}"))
                        .unwrap_or_default()
                );
                BridgeOutcome {
                    value: json!({
                        "ok": true,
                        "event": "tool.execute.after",
                        "session_id": digest.session_id,
                        "task_id": digest.task_id,
                        "context": context,
                        "findings": digest.findings.len(),
                        "checkpoint": digest.checkpoint_id,
                    }),
                    metrics: digest.metrics,
                    outcome: Outcome::Success,
                    role: Some(Role::Explore.as_str().to_string()),
                    session_id: Some(digest.session_id.clone()),
                    task_type: "orchestration".to_string(),
                }
            }
            Err(error) => self.error_outcome(
                error.to_string(),
                Some(Role::Explore),
                Some(session_id.to_string()),
            ),
        }
    }

    fn after_build(&self, session_id: &str) -> BridgeOutcome {
        match self.controller.after_build(session_id, self.runner, None) {
            Ok(outcome) => {
                let (context, telemetry_outcome) = build_feedback(&outcome.decision);
                BridgeOutcome {
                    value: json!({
                        "ok": true,
                        "event": "tool.execute.after",
                        "session_id": outcome.session_id,
                        "task_id": outcome.task_id,
                        "stage": outcome.stage,
                        "context": context,
                        "checkpoint": outcome.checkpoint_id,
                    }),
                    metrics: outcome.metrics,
                    outcome: telemetry_outcome,
                    role: Some(Role::Build.as_str().to_string()),
                    session_id: Some(outcome.session_id.clone()),
                    task_type: "orchestration".to_string(),
                }
            }
            Err(error) => self.error_outcome(
                error.to_string(),
                Some(Role::Build),
                Some(session_id.to_string()),
            ),
        }
    }

    fn error_outcome(
        &self,
        message: String,
        role: Option<Role>,
        session_id: Option<String>,
    ) -> BridgeOutcome {
        BridgeOutcome {
            value: json!({"ok": false, "error": safe_error(&message)}),
            metrics: OrchestrationMetrics::default(),
            outcome: Outcome::Unknown,
            role: role.map(|role| role.as_str().to_string()),
            session_id,
            task_type: "orchestration".to_string(),
        }
    }

    fn record(&self, outcome: &BridgeOutcome, duration_ms: u64) {
        let timestamp = self.controller.now_unix();
        let mut event = Event::new(
            crate::telemetry::Event::hashed_task_id(&format!(
                "orchestration|{}|{}",
                outcome.session_id.as_deref().unwrap_or(""),
                outcome.role.as_deref().unwrap_or("")
            )),
            timestamp,
        );
        event.session_id = outcome.session_id.clone();
        event.task_type = Some(outcome.task_type.clone());
        event.role = outcome.role.clone();
        event.duration_ms = duration_ms;
        event.orchestration = outcome.metrics.clone();
        event.outcome = outcome.outcome;
        for warning in telemetry::record(self.controller.root(), &self.telemetry, event) {
            eprintln!("ocg: warning: {warning}");
        }
    }
}

fn handoff_value(event: &str, handoff: &HandoffOutcome) -> Value {
    json!({
        "ok": true,
        "event": event,
        "session_id": handoff.session_id,
        "task_id": handoff.task_id,
        "source": handoff.source.as_str(),
        "destination": handoff.destination.as_str(),
        "agent": handoff.agent,
        "context": handoff.dynamic_context,
        "handoff_bytes": handoff.capsule.measured_bytes(),
        "stale": handoff.stale,
        "stale_reasons": handoff.stale_reasons,
        "advisory_permissions": handoff.advisory_permissions,
    })
}

fn build_feedback(decision: &BuildDecision) -> (String, Outcome) {
    let mut context = String::new();
    let outcome = match decision {
        BuildDecision::Passed { verification, .. } => {
            context.push_str(&format!(
                "ocg verification: stage '{}' passed; no Debug hand-off is recommended.\n",
                verification.stage
            ));
            Outcome::Success
        }
        BuildDecision::RetryBuild {
            attempt,
            verification,
            ..
        } => {
            context.push_str(&format!(
                "ocg verification: stage '{}' failed; bounded Build retry {attempt} is allowed.\n",
                verification.stage
            ));
            append_verification(&mut context, verification);
            Outcome::Failure
        }
        BuildDecision::Debug {
            reason,
            handoff,
            report,
        } => {
            context.push_str(&format!("ocg verification failed: {reason}\n"));
            context.push_str(&format!(
                "ocg recommends the Debug role (agent {}).\n",
                handoff.agent.as_deref().unwrap_or("ocg-debug")
            ));
            let verification = crate::orchestration::controller::handoff_verification(report);
            append_verification(&mut context, &verification);
            Outcome::Failure
        }
        BuildDecision::NotConfigured { note } => {
            context.push_str(&format!("ocg verification: {note}\n"));
            Outcome::Unknown
        }
    };
    (context, outcome)
}

fn append_verification(
    context: &mut String,
    verification: &crate::orchestration::handoff::HandoffVerification,
) {
    if !verification.failed_commands.is_empty() {
        context.push_str("failed commands:\n");
        for command in &verification.failed_commands {
            context.push_str(&format!("- {command}\n"));
        }
    }
    if !verification.failed_tests.is_empty() {
        context.push_str("failed tests:\n");
        for test in &verification.failed_tests {
            context.push_str(&format!("- {test}\n"));
        }
    }
    if !verification.locations.is_empty() {
        context.push_str("failing locations:\n");
        for location in &verification.locations {
            context.push_str(&format!("- {}\n", location.display()));
        }
    }
    if !verification.raw_log_refs.is_empty() {
        context.push_str("raw logs:\n");
        for reference in &verification.raw_log_refs {
            context.push_str(&format!("- {reference}\n"));
        }
    }
}

fn session_id(payload: &Value) -> String {
    for key in ["session_id", "sessionID", "sessionId"] {
        if let Some(value) = payload.get(key).and_then(Value::as_str) {
            if !value.is_empty() {
                return value.to_string();
            }
        }
    }
    "default".to_string()
}

fn subagent(args: &Value) -> Option<String> {
    for key in ["subagent_type", "subagentType", "agent", "subagent"] {
        if let Some(value) = args.get(key).and_then(Value::as_str) {
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn task_text(args: &Value) -> String {
    for key in ["prompt", "description"] {
        if let Some(value) = args.get(key).and_then(Value::as_str) {
            if !value.trim().is_empty() {
                return value.to_string();
            }
        }
    }
    String::new()
}

fn result_text(payload: &Value) -> String {
    if let Some(text) = payload.get("result").and_then(Value::as_str) {
        return text.to_string();
    }
    if let Some(result) = payload.get("result") {
        if let Some(text) = result.get("output").and_then(Value::as_str) {
            return text.to_string();
        }
    }
    if let Some(text) = payload.get("output").and_then(Value::as_str) {
        return text.to_string();
    }
    String::new()
}

fn safe_error(message: &str) -> String {
    if crate::telemetry::task::is_secret_like(message) {
        "orchestration bridge error (details withheld)".to_string()
    } else {
        message.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::CapabilityConfig;
    use crate::clock::FixedClock;
    use crate::context::ContextConfig;
    use crate::orchestration::config::OrchestrationConfig;
    use crate::process::{FakeCaptureRunner, FakeGitHost};
    use crate::verification::config::VerificationConfig;

    fn controller<'a>(
        root: &'a std::path::Path,
        git: &'a FakeGitHost,
        clock: &'a FixedClock,
        verification: VerificationConfig,
    ) -> Controller<'a> {
        Controller::new(
            root,
            OrchestrationConfig::default(),
            ContextConfig::default(),
            CapabilityConfig::default(),
            verification,
            git,
            clock,
        )
    }

    #[test]
    fn unknown_event_is_rejected_without_panicking() {
        let dir = tempfile::tempdir().unwrap();
        let git = FakeGitHost::new();
        let clock = FixedClock::new(1);
        let controller = controller(dir.path(), &git, &clock, VerificationConfig::default());
        let runner = FakeCaptureRunner::new();
        let bridge = BridgeContext::new(&controller, &runner, TelemetryConfig::disabled());
        let value = bridge.dispatch("nope", &json!({}));
        assert_eq!(value["ok"], json!(false));
    }

    #[test]
    fn unknown_subagent_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let git = FakeGitHost::new();
        let clock = FixedClock::new(1);
        let controller = controller(dir.path(), &git, &clock, VerificationConfig::default());
        let runner = FakeCaptureRunner::new();
        let bridge = BridgeContext::new(&controller, &runner, TelemetryConfig::disabled());
        let value = bridge.dispatch(
            "tool.execute.before",
            &json!({"session_id": "s", "args": {"subagent_type": "nonsense"}}),
        );
        assert_eq!(value["ok"], json!(false));
    }

    #[test]
    fn chat_message_returns_dynamic_context() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("parser.rs"), "pub fn parse() {}\n").unwrap();
        let git = FakeGitHost::new();
        let clock = FixedClock::new(1);
        let controller = controller(dir.path(), &git, &clock, VerificationConfig::default());
        let runner = FakeCaptureRunner::new();
        let bridge = BridgeContext::new(&controller, &runner, TelemetryConfig::disabled());
        let value = bridge.dispatch(
            "chat.message",
            &json!({"session_id": "s", "text": "fix the parser"}),
        );
        assert_eq!(value["ok"], json!(true));
        assert!(value["context"]
            .as_str()
            .unwrap()
            .contains("fix the parser"));
    }
}

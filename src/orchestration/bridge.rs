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

use crate::orchestration::context_governor::{ContextObservation, GovernorState};
use crate::orchestration::controller::{
    BuildDecision, ContextGovernanceResult, Controller, HandoffOutcome,
};
use crate::orchestration::handoff::Role;
use crate::process::CaptureRunner;
use crate::reports::ReportsConfig;
use crate::runtime::compat::{LeadSelection, RolloverRuntime};
use crate::telemetry::{self, Event, OrchestrationMetrics, Outcome, TelemetryConfig};
use serde_json::{json, Value};
use std::cell::RefCell;
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
    pub reports: ReportsConfig,
    /// Invocation-scoped runtime client. It is absent for ordinary bridge
    /// calls and for tests that only exercise policy projection.
    pub rollover_runtime: Option<RefCell<Box<dyn RolloverRuntime>>>,
    pub rollover_lead: Option<LeadSelection>,
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
            reports: ReportsConfig::default(),
            rollover_runtime: None,
            rollover_lead: None,
        }
    }

    /// Attach the invocation-owned V2 client used only by `context.observe`.
    /// The client is held inside this short-lived bridge value: a dropped
    /// OpenCode/UI connection cannot turn into a Mission failure because the
    /// Mission record is never stored in this object.
    pub fn with_rollover_runtime<R: RolloverRuntime + 'static>(
        mut self,
        runtime: R,
        lead: LeadSelection,
    ) -> Self {
        self.rollover_runtime = Some(RefCell::new(Box::new(runtime)));
        self.rollover_lead = Some(lead);
        self
    }

    /// Apply the report policy for this bridge.
    pub fn with_reports(mut self, reports: ReportsConfig) -> Self {
        self.reports = reports;
        self
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
            "session.prompt" => self.session_prompt(payload),
            "session.context" => self.session_context(payload),
            "context.observe" | "session.context.observe" | "context-observation" => {
                self.context_observe(payload)
            }
            "tool.execute.before" | "tool.execute.before/task" | "task-before" => {
                self.tool_before(payload)
            }
            "tool.execute.after" | "task-after" => self.tool_after(payload),
            "lead.output" | "lead-output" => self.lead_output(payload),
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
            Ok(context) => BridgeOutcome {
                value: json!({
                    "ok": true,
                    "event": "chat.message",
                    "session_id": context.session_id,
                    "task_id": context.task_id,
                    // V1 persisted-prompt path: an unchanged repository
                    // baseline already lives in the persisted history, so it
                    // is not appended again. The plugin treats an empty
                    // context as a no-op. (The V2 `session.context` path above
                    // always returns the full baseline instead.)
                    "context": if context.cached { String::new() } else { context.dynamic_context },
                    "snapshot_id": context.snapshot_id,
                    "cached": context.cached,
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
            },
            Err(error) => self.error_outcome(error.to_string(), Some(Role::Lead), Some(session_id)),
        }
    }

    /// `session.prompt` (OpenCode V2 prompt admission): a genuinely admitted
    /// user prompt. This is the *only* bridge event that may establish or
    /// reset the session's task identity. Runtime-generated synthetic
    /// user-role messages (interruption/resume continuations and similar)
    /// never pass through OpenCode's prompt admission, so they never reach
    /// this handler and can never reset task-scoped state.
    fn session_prompt(&self, payload: &Value) -> BridgeOutcome {
        let session_id = session_id(payload);
        let text = payload
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if text.trim().is_empty() {
            return BridgeOutcome {
                value: json!({"ok": false, "error": "empty session.prompt payload"}),
                metrics: OrchestrationMetrics::default(),
                outcome: Outcome::Unknown,
                role: Some(Role::Lead.as_str().to_string()),
                session_id: Some(session_id),
                task_type: "orchestration".to_string(),
            };
        }
        match self.controller.admit_user_task(&session_id, &text) {
            Ok(admission) => BridgeOutcome {
                value: json!({
                    "ok": true,
                    "event": "session.prompt",
                    "session_id": admission.session_id,
                    "task_id": admission.task_id,
                    "changed": admission.changed,
                }),
                metrics: OrchestrationMetrics::default(),
                outcome: Outcome::Success,
                role: Some(Role::Lead.as_str().to_string()),
                session_id: Some(admission.session_id),
                task_type: "orchestration".to_string(),
            },
            Err(error) => self.error_outcome(error.to_string(), Some(Role::Lead), Some(session_id)),
        }
    }

    /// `session.context` (OpenCode V2 model dispatch): supply the current
    /// session repository baseline for one root-Lead request. Unlike the V1
    /// `chat.message` path, the full baseline is returned on *every* dispatch —
    /// the adapter injects it into the outgoing request's system context,
    /// which is never persisted, so nothing accumulates in the conversation
    /// history. `cached` only reports that baseline computation was reused; it
    /// never suppresses inclusion.
    ///
    /// Dispatch-time conversation content is never a task signal here: task
    /// identity is owned by `session.prompt` admission. A tool-driven
    /// continuation or a synthetic user-role message must not reset
    /// task-scoped state, so a `text` field in the payload is ignored.
    fn session_context(&self, payload: &Value) -> BridgeOutcome {
        let session_id = session_id(payload);
        match self.controller.prepare_model_context(&session_id) {
            Ok(context) => BridgeOutcome {
                value: json!({
                    "ok": true,
                    "event": "session.context",
                    "session_id": context.session_id,
                    "task_id": context.task_id,
                    "context": context.dynamic_context,
                    "snapshot_id": context.snapshot_id,
                    "cached": context.cached,
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
            },
            Err(error) => self.error_outcome(error.to_string(), Some(Role::Lead), Some(session_id)),
        }
    }

    fn context_observe(&self, payload: &Value) -> BridgeOutcome {
        let session = session_id(payload);
        if session.is_empty() {
            return BridgeOutcome {
                value: json!({"ok": false, "error": "context.observe requires session_id"}),
                metrics: OrchestrationMetrics::default(),
                outcome: Outcome::Unknown,
                role: Some(Role::Lead.as_str().to_string()),
                session_id: None,
                task_type: "context".to_string(),
            };
        }
        let Some(agent) = payload.get("agent").and_then(Value::as_str) else {
            return BridgeOutcome {
                value: json!({
                    "ok": true,
                    "event": "context.observe",
                    "ignored": true,
                    "reason": "context pressure is observed only for a root Lead session",
                }),
                metrics: OrchestrationMetrics::default(),
                outcome: Outcome::Unknown,
                role: Some(Role::Lead.as_str().to_string()),
                session_id: Some(session),
                task_type: "context".to_string(),
            };
        };
        if !agent.starts_with("lead-") {
            return BridgeOutcome {
                value: json!({
                    "ok": true,
                    "event": "context.observe",
                    "ignored": true,
                    "reason": "context pressure is observed only for a root Lead session",
                }),
                metrics: OrchestrationMetrics::default(),
                outcome: Outcome::Unknown,
                role: Some(agent.to_string()),
                session_id: Some(session),
                task_type: "context".to_string(),
            };
        }
        if !self.controller.config().context_governor.enabled {
            let observation = ContextObservation {
                session_id: session.clone(),
                ..ContextObservation::default()
            };
            let decision = crate::orchestration::context_governor::GovernorDecision {
                state: GovernorState::Disabled,
                action: crate::orchestration::context_governor::GovernorAction::Continue,
                utilization_percent: None,
                rollover_allowed: false,
                deferred_for_boundary: false,
                reason: "context governor is disabled".to_string(),
            };
            return BridgeOutcome {
                value: context_governance_value(&observation, &decision, None),
                metrics: OrchestrationMetrics::default(),
                outcome: Outcome::Success,
                role: Some(Role::Lead.as_str().to_string()),
                session_id: Some(session),
                task_type: "context".to_string(),
            };
        }
        let now = self.controller.now_unix();
        let event_id = payload
            .get("event_id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| {
                crate::orchestration::context_governor::event_identity(
                    &session,
                    payload.get("assistant_message_id").and_then(Value::as_str),
                    payload.get("finish").and_then(Value::as_str),
                    payload.get("observed_at").and_then(Value::as_i64),
                )
            });
        let finish = payload
            .get("finish")
            .and_then(Value::as_str)
            .map(str::to_string);
        let assistant_message_id = payload
            .get("assistant_message_id")
            .and_then(Value::as_str)
            .map(str::to_string);
        // A step is a rollover boundary only after the event adapter has
        // durably handed its completed output to the bridge. A caller cannot
        // simply set a boolean in an arbitrary payload and skip that ordering.
        let safe_boundary = payload
            .get("safe_boundary")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            && payload
                .get("output_persisted")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            && finish.as_deref() == Some("stop");
        let step_tokens = payload
            .get("tokens")
            .or_else(|| payload.get("data").and_then(|data| data.get("tokens")))
            .cloned();

        let (observation, decision, rollover) = if let Some(runtime_cell) =
            self.rollover_runtime.as_ref()
        {
            let mut runtime_cell = runtime_cell.borrow_mut();
            let runtime: &mut dyn RolloverRuntime = runtime_cell.as_mut();
            match self.collect_v2_observation(
                runtime,
                &session,
                &event_id,
                now,
                finish.clone(),
                safe_boundary,
                assistant_message_id.clone(),
                step_tokens.as_ref(),
                payload,
            ) {
                Ok(observation) => match self.controller.observe_context(
                    &session,
                    observation.clone(),
                    runtime,
                    self.rollover_lead
                        .as_ref()
                        .expect("runtime implies a Lead contract"),
                ) {
                    Ok(result) => {
                        let decision = result.decision.clone();
                        (observation, decision, Some(result))
                    }
                    Err(error) => {
                        return self.error_outcome(
                            safe_error(&error.to_string()),
                            Some(Role::Lead),
                            Some(session),
                        )
                    }
                },
                Err(reason) => {
                    let observation =
                        ContextObservation::unknown(&session, &event_id, now, safe_error(&reason));
                    let decision = self
                        .controller
                        .evaluate_context(&session, observation.clone())
                        .unwrap_or_else(|error| {
                            crate::orchestration::context_governor::GovernorDecision {
                                state: GovernorState::Unknown,
                                action:
                                    crate::orchestration::context_governor::GovernorAction::Warn,
                                utilization_percent: None,
                                rollover_allowed: false,
                                deferred_for_boundary: false,
                                reason: safe_error(&error.to_string()),
                            }
                        });
                    (observation, decision, None)
                }
            }
        } else {
            let observation = ContextObservation::unknown(
                &session,
                &event_id,
                now,
                "no invocation-scoped OpenCode V2 client is available; context telemetry is unknown",
            );
            let decision = self
                .controller
                .evaluate_context(&session, observation.clone())
                .unwrap_or_else(
                    |error| crate::orchestration::context_governor::GovernorDecision {
                        state: GovernorState::Unknown,
                        action: crate::orchestration::context_governor::GovernorAction::Warn,
                        utilization_percent: None,
                        rollover_allowed: false,
                        deferred_for_boundary: false,
                        reason: safe_error(&error.to_string()),
                    },
                );
            (observation, decision, None)
        };

        let value = context_governance_value(&observation, &decision, rollover.as_ref());
        BridgeOutcome {
            value,
            metrics: OrchestrationMetrics::default(),
            outcome: if decision.state == GovernorState::Unknown {
                Outcome::Unknown
            } else {
                Outcome::Success
            },
            role: Some(Role::Lead.as_str().to_string()),
            session_id: Some(session),
            task_type: "context".to_string(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn collect_v2_observation(
        &self,
        runtime: &dyn RolloverRuntime,
        session: &str,
        event_id: &str,
        now: i64,
        finish: Option<String>,
        safe_boundary: bool,
        assistant_message_id: Option<String>,
        step_tokens: Option<&Value>,
        payload: &Value,
    ) -> Result<ContextObservation, String> {
        let messages = runtime.context_messages(session).map_err(|error| {
            format!(
                "V2 context query unavailable: {}",
                safe_error(&error.to_string())
            )
        })?;
        let info = runtime.session_info(session).map_err(|error| {
            format!(
                "V2 session query unavailable: {}",
                safe_error(&error.to_string())
            )
        })?;
        if info.get("id").and_then(Value::as_str) != Some(session) {
            return Err("V2 session query returned a different session identity".to_string());
        }
        let model = info.get("model").and_then(Value::as_object);
        let provider_id = payload
            .get("provider_id")
            .and_then(Value::as_str)
            .or_else(|| {
                model
                    .and_then(|model| model.get("providerID"))
                    .and_then(Value::as_str)
            })
            .map(str::to_string);
        let model_id = payload
            .get("model_id")
            .and_then(Value::as_str)
            .or_else(|| {
                model
                    .and_then(|model| model.get("id").or_else(|| model.get("modelID")))
                    .and_then(Value::as_str)
            })
            .map(str::to_string);
        let mut metadata = if provider_id.is_some() || model_id.is_some() {
            runtime
                .model_metadata(provider_id.as_deref(), model_id.as_deref())
                .map_err(|error| {
                    format!(
                        "V2 model metadata unavailable: {}",
                        safe_error(&error.to_string())
                    )
                })?
        } else {
            crate::orchestration::context_governor::ModelMetadata::default()
        };
        // If the runtime reports a smaller effective budget on the session
        // record, prefer it over the broader catalogue value. This is still a
        // runtime observation, not a guessed provider limit.
        let runtime_limit = info
            .get("limit")
            .and_then(|limit| limit.get("context").or_else(|| limit.get("contextLimit")))
            .and_then(Value::as_u64)
            .or_else(|| {
                model.and_then(|model| {
                    model
                        .get("limit")
                        .and_then(|limit| {
                            limit.get("context").or_else(|| limit.get("contextLimit"))
                        })
                        .and_then(Value::as_u64)
                })
            })
            .or_else(|| info.get("contextLimit").and_then(Value::as_u64))
            .or_else(|| model.and_then(|model| model.get("contextLimit").and_then(Value::as_u64)))
            .filter(|limit| *limit > 0);
        if let Some(runtime_limit) = runtime_limit {
            metadata.effective_limit = Some(
                metadata
                    .effective_limit
                    .map(|limit| limit.min(runtime_limit))
                    .unwrap_or(runtime_limit),
            );
            metadata.source = Some("opencode-v2:/api/session+model".to_string());
        }
        Ok(ContextObservation::from_v2(
            session,
            event_id,
            now,
            finish,
            safe_boundary,
            assistant_message_id,
            step_tokens,
            &messages,
            metadata,
        ))
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

    /// `lead.output` (OpenCode V2 event stream): persist the raw user-visible
    /// text of one *completed* root Lead assistant message.
    ///
    /// The adapter decides completion and only ever reports OCG Lead agents, but
    /// the bridge re-checks both: a worker (`ocg-*`), an unknown agent and an
    /// empty payload are refused, so no other session can overwrite the file.
    /// The write itself is atomic and every failure is soft — a broken report
    /// must never break a session.
    fn lead_output(&self, payload: &Value) -> BridgeOutcome {
        let session_id = session_id(payload);
        let agent = payload.get("agent").and_then(Value::as_str).unwrap_or("");
        let text = payload.get("text").and_then(Value::as_str).unwrap_or("");
        let reject = |message: &str, outcome: Outcome| BridgeOutcome {
            value: json!({"ok": false, "error": message}),
            metrics: OrchestrationMetrics::default(),
            outcome,
            role: None,
            session_id: Some(session_id.clone()),
            task_type: "report".to_string(),
        };
        if !self.reports.latest_lead_output.enabled {
            return BridgeOutcome {
                value: json!({"ok": false, "disabled": true, "context": ""}),
                metrics: OrchestrationMetrics::default(),
                outcome: Outcome::Unknown,
                role: None,
                session_id: Some(session_id),
                task_type: "report".to_string(),
            };
        }
        if !agent.starts_with("lead-") {
            return reject(
                "lead.output is only accepted from a root Lead agent",
                Outcome::Unknown,
            );
        }
        if text.trim().is_empty() {
            return reject("empty lead.output payload", Outcome::Unknown);
        }
        match crate::reports::write_latest_lead_output(self.controller.root(), text) {
            Ok(path) => BridgeOutcome {
                value: json!({
                    "ok": true,
                    "event": "lead.output",
                    "session_id": session_id,
                    "bytes": text.len(),
                    "path": path.to_string_lossy(),
                }),
                metrics: OrchestrationMetrics::default(),
                outcome: Outcome::Success,
                role: Some(Role::Lead.as_str().to_string()),
                session_id: Some(session_id),
                task_type: "report".to_string(),
            },
            Err(error) => reject(&safe_error(&error.to_string()), Outcome::Failure),
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

fn context_governance_value(
    observation: &ContextObservation,
    decision: &crate::orchestration::context_governor::GovernorDecision,
    result: Option<&ContextGovernanceResult>,
) -> Value {
    let mut value = json!({
        "ok": true,
        "event": "context.observe",
        "session_id": observation.session_id,
        "event_id": observation.event_id,
        "observation": observation,
        "decision": decision,
    });
    if let Some(result) = result {
        value["rollover_status"] = result
            .rollover_status
            .map(|status| json!(status.as_str()))
            .unwrap_or(Value::Null);
        value["artifact_status"] = result
            .artifact_status
            .map(|status| json!(status.as_str()))
            .unwrap_or(Value::Null);
        value["artifact_id"] = result
            .artifact_id
            .clone()
            .map(Value::String)
            .unwrap_or(Value::Null);
        value["source_session_id"] = result
            .source_session_id
            .clone()
            .map(Value::String)
            .unwrap_or(Value::Null);
        value["target_session_id"] = result
            .target_session_id
            .clone()
            .map(Value::String)
            .unwrap_or(Value::Null);
        value["note"] = result
            .note
            .clone()
            .map(Value::String)
            .unwrap_or(Value::Null);
    } else {
        value["rollover_status"] = Value::Null;
        value["artifact_status"] = Value::Null;
        value["artifact_id"] = Value::Null;
        value["source_session_id"] = json!(observation.session_id);
        value["target_session_id"] = Value::Null;
        value["note"] = if decision.state == GovernorState::Disabled {
            json!("context governor is disabled; no telemetry or rollover was attempted")
        } else {
            json!("runtime context was unknown; no rollover was attempted")
        };
    }
    value
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

    fn bridge_with_reports<'a>(
        controller: &'a Controller<'a>,
        runner: &'a FakeCaptureRunner,
        reports: ReportsConfig,
    ) -> BridgeContext<'a> {
        BridgeContext::new(controller, runner, TelemetryConfig::disabled()).with_reports(reports)
    }

    #[test]
    fn context_observation_without_a_runtime_is_unknown_and_fail_soft() {
        let dir = tempfile::tempdir().unwrap();
        let git = FakeGitHost::new();
        let clock = FixedClock::new(1);
        let controller = controller(dir.path(), &git, &clock, VerificationConfig::default());
        let runner = FakeCaptureRunner::new();
        let bridge = BridgeContext::new(&controller, &runner, TelemetryConfig::disabled());
        let admitted = bridge.dispatch(
            "session.prompt",
            &json!({"session_id": "ses_source", "text": "keep this Mission durable"}),
        );
        assert_eq!(admitted["ok"], json!(true));
        let value = bridge.dispatch(
            "context.observe",
            &json!({
                "session_id": "ses_source",
                "event_id": "event-unknown",
                "agent": "lead-high",
                "finish": "stop",
                "safe_boundary": true,
                "output_persisted": true,
                "tokens": {"input": 90, "cache": {"read": 0}},
            }),
        );
        assert_eq!(value["ok"], json!(true));
        assert_eq!(value["decision"]["state"], json!("unknown"));
        assert_eq!(value["decision"]["rollover_allowed"], json!(false));
        assert_eq!(value["artifact_id"], json!(null));
    }

    #[test]
    fn lead_output_persists_the_raw_text_byte_verbatim() {
        let dir = tempfile::tempdir().unwrap();
        let git = FakeGitHost::new();
        let clock = FixedClock::new(1);
        let controller = controller(dir.path(), &git, &clock, VerificationConfig::default());
        let runner = FakeCaptureRunner::new();
        let bridge = bridge_with_reports(&controller, &runner, ReportsConfig::default());
        let text = "# Final answer\n\n- one\n- two\n\n```rust\nfn done() {}\n```\n";
        let value = bridge.dispatch(
            "lead.output",
            &json!({"session_id": "s", "message_id": "m", "agent": "lead-high", "text": text}),
        );
        assert_eq!(value["ok"], json!(true), "{value}");
        let path = crate::reports::latest_lead_output_path(dir.path());
        assert_eq!(value["path"], json!(path.to_string_lossy()));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }

    #[test]
    fn lead_output_refuses_worker_and_non_ocg_agents() {
        let dir = tempfile::tempdir().unwrap();
        let git = FakeGitHost::new();
        let clock = FixedClock::new(1);
        let controller = controller(dir.path(), &git, &clock, VerificationConfig::default());
        let runner = FakeCaptureRunner::new();
        let bridge = bridge_with_reports(&controller, &runner, ReportsConfig::default());
        for agent in ["ocg-build", "ocg-verify", "build", ""] {
            let value = bridge.dispatch(
                "lead.output",
                &json!({"session_id": "s", "agent": agent, "text": "worker text\n"}),
            );
            assert_eq!(value["ok"], json!(false), "agent {agent}");
        }
        assert!(
            !crate::reports::latest_lead_output_path(dir.path()).exists(),
            "a worker must never create the report"
        );
    }

    #[test]
    fn lead_output_refuses_an_empty_payload() {
        let dir = tempfile::tempdir().unwrap();
        let git = FakeGitHost::new();
        let clock = FixedClock::new(1);
        let controller = controller(dir.path(), &git, &clock, VerificationConfig::default());
        let runner = FakeCaptureRunner::new();
        let bridge = bridge_with_reports(&controller, &runner, ReportsConfig::default());
        let value = bridge.dispatch(
            "lead.output",
            &json!({"session_id": "s", "agent": "lead-low", "text": "   \n"}),
        );
        assert_eq!(value["ok"], json!(false), "{value}");
        assert!(!crate::reports::latest_lead_output_path(dir.path()).exists());
    }

    #[test]
    fn lead_output_is_inert_when_the_switch_is_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let git = FakeGitHost::new();
        let clock = FixedClock::new(1);
        let controller = controller(dir.path(), &git, &clock, VerificationConfig::default());
        let runner = FakeCaptureRunner::new();
        let reports = ReportsConfig::from_config(&json!({
            "reports": {"latestLeadOutput": {"enabled": false}}
        }))
        .unwrap();
        let bridge = bridge_with_reports(&controller, &runner, reports);
        let value = bridge.dispatch(
            "lead.output",
            &json!({"session_id": "s", "agent": "lead-high", "text": "hidden\n"}),
        );
        assert_eq!(value["ok"], json!(false));
        assert_eq!(value["disabled"], json!(true));
        assert!(!crate::reports::latest_lead_output_path(dir.path()).exists());
    }

    #[test]
    fn lead_output_reports_a_write_failure_without_panicking() {
        let dir = tempfile::tempdir().unwrap();
        // A file where the reports directory must be makes the write fail.
        std::fs::create_dir_all(dir.path().join(".opencode-gear")).unwrap();
        std::fs::write(dir.path().join(".opencode-gear/reports"), "blocked").unwrap();
        let git = FakeGitHost::new();
        let clock = FixedClock::new(1);
        let controller = controller(dir.path(), &git, &clock, VerificationConfig::default());
        let runner = FakeCaptureRunner::new();
        let bridge = bridge_with_reports(&controller, &runner, ReportsConfig::default());
        let value = bridge.dispatch(
            "lead.output",
            &json!({"session_id": "s", "agent": "lead-high", "text": "cannot land\n"}),
        );
        assert_eq!(value["ok"], json!(false), "{value}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join(".opencode-gear/reports")).unwrap(),
            "blocked"
        );
    }
}

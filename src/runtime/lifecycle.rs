//! Runtime-neutral execution lifecycle vocabulary.
//!
//! This is the small boundary between durable OCG control-plane semantics and
//! a concrete execution engine. It contains only operations that the current
//! controller and bridge already need. OpenCode HTTP, session routes, agent
//! names, and plugin event shapes belong to the concrete adapter, not here.

use crate::telemetry::task::redact;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;

/// An opaque execution identity owned by a runtime adapter.
///
/// It is intentionally not a Mission identity. A Mission may outlive many
/// execution objects, and a rollover replaces this value without changing the
/// Mission ID or generation.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RuntimeExecutionId(String);

impl RuntimeExecutionId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RuntimeExecutionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl From<String> for RuntimeExecutionId {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl From<&str> for RuntimeExecutionId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

/// A logical execution profile selected by policy and applied by an adapter.
///
/// `profile_id` and `model_selector` are opaque to the control plane. The
/// OpenCode adapter currently maps them to an agent and provider/model; another
/// adapter may use the same fields for a different profile mechanism.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeProfile {
    pub profile_id: String,
    pub model_selector: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
}

impl RuntimeProfile {
    pub fn new(
        profile_id: impl Into<String>,
        model_selector: impl Into<String>,
        variant: Option<String>,
    ) -> Self {
        Self {
            profile_id: profile_id.into(),
            model_selector: model_selector.into(),
            level: None,
            variant,
        }
    }

    pub fn with_level(mut self, level: impl Into<String>) -> Self {
        self.level = Some(level.into());
        self
    }

    pub fn is_empty(&self) -> bool {
        self.profile_id.trim().is_empty() || self.model_selector.trim().is_empty()
    }
}

/// A credential-free identity for the runtime implementation behind an
/// adapter. It describes the engine/family, never a service endpoint or secret.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeIdentity {
    pub runtime: String,
    pub family: String,
    pub instance: String,
}

impl RuntimeIdentity {
    pub fn new(
        runtime: impl Into<String>,
        family: impl Into<String>,
        instance: impl Into<String>,
    ) -> Self {
        Self {
            runtime: runtime.into(),
            family: family.into(),
            instance: instance.into(),
        }
    }
}

/// Capabilities are facts about an adapter, not policy or placement decisions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RuntimeCapabilities {
    pub resolve_execution: bool,
    pub create_execution: bool,
    pub inspect_execution: bool,
    pub select_profile: bool,
    pub observe_context: bool,
    pub stage_continuation: bool,
    pub resume_continuation: bool,
}

impl RuntimeCapabilities {
    pub const NONE: Self = Self {
        resolve_execution: false,
        create_execution: false,
        inspect_execution: false,
        select_profile: false,
        observe_context: false,
        stage_continuation: false,
        resume_continuation: false,
    };

    pub const OPENCODE_V2: Self = Self {
        resolve_execution: true,
        create_execution: true,
        inspect_execution: true,
        select_profile: true,
        observe_context: true,
        stage_continuation: true,
        resume_continuation: true,
    };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeCapability {
    ResolveExecution,
    CreateExecution,
    InspectExecution,
    SelectProfile,
    ObserveContext,
    StageContinuation,
    ResumeContinuation,
}

impl RuntimeCapability {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ResolveExecution => "resolve_execution",
            Self::CreateExecution => "create_execution",
            Self::InspectExecution => "inspect_execution",
            Self::SelectProfile => "select_profile",
            Self::ObserveContext => "observe_context",
            Self::StageContinuation => "stage_continuation",
            Self::ResumeContinuation => "resume_continuation",
        }
    }
}

/// The small set of runtime failure classifications needed by control-plane
/// callers. Details remain available after secret redaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeErrorKind {
    Unavailable,
    Unsupported,
    ExecutionMissing,
    Authentication,
    Transport,
    InvalidResponse,
    ProfileSelection,
    ProviderCompletion,
}

impl RuntimeErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::Unsupported => "unsupported",
            Self::ExecutionMissing => "execution_missing",
            Self::Authentication => "authentication",
            Self::Transport => "transport",
            Self::InvalidResponse => "invalid_response",
            Self::ProfileSelection => "profile_selection",
            Self::ProviderCompletion => "provider_completion",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeError {
    kind: RuntimeErrorKind,
    detail: String,
}

impl RuntimeError {
    pub fn new(kind: RuntimeErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: redact(&detail.into()),
        }
    }

    pub fn unsupported(capability: RuntimeCapability) -> Self {
        Self::new(
            RuntimeErrorKind::Unsupported,
            format!("runtime does not support {}", capability.as_str()),
        )
    }

    pub fn kind(&self) -> RuntimeErrorKind {
        self.kind
    }

    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "runtime {}: {}", self.kind.as_str(), self.detail)
    }
}

impl std::error::Error for RuntimeError {}

pub type RuntimeResult<T> = std::result::Result<T, RuntimeError>;

/// A verified execution object returned by an adapter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeExecution {
    pub id: RuntimeExecutionId,
    pub profile: Option<RuntimeProfile>,
}

/// Neutral token usage reported by a runtime.
///
/// Cache reads and writes remain separate fields. The active-context
/// projection is one message's input plus cache-read count, never a sum over a
/// transcript.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RuntimeContextUsage {
    pub input: Option<u64>,
    pub output: Option<u64>,
    pub reasoning: Option<u64>,
    pub cache_read: Option<u64>,
    pub cache_write: Option<u64>,
}

impl RuntimeContextUsage {
    pub fn is_empty(&self) -> bool {
        self.input.is_none()
            && self.output.is_none()
            && self.reasoning.is_none()
            && self.cache_read.is_none()
            && self.cache_write.is_none()
    }

    /// Parse the common runtime token shape without turning missing values into
    /// zero. The concrete adapter remains responsible for choosing this data.
    pub fn from_value(value: Option<&Value>) -> Self {
        let Some(value) = value else {
            return Self::default();
        };
        let cache = value.get("cache");
        Self {
            input: value.get("input").and_then(Value::as_u64),
            output: value.get("output").and_then(Value::as_u64),
            reasoning: value.get("reasoning").and_then(Value::as_u64),
            cache_read: cache
                .and_then(|value| value.get("read"))
                .and_then(Value::as_u64),
            cache_write: cache
                .and_then(|value| value.get("write"))
                .and_then(Value::as_u64),
        }
    }

    pub fn active_context_tokens(&self) -> Option<u64> {
        match (self.input, self.cache_read) {
            (None, None) => None,
            (Some(input), None) => Some(input),
            (None, Some(cache_read)) => Some(cache_read),
            (Some(input), Some(cache_read)) => input.checked_add(cache_read),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeProvenance {
    Exact,
    Estimated,
    #[default]
    Unknown,
}

/// Model limits and identity after an adapter has normalized its transport
/// response. The source is a diagnostic label, never a raw API response.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RuntimeModelMetadata {
    pub provider_id: Option<String>,
    pub model_id: Option<String>,
    pub context_limit: Option<u64>,
    pub input_limit: Option<u64>,
    pub output_limit: Option<u64>,
    pub effective_limit: Option<u64>,
    pub source: Option<String>,
}

/// A safe boundary event supplied by a plugin/bridge and interpreted by the
/// runtime adapter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeContextEvent {
    pub execution_id: RuntimeExecutionId,
    pub event_id: String,
    pub observed_at: i64,
    pub assistant_message_id: Option<String>,
    pub finish: Option<String>,
    pub safe_boundary: bool,
    pub reported_usage: Option<RuntimeContextUsage>,
    pub reported_profile: Option<RuntimeProfile>,
}

/// Runtime-neutral context observation consumed by the context governor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeContextObservation {
    pub execution_id: RuntimeExecutionId,
    pub event_id: String,
    pub observed_at: i64,
    pub assistant_message_id: Option<String>,
    pub finish: Option<String>,
    pub safe_boundary: bool,
    pub usage: RuntimeContextUsage,
    pub used_tokens: Option<u64>,
    pub limit_tokens: Option<u64>,
    pub model: RuntimeModelMetadata,
    pub message_count: usize,
    pub compaction_count: usize,
    pub usage_provenance: RuntimeProvenance,
    pub context_provenance: RuntimeProvenance,
    pub note: Option<String>,
}

impl RuntimeContextObservation {
    pub fn unknown(event: &RuntimeContextEvent, note: impl Into<String>) -> Self {
        Self {
            execution_id: event.execution_id.clone(),
            event_id: event.event_id.clone(),
            observed_at: event.observed_at,
            assistant_message_id: event.assistant_message_id.clone(),
            finish: event.finish.clone(),
            safe_boundary: false,
            usage: RuntimeContextUsage::default(),
            used_tokens: None,
            limit_tokens: None,
            model: RuntimeModelMetadata::default(),
            message_count: 0,
            compaction_count: 0,
            usage_provenance: RuntimeProvenance::Unknown,
            context_provenance: RuntimeProvenance::Unknown,
            note: Some(redact(&note.into())),
        }
    }
}

/// A continuation is an execution request, not a Mission identity transition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeContinuation {
    pub id: String,
    pub text: String,
    pub description: String,
    pub metadata: Value,
}

impl RuntimeContinuation {
    pub fn new(
        id: impl Into<String>,
        text: impl Into<String>,
        description: impl Into<String>,
        metadata: Value,
    ) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
            description: description.into(),
            metadata,
        }
    }
}

/// The lifecycle operations currently consumed by OCG orchestration.
///
/// Optional operations are checked through [`RuntimeCapabilities`] and return
/// [`RuntimeErrorKind::Unsupported`] explicitly. Implementations must not
/// silently fall back to a different execution semantic.
pub trait RuntimeAdapter {
    fn identity(&self) -> RuntimeIdentity;
    fn capabilities(&self) -> RuntimeCapabilities;

    fn resolve_execution(&mut self) -> RuntimeResult<RuntimeExecutionId> {
        Err(RuntimeError::unsupported(
            RuntimeCapability::ResolveExecution,
        ))
    }

    fn create_execution(&mut self) -> RuntimeResult<RuntimeExecutionId> {
        Err(RuntimeError::unsupported(
            RuntimeCapability::CreateExecution,
        ))
    }

    fn inspect_execution(
        &self,
        _execution_id: &RuntimeExecutionId,
    ) -> RuntimeResult<RuntimeExecution> {
        Err(RuntimeError::unsupported(
            RuntimeCapability::InspectExecution,
        ))
    }

    fn prepare_execution(
        &mut self,
        _execution_id: &RuntimeExecutionId,
        _profile: &RuntimeProfile,
    ) -> RuntimeResult<RuntimeExecution> {
        Err(RuntimeError::unsupported(RuntimeCapability::SelectProfile))
    }

    fn observe_context(
        &self,
        _event: &RuntimeContextEvent,
    ) -> RuntimeResult<RuntimeContextObservation> {
        Err(RuntimeError::unsupported(RuntimeCapability::ObserveContext))
    }

    fn stage_runtime_continuation(
        &self,
        _execution_id: &RuntimeExecutionId,
        _continuation: &RuntimeContinuation,
    ) -> RuntimeResult<()> {
        Err(RuntimeError::unsupported(
            RuntimeCapability::StageContinuation,
        ))
    }

    fn resume_runtime_continuation(
        &self,
        _execution_id: &RuntimeExecutionId,
        _continuation: &RuntimeContinuation,
    ) -> RuntimeResult<()> {
        Err(RuntimeError::unsupported(
            RuntimeCapability::ResumeContinuation,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EmptyAdapter;

    impl RuntimeAdapter for EmptyAdapter {
        fn identity(&self) -> RuntimeIdentity {
            RuntimeIdentity::new("empty", "test", "unit")
        }

        fn capabilities(&self) -> RuntimeCapabilities {
            RuntimeCapabilities::NONE
        }
    }

    #[test]
    fn execution_identity_is_not_mission_identity() {
        let execution = RuntimeExecutionId::new("execution-a");
        assert_ne!(execution.as_str(), "task-mission");
        assert_eq!(execution.to_string(), "execution-a");
    }

    #[test]
    fn context_projection_does_not_sum_transcripts() {
        let usage = RuntimeContextUsage {
            input: Some(40),
            cache_read: Some(2),
            cache_write: Some(100),
            ..RuntimeContextUsage::default()
        };
        assert_eq!(usage.active_context_tokens(), Some(42));
    }

    #[test]
    fn unsupported_capabilities_are_explicit() {
        let error = RuntimeError::unsupported(RuntimeCapability::ResumeContinuation);
        assert_eq!(error.kind(), RuntimeErrorKind::Unsupported);
        assert!(error.to_string().contains("resume_continuation"));
    }

    #[test]
    fn empty_adapter_reports_unsupported_operations() {
        let mut adapter = EmptyAdapter;
        let error = adapter.create_execution().unwrap_err();
        assert_eq!(error.kind(), RuntimeErrorKind::Unsupported);
    }

    #[test]
    fn runtime_errors_are_classified_and_redacted() {
        let error = RuntimeError::new(
            RuntimeErrorKind::Authentication,
            "Authorization: Bearer super-secret-token-value",
        );
        assert_eq!(error.kind(), RuntimeErrorKind::Authentication);
        assert!(!error.detail().contains("super-secret-token-value"));
    }
}

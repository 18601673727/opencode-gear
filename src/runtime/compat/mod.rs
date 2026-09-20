//! OpenCode runtime compatibility boundary.
//!
//! Gear supports two structurally different OpenCode major families:
//!
//! ```text
//! runtime/common contract
//! ├── v1 adapter   (1.18.x: `plugin`, `task`, request-scoped Lead)
//! └── v2 adapter   (2.0.x: `plugins`, `subagent`, session-scoped Lead)
//! ```
//!
//! Every version-specific decision lives here. No unrelated module may branch
//! on the OpenCode major version: callers ask for the detected [`Major`], get an
//! adapter from [`adapter_for`], and use the adapter's contract. The two
//! adapters are deliberately thin data+behaviour; all policy (which model, which
//! variant, who may run what) stays in Rust.
//!
//! Detection is explicit. [`detect`] parses an `opencode --version` string and
//! fails clearly for a structurally incompatible major (for example `3.x` or a
//! `1.x` below the [`v1_floor`]). A runtime whose version genuinely cannot be
//! determined is *unclassified* and callers fall back to the v1 contract to
//! preserve the historical 1.18.x launch path.

pub mod v1;
pub mod v2;

use crate::error::{GearError, Result};
use crate::model::LeadContract;
use crate::process::ProcessHost;
use semver::Version;
use std::path::Path;

/// The oldest OpenCode release Gear supports. `OPENCODE_CONFIG_CONTENT` and the
/// orchestration adapter require at least this version.
pub fn v1_floor() -> Version {
    Version::new(1, 18, 0)
}

/// The oldest OpenCode 2 release Gear supports.
pub fn v2_floor() -> Version {
    Version::new(2, 0, 0)
}

/// The OpenCode 2 release Gear is currently verified against.
pub fn v2_verified_baseline() -> Version {
    Version::new(2, 0, 10)
}

/// A supported OpenCode major family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Major {
    V1,
    V2,
}

impl Major {
    pub fn as_str(self) -> &'static str {
        match self {
            Major::V1 => "v1",
            Major::V2 => "v2",
        }
    }
}

/// A detected OpenCode release, already classified into a supported family.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeVersion {
    version: Version,
    major: Major,
}

impl RuntimeVersion {
    pub fn version(&self) -> &Version {
        &self.version
    }

    pub fn major(&self) -> Major {
        self.major
    }
}

/// Parse a raw `opencode --version` string and classify it.
///
/// The output is tolerated to be either a bare semver (`1.18.31`) or a
/// decorated one (`opencode 1.18.31`). Every whitespace token is considered and
/// the first parseable semver wins. A parseable but unsupported major, or a
/// `1.x` below [`v1_floor`], is a clear error; an unparseable string is also an
/// error so callers can decide whether to fall back.
pub fn detect(raw: &str) -> Result<RuntimeVersion> {
    let version = parse_version_token(raw).ok_or_else(|| {
        GearError::config(format!(
            "cannot determine the OpenCode version from `{raw}`; Gear requires OpenCode {} or a 2.x release",
            v1_floor()
        ))
    })?;
    classify(version)
}

/// Classify an already parsed semver into a supported family, failing clearly
/// for an unsupported major or a release below the family floor.
pub fn classify(version: Version) -> Result<RuntimeVersion> {
    let major = match version.major {
        1 => {
            if version < v1_floor() {
                return Err(GearError::config(format!(
                    "OpenCode {version} is below the required {} floor",
                    v1_floor()
                )));
            }
            Major::V1
        }
        2 => {
            if version < v2_floor() {
                return Err(GearError::config(format!(
                    "OpenCode {version} is below the required {} floor",
                    v2_floor()
                )));
            }
            Major::V2
        }
        other => {
            return Err(GearError::config(format!(
                "OpenCode {version} belongs to unsupported major version {other}; Gear supports the 1.18.x (v1) and 2.x (v2) families"
            )))
        }
    };

    Ok(RuntimeVersion { version, major })
}

/// Probe a runtime explicitly and classify it. This is the only place the
/// runtime major is established for a launch.
pub fn detect_from_host(host: &dyn ProcessHost, program: &Path) -> Result<RuntimeVersion> {
    let raw = host.version(program)?;
    detect(&raw)
}

/// The adapter for a detected family.
pub fn adapter_for(version: &RuntimeVersion) -> &'static dyn RuntimeAdapter {
    match version.major() {
        Major::V1 => v1_adapter(),
        Major::V2 => v2_adapter(),
    }
}

/// The v1 (OpenCode 1.18.x) contract.
pub fn v1_adapter() -> &'static dyn RuntimeAdapter {
    &v1::V1Adapter
}

/// The v2 (OpenCode 2.x) contract.
pub fn v2_adapter() -> &'static dyn RuntimeAdapter {
    &v2::V2Adapter
}

fn parse_version_token(raw: &str) -> Option<Version> {
    raw.split_whitespace().find_map(|token| {
        let token =
            token.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '.' && c != '-');
        let token = token.strip_prefix('v').unwrap_or(token);
        Version::parse(token).ok()
    })
}

/// How a runtime applies the Rust-resolved Lead contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeadSelectionMode {
    /// The adapter rewrites the mutable request message before OpenCode saves
    /// it (`chat.message`); the contract wins over sticky session/UI state.
    RequestMessage,
    /// The Lead is selected on the session itself: resolve a session, switch
    /// its agent/model/variant, then read the effective Lead back.
    Session,
}

/// How Gear reaches the running OpenCode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchMode {
    /// Replace the process with `opencode` (interactive CLI).
    Exec,
    /// OpenCode 2 runs a daemon; the client talks HTTP/SSE to it.
    Daemon,
}

/// The narrow, version-specific contract every other module depends on.
pub trait RuntimeAdapter: Send + Sync {
    /// The family this adapter implements.
    fn major(&self) -> Major;

    /// The generated-config array key for local plugins (`plugin` vs `plugins`).
    fn plugin_key(&self) -> &'static str;

    /// The OpenCode permission/tool key for consumer delegation.
    fn task_key(&self) -> &'static str;

    /// The generated plugin source for this runtime.
    fn plugin_source(&self) -> &'static str;

    /// The `file://` URL for a local adapter, canonical and absolute.
    fn local_plugin_uri(&self, path: &Path) -> Result<String>;

    /// How the Lead contract is applied.
    fn lead_selection(&self) -> LeadSelectionMode;

    /// How a launch reaches the runtime.
    fn launch_mode(&self) -> LaunchMode;

    /// Whether a tool event names this runtime's delegation tool.
    fn is_task_tool(&self, tool: &str) -> bool;
}

/// The exact Lead selection Gear resolved for one throttle level, in the shape
/// a runtime adapter needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeadSelection {
    pub level: String,
    pub agent: String,
    pub provider_id: String,
    pub model_id: String,
    /// Present only when the Lead contract declares one. Never fabricated.
    pub variant: Option<String>,
}

impl LeadSelection {
    pub fn from_contract(contract: &LeadContract) -> Self {
        Self {
            level: contract.level.clone(),
            agent: contract.agent.clone(),
            provider_id: contract.provider_id.clone(),
            model_id: contract.model_id.clone(),
            variant: contract.variant.clone(),
        }
    }

    pub fn full_model_id(&self) -> String {
        format!("{}/{}", self.provider_id, self.model_id)
    }
}

/// The Lead a session actually reports after selection. Fields are optional so
/// an unavailable observation is distinct from a contradictory one.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EffectiveLead {
    pub agent: Option<String>,
    pub provider_id: Option<String>,
    pub model_id: Option<String>,
    pub variant: Option<String>,
}

/// The session-level operations a v2 runtime exposes. Gear keeps no HTTP or SSE
/// details here; implementations are injected, so tests use a deterministic
/// in-memory client and production can bind an HTTP client.
pub trait SessionClient {
    /// Create a fresh session or resolve the project's active one.
    fn resolve_session(&mut self) -> Result<String>;
    /// Select or switch the session's Lead agent.
    fn select_agent(&mut self, session: &str, agent: &str) -> Result<()>;
    /// Select or switch the session's model; a `variant` is only ever supplied
    /// when the Rust-resolved contract has one.
    fn select_model(
        &mut self,
        session: &str,
        provider_id: &str,
        model_id: &str,
        variant: Option<&str>,
    ) -> Result<()>;
    /// Read the effective Lead back. Unavailable is an error: Gear refuses to
    /// rely on a Lead it cannot verify.
    fn effective_lead(&self, session: &str) -> Result<EffectiveLead>;
}

/// A deterministic, offline [`SessionClient`] used by tests and by callers that
/// need to rehearse a session selection without a live daemon.
#[derive(Debug, Clone, Default)]
pub struct MemorySessionClient {
    session_id: String,
    selected_agent: Option<String>,
    selected_model: Option<(String, String, Option<String>)>,
    forced_effective: Option<EffectiveLead>,
    resolve_error: Option<String>,
    effective_error: Option<String>,
    calls: Vec<String>,
}

impl MemorySessionClient {
    pub fn new() -> Self {
        Self {
            session_id: "ocg-session".to_string(),
            ..Self::default()
        }
    }

    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = session_id.into();
        self
    }

    /// Force a specific effective Lead, simulating a session that refused or
    /// silently overrode Gear's selection.
    pub fn with_effective(mut self, effective: EffectiveLead) -> Self {
        self.forced_effective = Some(effective);
        self
    }

    pub fn failing_resolve(mut self, message: impl Into<String>) -> Self {
        self.resolve_error = Some(message.into());
        self
    }

    pub fn failing_effective(mut self, message: impl Into<String>) -> Self {
        self.effective_error = Some(message.into());
        self
    }

    /// The ordered transcript of session operations, for determinism tests.
    pub fn calls(&self) -> &[String] {
        &self.calls
    }
}

impl SessionClient for MemorySessionClient {
    fn resolve_session(&mut self) -> Result<String> {
        self.calls.push("resolve_session".to_string());
        if let Some(message) = &self.resolve_error {
            return Err(GearError::config(message.clone()));
        }
        let session = if self.session_id.is_empty() {
            "ocg-session".to_string()
        } else {
            self.session_id.clone()
        };
        Ok(session)
    }

    fn select_agent(&mut self, session: &str, agent: &str) -> Result<()> {
        self.calls.push(format!("select_agent({session},{agent})"));
        self.selected_agent = Some(agent.to_string());
        Ok(())
    }

    fn select_model(
        &mut self,
        session: &str,
        provider_id: &str,
        model_id: &str,
        variant: Option<&str>,
    ) -> Result<()> {
        self.calls.push(format!(
            "select_model({session},{provider_id},{model_id},{})",
            variant.unwrap_or("")
        ));
        self.selected_model = Some((
            provider_id.to_string(),
            model_id.to_string(),
            variant.map(str::to_string),
        ));
        Ok(())
    }

    fn effective_lead(&self, _session: &str) -> Result<EffectiveLead> {
        if let Some(message) = &self.effective_error {
            return Err(GearError::config(message.clone()));
        }
        if let Some(forced) = &self.forced_effective {
            return Ok(forced.clone());
        }
        let (provider_id, model_id, variant) = self.selected_model.clone().unwrap_or_default();
        Ok(EffectiveLead {
            agent: self.selected_agent.clone(),
            provider_id: (!provider_id.is_empty()).then_some(provider_id),
            model_id: (!model_id.is_empty()).then_some(model_id),
            variant,
        })
    }
}

/// The verified result of session-level Lead selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionLeadSelection {
    pub session_id: String,
    pub lead: LeadSelection,
    /// Deterministic transcript of the operations performed.
    pub steps: Vec<String>,
}

/// Perform v2 session-level Lead selection:
///
/// 1. create/resolve the session,
/// 2. select/switch the Lead agent,
/// 3. select/switch the model, applying the reasoning variant only when the
///    Rust-resolved contract has one,
/// 4. read the effective Lead back and verify it before relying on it.
///
/// A selection that cannot be read back, or that differs from the resolved
/// contract, is a hard error: Gear never proceeds on a guessed Lead.
pub fn select_session_lead(
    client: &mut dyn SessionClient,
    lead: &LeadSelection,
) -> Result<SessionLeadSelection> {
    let session_id = client.resolve_session()?;
    client.select_agent(&session_id, &lead.agent)?;
    client.select_model(
        &session_id,
        &lead.provider_id,
        &lead.model_id,
        lead.variant.as_deref(),
    )?;
    let effective = client.effective_lead(&session_id)?;
    verify_effective_lead(lead, &effective)?;
    Ok(SessionLeadSelection {
        session_id,
        lead: lead.clone(),
        steps: vec![
            "resolve_session".to_string(),
            format!("select_agent={}", lead.agent),
            format!("select_model={}", lead.full_model_id()),
            format!(
                "variant={}",
                lead.variant.as_deref().unwrap_or("provider-default")
            ),
            "verify_effective_lead".to_string(),
        ],
    })
}

/// Verify that the effective session Lead matches the Rust-resolved contract.
///
/// The reasoning variant is only checked when the contract declares one; a
/// provider-default contract is satisfied by any provider variant, and Gear
/// never invents one.
pub fn verify_effective_lead(expected: &LeadSelection, effective: &EffectiveLead) -> Result<()> {
    let agent = effective
        .agent
        .as_deref()
        .ok_or_else(|| unavailable(expected))?;
    let provider = effective
        .provider_id
        .as_deref()
        .ok_or_else(|| unavailable(expected))?;
    let model = effective
        .model_id
        .as_deref()
        .ok_or_else(|| unavailable(expected))?;

    if agent != expected.agent || provider != expected.provider_id || model != expected.model_id {
        return Err(GearError::config(format!(
            "contradictory active OpenCode Lead: resolved {} on {}/{}, but the session reports {} on {}/{}",
            expected.agent,
            expected.provider_id,
            expected.model_id,
            agent,
            provider,
            model
        )));
    }

    if let Some(variant) = &expected.variant {
        match effective.variant.as_deref() {
            Some(actual) if actual == variant => {}
            _ => {
                return Err(GearError::config(format!(
                    "contradictory active OpenCode Lead: resolved variant '{variant}', but the session reports '{}'",
                    effective.variant.as_deref().unwrap_or("none")
                )))
            }
        }
    }
    Ok(())
}

fn unavailable(expected: &LeadSelection) -> GearError {
    GearError::config(format!(
        "active OpenCode Lead is unavailable: the session did not report an effective agent/model for {} on {}",
        expected.agent,
        expected.full_model_id()
    ))
}

/// A non-authoritative observation. Optional catalogue/debug probes may be
/// unavailable; Gear warns and continues rather than failing a launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observation<T> {
    Observed(T),
    Unavailable(String),
}

impl<T> Observation<T> {
    pub fn observed(self) -> Option<T> {
        match self {
            Observation::Observed(value) => Some(value),
            Observation::Unavailable(_) => None,
        }
    }

    pub fn unavailable_reason(&self) -> Option<&str> {
        match self {
            Observation::Observed(_) => None,
            Observation::Unavailable(reason) => Some(reason),
        }
    }
}

/// Fold an optional probe result into a fail-soft observation, pushing a
/// warning on failure. This is the shared shape for catalogue/debug probes and
/// for a v2 session transport that is not configured.
pub fn observe_optional<T>(
    result: Result<T>,
    label: &str,
    warnings: &mut Vec<String>,
) -> Observation<T> {
    match result {
        Ok(value) => Observation::Observed(value),
        Err(error) => {
            warnings.push(format!("optional {label} probe unavailable: {error}"));
            Observation::Unavailable(error.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_supported_families_and_baselines() {
        let v1 = detect("1.18.31").unwrap();
        assert_eq!(v1.major(), Major::V1);
        assert_eq!(v1.version(), &Version::new(1, 18, 31));

        let v2 = detect("2.0.10").unwrap();
        assert_eq!(v2.major(), Major::V2);
        assert_eq!(v2.version(), &v2_verified_baseline());

        // A decorated output and a leading `v` still parse.
        assert_eq!(detect("opencode v2.0.1").unwrap().major(), Major::V2);
    }

    #[test]
    fn rejects_unknown_and_structurally_incompatible_majors() {
        for raw in ["3.0.0", "0.9.0", "opencode 10.1.2", "garbage", "v1.17.9"] {
            assert!(detect(raw).is_err(), "expected rejection for {raw}");
        }
    }

    #[test]
    fn adapters_expose_distinct_version_specific_contracts() {
        let v1 = adapter_for(&detect("1.18.31").unwrap());
        assert_eq!(v1.major(), Major::V1);
        assert_eq!(v1.plugin_key(), "plugin");
        assert_eq!(v1.task_key(), "task");
        assert_eq!(v1.lead_selection(), LeadSelectionMode::RequestMessage);
        assert_eq!(v1.launch_mode(), LaunchMode::Exec);
        assert!(v1.is_task_tool("task"));
        assert!(!v1.is_task_tool("subagent"));

        let v2 = adapter_for(&detect("2.0.10").unwrap());
        assert_eq!(v2.major(), Major::V2);
        assert_eq!(v2.plugin_key(), "plugins");
        assert_eq!(v2.task_key(), "subagent");
        assert_eq!(v2.lead_selection(), LeadSelectionMode::Session);
        assert_eq!(v2.launch_mode(), LaunchMode::Daemon);
        assert!(v2.is_task_tool("subagent"));
        assert!(!v2.is_task_tool("task"));
    }

    fn lead() -> LeadSelection {
        LeadSelection {
            level: "high".to_string(),
            agent: "lead-high".to_string(),
            provider_id: "openai".to_string(),
            model_id: "gpt-6-astra".to_string(),
            variant: Some("low".to_string()),
        }
    }

    #[test]
    fn session_lead_selection_is_deterministic_and_applies_the_variant() {
        let mut client = MemorySessionClient::new();
        let first = select_session_lead(&mut client, &lead()).unwrap();
        assert_eq!(first.session_id, "ocg-session");
        assert_eq!(
            client.calls(),
            [
                "resolve_session",
                "select_agent(ocg-session,lead-high)",
                "select_model(ocg-session,openai,gpt-6-astra,low)",
            ]
        );

        // A contract without a variant must not send one.
        let mut plain = MemorySessionClient::new();
        let mut no_variant = lead();
        no_variant.variant = None;
        let outcome = select_session_lead(&mut plain, &no_variant).unwrap();
        assert!(outcome
            .steps
            .iter()
            .any(|step| step == "variant=provider-default"));
        assert_eq!(
            plain.calls(),
            [
                "resolve_session",
                "select_agent(ocg-session,lead-high)",
                "select_model(ocg-session,openai,gpt-6-astra,)",
            ]
        );
    }

    #[test]
    fn contradictory_or_unavailable_effective_lead_is_a_failure() {
        let contradictory = EffectiveLead {
            agent: Some("lead-low".to_string()),
            provider_id: Some("openai".to_string()),
            model_id: Some("gpt-5.6-sol".to_string()),
            variant: Some("low".to_string()),
        };
        let error = verify_effective_lead(&lead(), &contradictory).unwrap_err();
        assert!(error.to_string().contains("contradictory"), "{error}");

        let unavailable = EffectiveLead::default();
        let error = verify_effective_lead(&lead(), &unavailable).unwrap_err();
        assert!(error.to_string().contains("unavailable"), "{error}");

        // A wrong variant is contradictory only when the contract declares one.
        let wrong_variant = EffectiveLead {
            agent: Some("lead-high".to_string()),
            provider_id: Some("openai".to_string()),
            model_id: Some("gpt-6-astra".to_string()),
            variant: Some("high".to_string()),
        };
        assert!(verify_effective_lead(&lead(), &wrong_variant).is_err());
        let mut no_variant = lead();
        no_variant.variant = None;
        assert!(verify_effective_lead(&no_variant, &wrong_variant).is_ok());
    }

    #[test]
    fn a_session_client_that_cannot_report_the_lead_fails_selection() {
        let mut client = MemorySessionClient::new().failing_effective("daemon unavailable");
        let error = select_session_lead(&mut client, &lead()).unwrap_err();
        assert!(error.to_string().contains("daemon unavailable"), "{error}");
    }

    #[test]
    fn optional_observations_warn_and_continue() {
        let mut warnings = Vec::new();
        let observation: Observation<u32> = observe_optional(
            Err(crate::error::GearError::config("no catalogue")),
            "catalogue",
            &mut warnings,
        );
        assert_eq!(
            observation.unavailable_reason(),
            Some("no catalogue"),
            "the reason must be preserved"
        );
        assert!(observation.observed().is_none());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("catalogue"), "{}", warnings[0]);

        let observed: Observation<u32> = observe_optional(Ok(7), "catalogue", &mut warnings);
        assert_eq!(observed.observed(), Some(7));
        assert_eq!(warnings.len(), 1);
    }
}

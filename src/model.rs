//! Model registry and role resolution.
//!
//! Roles are durable; providers and model ids are replaceable and live in
//! `config/models.yaml`. Routing roles are arbitrary: the code only assumes a
//! role maps to a model key.

use crate::error::{GearError, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub fn providers(data: &Value) -> Option<&Map<String, Value>> {
    data.get("models")
        .and_then(|models| models.get("providers"))
        .and_then(Value::as_object)
}

pub fn model_registry(data: &Value) -> Option<&Map<String, Value>> {
    data.get("models")
        .and_then(|models| models.get("models"))
        .and_then(Value::as_object)
}

pub fn role_specs(data: &Value) -> Option<&Map<String, Value>> {
    data.get("routing")
        .and_then(|routing| routing.get("roles"))
        .and_then(Value::as_object)
}

pub fn provider_label(data: &Value, provider: &str) -> String {
    providers(data)
        .and_then(|map| map.get(provider))
        .and_then(Value::as_object)
        .and_then(|entry| entry.get("label"))
        .and_then(Value::as_str)
        .filter(|label| !label.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| provider.to_string())
}

pub fn model_entry<'a>(data: &'a Value, key: &str) -> Result<&'a Map<String, Value>> {
    model_registry(data)
        .and_then(|registry| registry.get(key))
        .and_then(Value::as_object)
        .ok_or_else(|| GearError::config(format!("unknown model key: '{key}'")))
}

/// `(provider, "provider/model_id")` for a model key.
pub fn model_full_id(data: &Value, key: &str) -> Result<(String, String)> {
    let entry = model_entry(data, key)?;
    let provider = entry
        .get("provider")
        .and_then(Value::as_str)
        .filter(|provider| !provider.is_empty())
        .ok_or_else(|| GearError::config(format!("model '{key}' has no provider")))?;
    let id = entry
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| GearError::config(format!("model '{key}' has no id")))?;
    Ok((provider.to_string(), format!("{provider}/{id}")))
}

pub fn model_label(data: &Value, key: &str) -> String {
    match model_entry(data, key) {
        Ok(entry) => entry
            .get("label")
            .and_then(Value::as_str)
            .filter(|label| !label.is_empty())
            .or_else(|| entry.get("id").and_then(Value::as_str))
            .unwrap_or(key)
            .to_string(),
        Err(_) => key.to_string(),
    }
}

pub fn lead_agent_id(level: &str) -> String {
    format!("lead-{level}")
}

pub fn consumer_agent_id(role: &str) -> String {
    format!("{}{role}", crate::defaults::CONSUMER_AGENT_PREFIX)
}

/// The exact primary Lead request contract selected by one OCG throttle.
///
/// This value is resolved in Rust and handed to the thin OpenCode plugin for
/// enforcement at `chat.message`, after OpenCode has applied sticky UI/session
/// selection but before the user message is saved or sent to a provider.
///
/// The Lead is provider-agnostic: `provider_id`/`model_id` come from the
/// configured registry, not from a hardcoded OpenAI assumption. A reasoning
/// `variant` is optional and provider-specific. When the resolved model declares
/// no variant (or the throttle omits one), it is `None` and the request is left
/// at the provider default; it is never fabricated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeadContract {
    pub level: String,
    pub agent: String,
    pub provider_id: String,
    pub model_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
}

impl LeadContract {
    pub fn full_model_id(&self) -> String {
        format!("{}/{}", self.provider_id, self.model_id)
    }
}

/// Resolve the runtime Lead contract for a throttle level.
pub fn lead_contract(data: &Value, level: &str) -> Result<LeadContract> {
    let spec = data
        .get("throttle")
        .and_then(|throttle| throttle.get("levels"))
        .and_then(Value::as_object)
        .and_then(|levels| levels.get(level))
        .ok_or_else(|| GearError::config(format!("unknown throttle level: '{level}'")))?;
    let model_key = spec
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| GearError::config(format!("throttle level '{level}' is missing 'model'")))?;
    let entry = model_entry(data, model_key)?;
    let provider_id = entry
        .get("provider")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| GearError::config(format!("model '{model_key}' has no provider")))?;
    let model_id = entry
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| GearError::config(format!("model '{model_key}' has no id")))?;
    let variant = spec
        .get("variant")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    Ok(LeadContract {
        level: level.to_string(),
        agent: lead_agent_id(level),
        provider_id: provider_id.to_string(),
        model_id: model_id.to_string(),
        variant,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelRequirementKind {
    Lead,
    Consumer,
}

/// One provider/model that must be exposed by the active OpenCode runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRequirement {
    pub label: String,
    pub agent: String,
    pub full_model_id: String,
    pub variant: Option<String>,
    pub kind: ModelRequirementKind,
}

/// Every configured Lead throttle and consumer role, in declaration order.
/// Duplicate model IDs deliberately remain separate requirements because, for
/// example, low and mid may share a model while requiring different variants.
pub fn runtime_model_requirements(data: &Value) -> Result<Vec<ModelRequirement>> {
    let mut requirements = Vec::new();
    if let Some(levels) = data
        .get("throttle")
        .and_then(|throttle| throttle.get("levels"))
        .and_then(Value::as_object)
    {
        for level in levels.keys() {
            let contract = lead_contract(data, level)?;
            let full_model_id = contract.full_model_id();
            requirements.push(ModelRequirement {
                label: contract.agent.clone(),
                agent: contract.agent,
                full_model_id,
                variant: contract.variant,
                kind: ModelRequirementKind::Lead,
            });
        }
    }
    if let Some(roles) = role_specs(data) {
        for (role, spec) in roles {
            let key = spec.get("model").and_then(Value::as_str).ok_or_else(|| {
                GearError::config(format!("routing role '{role}' is missing 'model'"))
            })?;
            requirements.push(ModelRequirement {
                label: role.clone(),
                agent: consumer_agent_id(role),
                full_model_id: model_full_id(data, key)?.1,
                variant: spec
                    .get("variant")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                kind: ModelRequirementKind::Consumer,
            });
        }
    }
    Ok(requirements)
}

/// Resolve the active throttle level. CLI beats environment, which beats the
/// configured default.
pub fn resolve_throttle(data: &Value, cli: Option<&str>, env: Option<&str>) -> String {
    for value in [cli, env].into_iter().flatten() {
        if !value.is_empty() {
            return value.to_string();
        }
    }
    data.get("throttle")
        .and_then(|throttle| throttle.get("default"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// Every model key referenced by any lead level, role, fallback or small_model.
fn referenced_model_keys(data: &Value) -> Vec<String> {
    let mut keys = Vec::new();
    if let Some(levels) = data
        .get("throttle")
        .and_then(|throttle| throttle.get("levels"))
        .and_then(Value::as_object)
    {
        for spec in levels.values() {
            if let Some(key) = spec.get("model").and_then(Value::as_str) {
                keys.push(key.to_string());
            }
        }
    }
    if let Some(roles) = role_specs(data) {
        for spec in roles.values() {
            if let Some(key) = spec.get("model").and_then(Value::as_str) {
                keys.push(key.to_string());
            }
            if let Some(fallbacks) = spec.get("fallback").and_then(Value::as_array) {
                for fallback in fallbacks {
                    if let Some(key) = fallback.get("model").and_then(Value::as_str) {
                        keys.push(key.to_string());
                    }
                }
            }
        }
    }
    if let Some(small) = data
        .get("routing")
        .and_then(|routing| routing.get("small_model"))
        .and_then(Value::as_str)
    {
        keys.push(small.to_string());
    }
    keys
}

/// Providers used by the resolved routing, in declaration order. Undeclared
/// but used providers are appended so nothing silently disappears.
pub fn enabled_provider_order(data: &Value) -> Vec<String> {
    let mut used: Vec<String> = Vec::new();
    for key in referenced_model_keys(data) {
        let Ok((provider, _)) = model_full_id(data, &key) else {
            continue;
        };
        if !used.contains(&provider) {
            used.push(provider);
        }
    }
    let mut ordered: Vec<String> = Vec::new();
    if let Some(declared) = providers(data) {
        for name in declared.keys() {
            if used.contains(name) && !ordered.contains(name) {
                ordered.push(name.clone());
            }
        }
    }
    for name in &used {
        if !ordered.contains(name) {
            ordered.push(name.clone());
        }
    }
    ordered
}

/// `(role, agent, provider label, provider/model)` rows for reporting.
pub fn routing_rows(data: &Value) -> Result<Vec<(String, String, String, String)>> {
    let mut rows = Vec::new();
    if let Some(roles) = role_specs(data) {
        for (role, spec) in roles {
            let key = spec.get("model").and_then(Value::as_str).ok_or_else(|| {
                GearError::config(format!("routing role '{role}' is missing 'model'"))
            })?;
            let (provider, full) = model_full_id(data, key)?;
            rows.push((
                role.clone(),
                consumer_agent_id(role),
                provider_label(data, &provider),
                full,
            ));
        }
    }
    Ok(rows)
}

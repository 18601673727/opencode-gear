//! Deterministic OpenCode config generation.

use crate::config::Effective;
use crate::defaults::LEAD_ROLE;
use crate::error::{GearError, Result};
use crate::json::{deep_merge, is_truthy};
use crate::model;
use crate::prompt;
use crate::validate;
use serde_json::{json, Map, Value};

/// Build the OpenCode config for one throttle level using the v1 contract.
///
/// The result is validated, deterministic and independent of the current
/// environment: the same inputs always produce the same JSON. Callers that
/// already detected the runtime use [`build_opencode_config_for`] so the
/// generated plugin array, local plugin URI and delegation permission key match
/// the runtime family.
pub fn build_opencode_config(effective: &Effective, level: &str) -> Result<Value> {
    build_opencode_config_for(effective, level, crate::runtime::compat::v1_adapter())
}

/// Build the OpenCode config for one throttle level against a runtime adapter.
///
/// No version conditional lives here: the adapter supplies the plugin key, the
/// delegation permission key and the canonical local plugin URI.
pub fn build_opencode_config_for(
    effective: &Effective,
    level: &str,
    adapter: &dyn crate::runtime::compat::RuntimeAdapter,
) -> Result<Value> {
    validate::require_valid(effective)?;
    let data = &effective.data;

    let empty = Map::new();
    let levels = data
        .get("throttle")
        .and_then(|throttle| throttle.get("levels"))
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    if !levels.contains_key(level) {
        let available = levels.keys().cloned().collect::<Vec<_>>().join(", ");
        return Err(GearError::config(format!(
            "unknown throttle level: '{level}' (available: {available})"
        )));
    }

    let roles = model::role_specs(data).cloned().unwrap_or_default();
    let empty_permissions = Map::new();
    let permissions = data
        .get("permissions")
        .and_then(Value::as_object)
        .unwrap_or(&empty_permissions);
    let subagent = permissions
        .get("subagent")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let subagent_permission = subagent
        .get("permission")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let profiles = permissions
        .get("profiles")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let role_profiles = permissions
        .get("role_profiles")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let lead_permission = permissions
        .get("lead")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    let consumer_agents: Vec<(String, String)> = roles
        .keys()
        .map(|role| (role.clone(), model::consumer_agent_id(role)))
        .collect();

    let mut agent_config = Map::new();

    // Lead agents: one per throttle level so the TUI can cycle them live.
    let lead_template = prompt::read_prompt(effective, LEAD_ROLE)?.1;
    for (lead_level, spec) in levels {
        let model_key = spec.get("model").and_then(Value::as_str).ok_or_else(|| {
            GearError::config(format!("throttle level '{lead_level}' is missing 'model'"))
        })?;
        let (_, full) = model::model_full_id(data, model_key)?;
        let temperature = lead_permission
            .get("temperature")
            .cloned()
            .unwrap_or_else(|| json!(0.1));
        let rendered = prompt::render_lead_prompt(effective, lead_level, &lead_template)?;

        let mut task = Map::new();
        task.insert(
            "*".to_string(),
            lead_permission
                .get("task_default")
                .cloned()
                .unwrap_or_else(|| json!("deny")),
        );
        for (_, agent) in &consumer_agents {
            task.insert(agent.clone(), json!("allow"));
        }

        let mut lead_permissions = Map::new();
        lead_permissions.insert(adapter.task_key().to_string(), Value::Object(task));

        let mut lead_spec = Map::new();
        lead_spec.insert("mode".to_string(), json!("primary"));
        lead_spec.insert("model".to_string(), json!(full));
        lead_spec.insert("temperature".to_string(), temperature);
        lead_spec.insert("prompt".to_string(), json!(rendered));
        lead_spec.insert("permission".to_string(), Value::Object(lead_permissions));
        if let Some(variant) = spec
            .get("variant")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            lead_spec.insert("variant".to_string(), json!(variant));
        }
        agent_config.insert(model::lead_agent_id(lead_level), Value::Object(lead_spec));
    }

    // Consumer agents: one per role, independent of the throttle.
    for (role, spec) in &roles {
        let model_key = spec.get("model").and_then(Value::as_str).ok_or_else(|| {
            GearError::config(format!("routing role '{role}' is missing 'model'"))
        })?;
        let (_, full) = model::model_full_id(data, model_key)?;
        let (meta, body) = prompt::read_prompt(effective, role)?;

        let profile = role_profiles
            .get(role)
            .and_then(Value::as_str)
            .and_then(|name| profiles.get(name))
            .cloned()
            .unwrap_or_else(|| json!({}));
        let permission = with_task_key(
            deep_merge(&subagent_permission, &profile),
            adapter.task_key(),
        );

        let description = spec
            .get("description")
            .filter(|value| is_truthy(value))
            .cloned()
            .or_else(|| meta.get("description").map(|value| json!(value)))
            .unwrap_or_else(|| json!(role));

        let hidden = subagent.get("hidden").map(is_truthy).unwrap_or(true);

        let mut consumer_spec = Map::new();
        consumer_spec.insert("mode".to_string(), json!("subagent"));
        consumer_spec.insert("model".to_string(), json!(full));
        consumer_spec.insert("description".to_string(), description);
        consumer_spec.insert("prompt".to_string(), json!(body));
        consumer_spec.insert("hidden".to_string(), json!(hidden));
        consumer_spec.insert("permission".to_string(), permission);
        if let Some(temperature) = meta.get("temperature") {
            let parsed: f64 = temperature.parse().map_err(|_| {
                GearError::config(format!("{role} prompt temperature is not a number"))
            })?;
            consumer_spec.insert("temperature".to_string(), json!(parsed));
        }
        if let Some(variant) = spec
            .get("variant")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            consumer_spec.insert("variant".to_string(), json!(variant));
        }
        agent_config.insert(model::consumer_agent_id(role), Value::Object(consumer_spec));
    }

    let lead_key = levels
        .get(level)
        .and_then(|spec| spec.get("model"))
        .and_then(Value::as_str)
        .ok_or_else(|| GearError::config(format!("unknown throttle level: '{level}'")))?;

    let mut result = Map::new();
    result.insert(
        "default_agent".to_string(),
        json!(model::lead_agent_id(level)),
    );
    result.insert(
        "model".to_string(),
        json!(model::model_full_id(data, lead_key)?.1),
    );
    result.insert(
        "enabled_providers".to_string(),
        json!(model::enabled_provider_order(data)),
    );
    result.insert("agent".to_string(), Value::Object(agent_config));
    if let Some(small) = data
        .get("routing")
        .and_then(|routing| routing.get("small_model"))
        .and_then(Value::as_str)
    {
        result.insert(
            "small_model".to_string(),
            json!(model::model_full_id(data, small)?.1),
        );
    }

    let base = data.get("base").cloned().unwrap_or_else(|| json!({}));
    let mut merged = deep_merge(&base, &Value::Object(result));
    if let Some(extra) = data.get("opencode").filter(|value| value.is_object()) {
        merged = deep_merge(&merged, extra);
    }

    // The generated adapter enforces the selected Lead request contract in
    // addition to optional dynamic orchestration. Preserve the explicit
    // no-hook escape hatch: disabled orchestration emits no OCG plugin.
    if crate::orchestration::OrchestrationConfig::from_config(data)?.enabled {
        if let Some(uri) = adapter.local_plugin_uri(&effective.cwd)? {
            crate::orchestration::plugin::inject_plugin_for(
                &mut merged,
                adapter.plugin_key(),
                &uri,
            );
        }
    }
    Ok(merged)
}

/// Rename the top-level `task` permission key to the runtime's delegation key.
///
/// OpenCode 1 uses `task`; OpenCode 2 renamed the tool to `subagent`. The
/// profiles and defaults stay written once, in the project-agnostic v1 shape.
fn with_task_key(permission: Value, task_key: &str) -> Value {
    if task_key == "task" {
        return permission;
    }
    let Value::Object(mut map) = permission else {
        return permission;
    };
    if let Some(task) = map.remove("task") {
        map.insert(task_key.to_string(), task);
    }
    Value::Object(map)
}

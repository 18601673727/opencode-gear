//! Human-readable reporting for `status`, `routing` and `layers`.

use crate::config::Effective;
use crate::error::{GearError, Result};
use crate::model;
use crate::observability;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// `(level, "provider/model", variant|"provider-default")` rows.
pub fn throttle_rows(effective: &Effective) -> Result<Vec<(String, String, String)>> {
    let mut rows = Vec::new();
    if let Some(levels) = effective
        .data
        .get("throttle")
        .and_then(|throttle| throttle.get("levels"))
        .and_then(Value::as_object)
    {
        for (level, spec) in levels {
            let key = spec.get("model").and_then(Value::as_str).ok_or_else(|| {
                GearError::config(format!("throttle level '{level}' is missing 'model'"))
            })?;
            let (_, full) = model::model_full_id(&effective.data, key)?;
            let variant = spec
                .get("variant")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .unwrap_or("provider-default")
                .to_string();
            rows.push((level.clone(), full, variant));
        }
    }
    Ok(rows)
}

pub fn throttle_text(effective: &Effective) -> Result<String> {
    let mut lines = vec!["Throttle (Execution Tier):".to_string(), String::new()];
    for (level, full, variant) in throttle_rows(effective)? {
        lines.push(format!("  {level:<6} {full:<24} {variant}"));
    }
    let default = effective
        .data
        .get("throttle")
        .and_then(|throttle| throttle.get("default"))
        .and_then(Value::as_str)
        .unwrap_or("");
    lines.push(String::new());
    lines.push(format!("  default: {default}"));
    Ok(lines.join("\n"))
}

pub fn routing_text(effective: &Effective) -> Result<String> {
    let mut lines = vec!["Worker router (role -> model):".to_string(), String::new()];
    for (role, agent, provider, full) in model::routing_rows(&effective.data)? {
        lines.push(format!("  {role:<13} {agent:<18} {provider:<24} {full}"));
    }
    if let Some(small) = effective
        .data
        .get("routing")
        .and_then(|routing| routing.get("small_model"))
        .and_then(Value::as_str)
    {
        let (provider, full) = model::model_full_id(&effective.data, small)?;
        lines.push(String::new());
        lines.push(format!(
            "  small_model   {} -> {}",
            model::provider_label(&effective.data, &provider),
            full
        ));
    }
    Ok(lines.join("\n"))
}

fn layer_mark(is_applied: bool) -> &'static str {
    if is_applied {
        "applied"
    } else {
        "not found"
    }
}

pub fn status_text(effective: &Effective, level: &str) -> Result<String> {
    let lead_key = effective
        .data
        .get("throttle")
        .and_then(|throttle| throttle.get("levels"))
        .and_then(|levels| levels.get(level))
        .and_then(|spec| spec.get("model"))
        .and_then(Value::as_str)
        .ok_or_else(|| GearError::config(format!("unknown throttle level: '{level}'")))?;
    let lead = model::model_full_id(&effective.data, lead_key)?.1;
    // A missing variant is reported as the provider default, never as a
    // fabricated value.
    let lead_variant = effective
        .data
        .get("throttle")
        .and_then(|throttle| throttle.get("levels"))
        .and_then(|levels| levels.get(level))
        .and_then(|spec| spec.get("variant"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or("provider-default");
    let workers = model::role_specs(&effective.data)
        .map(|roles| roles.len())
        .unwrap_or(0);
    let providers = model::enabled_provider_order(&effective.data);
    let provider_text = if providers.is_empty() {
        "(none)".to_string()
    } else {
        providers.join(", ")
    };

    let mut lines = vec![
        "OpenCode Gear".to_string(),
        String::new(),
        format!("  throttle      {level}"),
        format!("  default agent {}", model::lead_agent_id(level)),
        format!("  lead model    {lead}"),
        format!("  lead variant  {lead_variant}"),
        format!("  workers       {workers}"),
        format!("  providers     {provider_text}"),
        format!("  cwd           {}", effective.cwd.display()),
        String::new(),
        "Config layers:".to_string(),
    ];

    let applied: Vec<&PathBuf> = effective.applied.iter().map(|(_, path)| path).collect();
    for (name, path) in [
        ("user", &effective.user_path),
        ("project", &effective.project_path),
    ] {
        let marked = applied.contains(&path);
        lines.push(format!(
            "  {name:<8} {}  [{}]",
            path.display(),
            layer_mark(marked)
        ));
    }

    lines.push(String::new());
    lines.push(throttle_text(effective)?);
    lines.push(String::new());
    lines.push(routing_text(effective)?);
    Ok(lines.join("\n"))
}

/// `layers` output. `env_trace` is the environment-provided trace path.
pub fn layers_text(effective: &Effective, env_trace: Option<&Path>) -> String {
    let home = effective
        .gear_home
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "(embedded)".to_string());
    let mut lines = vec![format!("gear home: {home}")];
    for (name, path) in [
        ("user", &effective.user_path),
        ("project", &effective.project_path),
    ] {
        let found = if path.is_file() { "found" } else { "not found" };
        lines.push(format!("{name:<8} {}  [{found}]", path.display()));
    }
    let trace = observability::trace_path(effective, env_trace)
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "(disabled)".to_string());
    lines.push(format!("{:<8} {trace}", "trace"));
    lines.join("\n")
}

//! Layered configuration: defaults -> user -> project -> CLI/environment.

use crate::error::{GearError, Result};
use crate::json::deep_merge;
use crate::yaml::read_yaml_object;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The fully resolved configuration plus the paths and metadata needed to
/// report on it.
#[derive(Debug, Clone)]
pub struct Effective {
    /// Merged registries (throttle, models, routing, permissions, base,
    /// prompts, observability).
    pub data: Value,
    /// The disk gear home, when defaults were loaded from disk.
    pub gear_home: Option<PathBuf>,
    /// Directory used to resolve project overrides and launch the child process.
    pub cwd: PathBuf,
    /// User home, used for `~` expansion and the default user config path.
    pub home_dir: Option<PathBuf>,
    /// Resolved user override path (may not exist).
    pub user_path: PathBuf,
    /// Resolved project override path (may not exist).
    pub project_path: PathBuf,
    /// Layers that were found and merged, in application order.
    pub applied: Vec<(&'static str, PathBuf)>,
}

/// Expand a leading `~` using the supplied home directory.
pub fn expand_tilde(raw: &str, home_dir: Option<&Path>) -> PathBuf {
    if raw == "~" {
        if let Some(home) = home_dir {
            return home.to_path_buf();
        }
        return PathBuf::from(raw);
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(home) = home_dir {
            return home.join(rest);
        }
    }
    PathBuf::from(raw)
}

fn xdg_config_home(home_dir: Option<&Path>, explicit: Option<&Path>) -> PathBuf {
    if let Some(dir) = explicit {
        return dir.to_path_buf();
    }
    match home_dir {
        Some(home) => home.join(".config"),
        None => PathBuf::from(".config"),
    }
}

/// Resolve the user override path. An explicit flag beats the environment,
/// which beats the default under `$XDG_CONFIG_HOME`.
pub fn user_config_path(
    explicit: Option<&Path>,
    env_path: Option<&Path>,
    xdg: Option<&Path>,
    home_dir: Option<&Path>,
) -> PathBuf {
    if let Some(raw) = explicit.or(env_path) {
        return expand_tilde(&raw.to_string_lossy(), home_dir);
    }
    xdg_config_home(home_dir, xdg)
        .join("opencode-gear")
        .join("config.yaml")
}

/// Resolve the project override path. An explicit flag beats the environment,
/// which beats `<cwd>/.opencode-gear.yaml`.
pub fn project_config_path(
    cwd: &Path,
    explicit: Option<&Path>,
    env_path: Option<&Path>,
    home_dir: Option<&Path>,
) -> PathBuf {
    if let Some(raw) = explicit.or(env_path) {
        return expand_tilde(&raw.to_string_lossy(), home_dir);
    }
    cwd.join(".opencode-gear.yaml")
}

/// Reject a legacy JSON override instead of silently ignoring or migrating it.
///
/// `path` is the resolved layer path. Two shapes are rejected:
///
/// * the resolved path itself is an existing `.json` file, and
/// * an existing JSON sibling next to the YAML path (for example
///   `~/.config/opencode-gear/config.json` next to `config.yaml`).
///
/// A non-existent explicit `.json` path is left alone so previous "missing
/// override" behavior is preserved.
pub fn reject_stale_json(path: &Path, label: &str) -> Result<()> {
    let is_json = path
        .extension()
        .map(|extension| extension.eq_ignore_ascii_case("json"))
        .unwrap_or(false);
    if is_json {
        if path.is_file() {
            let target = path.with_extension("yaml");
            return Err(GearError::config(format!(
                "unsupported JSON {label} file: {}\nOpenCode Gear reads YAML only; rename it to {} and convert its contents.",
                path.display(),
                target.display()
            )));
        }
        return Ok(());
    }
    let legacy = path.with_extension("json");
    if legacy != path && legacy.is_file() {
        return Err(GearError::config(format!(
            "unsupported JSON {label} file: {}\nOpenCode Gear reads YAML only; remove it or convert it to {}. Existing JSON is never migrated or merged.",
            legacy.display(),
            path.display()
        )));
    }
    Ok(())
}

/// Deep-merge the user and project overrides onto the defaults.
pub fn build_effective(
    defaults: Value,
    gear_home: Option<PathBuf>,
    cwd: &Path,
    user_path: &Path,
    project_path: &Path,
    home_dir: Option<PathBuf>,
) -> Result<Effective> {
    let mut data = defaults;
    let mut applied = Vec::new();
    let layers: [(&'static str, &Path); 2] = [("user", user_path), ("project", project_path)];
    for (name, path) in layers {
        reject_stale_json(path, name)?;
        if path.is_file() {
            let overlay = read_yaml_object(path)?;
            let prior = data.clone();
            data = deep_merge(&data, &overlay);
            normalize_model_variant_overrides(&prior, &mut data, &overlay);
            applied.push((name, path.to_path_buf()));
        }
    }
    Ok(Effective {
        data,
        gear_home,
        cwd: cwd.to_path_buf(),
        home_dir,
        user_path: user_path.to_path_buf(),
        project_path: project_path.to_path_buf(),
        applied,
    })
}

/// A variant belongs to a model, not to a route name. Deep merge deliberately
/// preserves omitted scalars, but a model replacement must not retain the old
/// model's variant. Omitted variants still inherit when the model is unchanged;
/// an explicit null remains provider-default.
fn normalize_model_variant_overrides(prior: &Value, merged: &mut Value, overlay: &Value) {
    for (section, routes_key) in [("throttle", "levels"), ("routing", "roles")] {
        let Some(overrides) = overlay
            .get(section)
            .and_then(|value| value.get(routes_key))
            .and_then(Value::as_object)
        else {
            continue;
        };
        let Some(routes) = merged
            .get_mut(section)
            .and_then(|value| value.get_mut(routes_key))
            .and_then(Value::as_object_mut)
        else {
            continue;
        };
        for (name, route_override) in overrides {
            let Some(route_override) = route_override.as_object() else {
                continue;
            };
            if route_override.contains_key("model") && !route_override.contains_key("variant") {
                let old_model = prior
                    .get(section)
                    .and_then(|value| value.get(routes_key))
                    .and_then(|value| value.get(name))
                    .and_then(|value| value.get("model"));
                let new_model = route_override.get("model");
                if old_model != new_model {
                    if let Some(route) = routes.get_mut(name).and_then(Value::as_object_mut) {
                        route.remove("variant");
                    }
                }
            }
        }
    }
}

//! Layered configuration: defaults -> user -> project -> CLI/environment.

use crate::error::Result;
use crate::json::{deep_merge, read_json_object};
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
        .join("config.json")
}

/// Resolve the project override path. An explicit flag beats the environment,
/// which beats `<cwd>/.opencode-gear.json`.
pub fn project_config_path(
    cwd: &Path,
    explicit: Option<&Path>,
    env_path: Option<&Path>,
    home_dir: Option<&Path>,
) -> PathBuf {
    if let Some(raw) = explicit.or(env_path) {
        return expand_tilde(&raw.to_string_lossy(), home_dir);
    }
    cwd.join(".opencode-gear.json")
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
        if path.is_file() {
            let overlay = read_json_object(path)?;
            data = deep_merge(&data, &overlay);
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

//! Generated OpenCode plugin adapter.
//!
//! OpenCode's supported extension mechanism is a local JavaScript plugin. Gear
//! generates a *thin* adapter under the project's ignored state directory and
//! injects its `file://` URL into the generated config's `plugin` array. The
//! adapter does no ranking and no policy: every decision is made by the Rust
//! bridge it spawns (`ocg __bridge ...`) with a direct argv and stdin, never a
//! shell.
//!
//! The adapter is deliberately small and boring:
//!
//! - `chat.message` appends a delimited dynamic-context suffix to the existing
//!   text part (never replacing the user's prompt);
//! - `tool.execute.before` appends the role hand-off to a `task` prompt;
//! - `tool.execute.after` appends verification feedback to a `task` result;
//! - a delimiter guard makes the append idempotent if a hook fires twice;
//! - every bridge failure is swallowed so a broken bridge can never break the
//!   session.

use crate::error::{GearError, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The plugin directory under `.opencode-gear/orchestration/`.
pub const PLUGIN_DIR: &str = "plugin";
/// The generated adapter file name.
pub const PLUGIN_FILE: &str = "ocg-orchestration.js";
/// The stable start delimiter of the dynamic context suffix.
pub const CONTEXT_START: &str = "<<<OCG:DYNAMIC_CONTEXT v1>>>";
/// The stable end delimiter of the dynamic context suffix.
pub const CONTEXT_END: &str = "<<<OCG:END>>>";

/// The plugin path for a project root.
pub fn plugin_path(root: &Path) -> PathBuf {
    crate::orchestration::state::state_dir(root)
        .join(PLUGIN_DIR)
        .join(PLUGIN_FILE)
}

/// The `file://` URL injected into the OpenCode config.
pub fn plugin_uri(root: &Path) -> Option<String> {
    let path = plugin_path(root);
    let absolute = path
        .canonicalize()
        .unwrap_or_else(|_| absolutize(root).join(relative_from_state()));
    Some(format!("file://{}", absolute.to_string_lossy()))
}

fn relative_from_state() -> PathBuf {
    Path::new(crate::context::repomap::GEAR_DIR)
        .join(crate::orchestration::state::ORCHESTRATION_DIR)
        .join(PLUGIN_DIR)
        .join(PLUGIN_FILE)
}

fn absolutize(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .unwrap_or_else(|_| path.to_path_buf())
}

/// Whether the generated adapter exists on disk.
pub fn is_installed(root: &Path) -> bool {
    plugin_path(root).is_file()
}

/// Materialize the generated adapter. Writes only under ignored local state.
pub fn materialize(root: &Path) -> Result<PathBuf> {
    crate::runtime::install::ensure_gitignore(root)?;
    let path = plugin_path(root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| GearError::io(format!("cannot create {}", parent.display()), error))?;
    }
    std::fs::write(&path, plugin_source()).map_err(|error| GearError::write(&path, error))?;
    Ok(path)
}

/// Inject the plugin URL into a config, preserving every existing plugin entry
/// and never adding a duplicate.
pub fn inject_plugin(config: &mut Value, uri: &str) {
    let Some(object) = config.as_object_mut() else {
        return;
    };
    let mut entries: Vec<Value> = match object.get("plugin") {
        Some(Value::Array(values)) => values
            .iter()
            .filter(|value| !entry_is_uri(value, uri))
            .cloned()
            .collect(),
        _ => Vec::new(),
    };
    entries.push(Value::String(uri.to_string()));
    object.insert("plugin".to_string(), Value::Array(entries));
}

/// Whether one configured plugin entry already names `uri`.
fn entry_is_uri(value: &Value, uri: &str) -> bool {
    match value {
        Value::String(text) => text == uri,
        Value::Array(items) => items.first().and_then(Value::as_str) == Some(uri),
        _ => false,
    }
}

/// Whether a config already contains an OCG plugin entry (used by diagnostics
/// and by tests that assert disabled orchestration emits nothing).
pub fn has_ocg_plugin(config: &Value) -> bool {
    config
        .get("plugin")
        .and_then(Value::as_array)
        .map(|entries| entries.iter().any(entry_is_ocg))
        .unwrap_or(false)
}

/// Remove the generated OCG plugin entry while preserving every user plugin.
/// Used for non-coding sessions (`ocg models`) that must not depend on the
/// adapter being materialized.
pub fn remove_ocg_plugin(config: &mut Value) {
    let Some(object) = config.as_object_mut() else {
        return;
    };
    let Some(Value::Array(entries)) = object.get_mut("plugin") else {
        return;
    };
    entries.retain(|entry| !entry_is_ocg(entry));
    if entries.is_empty() {
        object.remove("plugin");
    }
}

fn entry_is_ocg(value: &Value) -> bool {
    let text = match value {
        Value::String(text) => text.as_str(),
        Value::Array(items) => items.first().and_then(Value::as_str).unwrap_or(""),
        _ => "",
    };
    text.contains(PLUGIN_FILE)
}

/// The generated adapter source. Kept as one `const` so the bytes are stable
/// and reviewable.
pub fn plugin_source() -> &'static str {
    PLUGIN_SOURCE
}

const PLUGIN_SOURCE: &str = r#"// Generated by OpenCode Gear (ocg). Do not edit by hand.
// Thin adapter: all ranking, projection and policy live in the `ocg` Rust
// bridge. No shell is used; Bun.spawn receives a direct argv vector.
//
// Hooks:
//   chat.message          -> append dynamic context to the user text part
//   tool.execute.before   -> append the role hand-off to a `task` prompt
//   tool.execute.after    -> append verification feedback to a `task` result
//
// Every bridge failure is swallowed: a broken bridge must never break a
// session. `ocg __bridge` is expected on OPENCODE_GEAR_OCG (or PATH).

const START = "<<<OCG:DYNAMIC_CONTEXT v1>>>";
const END = "<<<OCG:END>>>";

function executable() {
  return process.env.OPENCODE_GEAR_OCG || "ocg";
}

function project() {
  return process.env.OPENCODE_GEAR_PROJECT || process.cwd();
}

async function bridge(event, payload) {
  try {
    const proc = Bun.spawn([executable(), "__bridge", event, "--project", project()], {
      stdin: "pipe",
      stdout: "pipe",
      stderr: "ignore",
      env: process.env,
    });
    try {
      proc.stdin.write(JSON.stringify(payload));
      proc.stdin.end();
    } catch (_) {
      // stdin may already be closed; the bridge still answers or fails soft.
    }
    const text = await new Response(proc.stdout).text();
    await proc.exited;
    if (!text) return null;
    return JSON.parse(text);
  } catch (_) {
    return null;
  }
}

function hasContext(text) {
  return typeof text === "string" && text.includes(START) && text.includes(END);
}

function suffix(context) {
  return "\n\n" + START + "\n" + context + "\n" + END + "\n";
}

function textParts(parts) {
  return (parts || []).filter(
    (part) => part && part.type === "text" && typeof part.text === "string",
  );
}

function appendToParts(parts, context) {
  const texts = textParts(parts);
  if (texts.length === 0) return;
  const target = texts[texts.length - 1];
  if (hasContext(target.text)) return;
  target.text = target.text + suffix(context);
}

function appendToPrompt(args, context) {
  if (!args || typeof args.prompt !== "string" || hasContext(args.prompt)) return;
  args.prompt = args.prompt + suffix(context);
}

function appendToOutput(output, context) {
  if (!output || typeof output.output !== "string" || hasContext(output.output)) return;
  output.output = output.output + suffix(context);
}

export const server = async (_input) => ({
  "chat.message": async (input, output) => {
    // Only the Lead owns the top-level dynamic context. A consumer subagent
    // session must not receive a second/duplicate Lead context block. The
    // task before/after hooks below stay active in every session.
    const agent = typeof input.agent === "string" ? input.agent : "";
    if (agent && !agent.startsWith("lead-")) return;
    const parts = output && output.parts ? output.parts : [];
    const text = textParts(parts)
      .map((part) => part.text)
      .join("\n");
    if (!text) return;
    const result = await bridge("chat.message", {
      session_id: input.sessionID,
      agent: input.agent,
      text,
    });
    if (result && result.context) appendToParts(parts, result.context);
  },
  "tool.execute.before": async (input, output) => {
    if (input.tool !== "task") return;
    const args = output && output.args ? output.args : {};
    const result = await bridge("tool.execute.before", {
      session_id: input.sessionID,
      tool: input.tool,
      args,
    });
    if (result && result.context) appendToPrompt(args, result.context);
  },
  "tool.execute.after": async (input, output) => {
    if (input.tool !== "task") return;
    const result = await bridge("tool.execute.after", {
      session_id: input.sessionID,
      tool: input.tool,
      args: input.args,
      result: output,
    });
    if (result && result.context) appendToOutput(output, result.context);
  },
});
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn source_is_shell_free_and_bridge_backed() {
        let source = plugin_source();
        assert!(source.contains("Bun.spawn"));
        assert!(source.contains("__bridge"));
        // The generated adapter must not route through a shell.
        assert!(!source.contains("child_process"));
        assert!(!source.contains("execSync"));
        assert!(!source.contains("sh -c"));
        assert!(source.contains(CONTEXT_START));
        assert!(source.contains(CONTEXT_END));
    }

    #[test]
    fn chat_message_only_targets_the_lead_but_task_hooks_stay_active() {
        let source = plugin_source();
        // The Lead guard is present for chat.message.
        assert!(
            source.contains("startsWith(\"lead-\")"),
            "chat.message must ignore consumer subagent sessions"
        );
        assert!(source.contains("if (agent && !agent.startsWith(\"lead-\")) return;"));
        // The task before/after hooks are still registered unconditionally.
        assert!(source.contains("\"tool.execute.before\": async"));
        assert!(source.contains("\"tool.execute.after\": async"));
    }

    #[test]
    fn materialize_writes_under_ignored_state() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_installed(dir.path()));
        let path = materialize(dir.path()).unwrap();
        assert!(path.is_file());
        assert!(is_installed(dir.path()));
        assert_eq!(
            std::fs::read_to_string(dir.path().join(".gitignore")).unwrap(),
            ".opencode-gear/\n"
        );
        let uri = plugin_uri(dir.path()).unwrap();
        assert!(uri.starts_with("file://"));
        assert!(uri.ends_with(PLUGIN_FILE));
    }

    #[test]
    fn injection_preserves_user_plugins_and_dedups() {
        let mut config = json!({
            "plugin": ["my-plugin", ["other", {"x": 1}]]
        });
        let uri = "file:///tmp/project/.opencode-gear/orchestration/plugin/ocg-orchestration.js";
        inject_plugin(&mut config, uri);
        let entries = config["plugin"].as_array().unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0], json!("my-plugin"));
        assert_eq!(entries[1], json!(["other", {"x": 1}]));
        assert_eq!(entries[2], json!(uri));
        // A second injection is idempotent.
        inject_plugin(&mut config, uri);
        assert_eq!(config["plugin"].as_array().unwrap().len(), 3);
        assert!(has_ocg_plugin(&config));
    }

    #[test]
    fn injection_creates_the_array_when_absent() {
        let mut config = json!({"model": "x"});
        inject_plugin(&mut config, "file:///tmp/ocg-orchestration.js");
        assert!(has_ocg_plugin(&config));
        assert_eq!(config["plugin"].as_array().unwrap().len(), 1);
    }
}

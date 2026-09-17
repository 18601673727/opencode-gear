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
//! - `chat.message` first enforces the Rust-resolved primary Lead
//!   agent/model/variant contract on the mutable user message, then optionally
//!   appends a delimited dynamic-context suffix (never replacing the prompt);
//! - `tool.execute.before` appends the role hand-off to a `task` prompt;
//! - `tool.execute.after` appends verification feedback to a `task` result;
//! - a delimiter guard makes the append idempotent if a hook fires twice;
//! - every bridge failure is swallowed so a broken bridge cannot break the
//!   session, while a Lead contract that cannot be enforced fails loudly before
//!   an incorrect provider request can be sent.

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
//   chat.message          -> enforce Lead contract; append dynamic context
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

function orchestrationEnabled() {
  return process.env.OPENCODE_GEAR_ORCHESTRATION_ENABLED === "1";
}

function leadContract() {
  const raw = process.env.OPENCODE_GEAR_LEAD_CONTRACT;
  if (!raw) throw new Error("OCG Lead contract is missing");
  let value;
  try {
    value = JSON.parse(raw);
  } catch (_) {
    throw new Error("OCG Lead contract is invalid");
  }
  for (const key of ["agent", "provider_id", "model_id", "variant"]) {
    if (typeof value[key] !== "string" || value[key].length === 0) {
      throw new Error("OCG Lead contract is incomplete");
    }
  }
  return value;
}

function agentOf(value) {
  return value && typeof value.agent === "string" ? value.agent : "";
}

// Only a positively identified OCG Lead session is rewritten. OpenCode always
// resolves an agent before `chat.message` (the selected agent for the primary
// session, the subagent name for a `task` child), so a consumer request can
// never be mistaken for the Lead. A request whose agent cannot be established
// is left untouched rather than silently re-routed onto a Lead model.
function isPrimaryLead(input, output) {
  const selected = agentOf(input) || agentOf(output && output.message);
  return selected.startsWith("lead-");
}

function enforceLeadContract(input, output) {
  if (!isPrimaryLead(input, output)) return null;
  const contract = leadContract();
  if (!output || !output.message) {
    throw new Error("OpenCode did not expose a mutable Lead request");
  }
  output.message.agent = contract.agent;
  output.message.model = {
    providerID: contract.provider_id,
    modelID: contract.model_id,
    variant: contract.variant,
  };
  const actual = output.message.model;
  if (
    output.message.agent !== contract.agent ||
    !actual ||
    actual.providerID !== contract.provider_id ||
    actual.modelID !== contract.model_id ||
    actual.variant !== contract.variant
  ) {
    throw new Error("OpenCode rejected the OCG Lead request contract");
  }
  return contract;
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
    // Observed OpenCode 1.18.x request lifecycle (supported plugin surface):
    //   SessionPrompt.createUserMessage builds the user message `j`
    //   (agent + model{providerID,modelID,variant}, where an explicit/selected
    //   variant wins over the agent variant), then triggers `chat.message`
    //   with {message: j, parts}, then persists `j` via updateMessage(j).
    //   SessionPrompt.run later reads that persisted message and derives the
    //   provider request from `j.agent` and `j.model.variant`.
    // Writing the contract onto the mutable `output.message` therefore decides
    // the actual request and overrides sticky per-model/session UI state,
    // without mutating OpenCode's global saved variant.
    const contract = enforceLeadContract(input, output);
    // Only the Lead owns the top-level dynamic context. A consumer subagent
    // session must not receive a second/duplicate Lead context block. The
    // task before/after hooks below stay active in every session.
    const agent = contract ? contract.agent : (typeof input.agent === "string" ? input.agent : "");
    if (agent && !agent.startsWith("lead-")) return;
    if (!orchestrationEnabled()) return;
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

    /// The generated plugin runs under an embedded JavaScript runtime; these
    /// tests execute it under `node`. Node is the only optional tool here, so a
    /// missing node skips the execution tests — but never silently in a
    /// controlled environment that sets `OPENCODE_GEAR_REQUIRE_JS_TESTS=1`.
    fn node_or_skip(test: &str) -> bool {
        let available = std::process::Command::new("node")
            .arg("--version")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false);
        if available {
            return true;
        }
        assert_ne!(
            std::env::var("OPENCODE_GEAR_REQUIRE_JS_TESTS").as_deref(),
            Ok("1"),
            "node is required for the JavaScript plugin contract tests ({test}) but was not found"
        );
        eprintln!("skipping JavaScript contract execution ({test}): node is unavailable");
        false
    }

    fn write_plugin(dir: &tempfile::TempDir, script: &str) {
        std::fs::write(dir.path().join("plugin.mjs"), plugin_source()).unwrap();
        std::fs::write(dir.path().join("check.mjs"), script).unwrap();
    }

    fn run_plugin_script(dir: &tempfile::TempDir) -> Value {
        let output = std::process::Command::new("node")
            .arg("check.mjs")
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

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
        assert!(source.contains("output.message.agent = contract.agent"));
        assert!(source.contains("providerID: contract.provider_id"));
        assert!(source.contains("modelID: contract.model_id"));
        assert!(source.contains("variant: contract.variant"));
    }

    #[test]
    fn chat_message_enforces_sticky_lead_state_and_preserves_consumers_when_node_is_available() {
        if !node_or_skip("chat_message_enforces_sticky_lead_state") {
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        write_plugin(
            &dir,
            r#"import { server } from "./plugin.mjs";
const hooks = await server({});
// The exact contracts OpenCode Gear exports for each throttle level.
const CONTRACTS = {
  low: {agent: "lead-low", provider_id: "openai", model_id: "gpt-5.6-sol", variant: "medium"},
  mid: {agent: "lead-mid", provider_id: "openai", model_id: "gpt-5.6-sol", variant: "high"},
  high: {agent: "lead-high", provider_id: "openai", model_id: "gpt-6-astra", variant: "high"},
};
// Mirrors upstream createUserMessage: an explicitly selected input variant wins
// over the agent variant, and a reused session keeps its sticky agent/model.
async function check(level, inputAgent, outputAgent, providerID, modelID, variant) {
  process.env.OPENCODE_GEAR_LEAD_CONTRACT = JSON.stringify(CONTRACTS[level]);
  process.env.OPENCODE_GEAR_ORCHESTRATION_ENABLED = "0";
  const output = {message: {agent: outputAgent, model: {providerID, modelID, variant}}, parts: []};
  await hooks["chat.message"]({sessionID: "session", agent: inputAgent}, output);
  return output.message;
}
const lead = {
  // Previous mid use left lead-mid sticky; low must still win.
  low: await check("low", "lead-mid", "lead-mid", "openai", "gpt-5.6-sol", "high"),
  // Sticky Sol medium must not satisfy mid, which needs Sol high.
  mid: await check("mid", "lead-low", "lead-low", "openai", "gpt-5.6-sol", "medium"),
  // A stale UI variant selection must not satisfy high, which needs Astra high.
  high: await check("high", "lead-low", "lead-low", "openai", "gpt-5.6-sol", "high"),
};
// A reused session whose sticky state is a different Lead tier must be corrected.
const reused = await check("high", "lead-low", "lead-low", "openai", "gpt-5.6-sol", "medium");
// Consumers keep their own configured route at every throttle level.
const consumer = {
  low: await check("low", "ocg-build", "ocg-build", "opencode-go", "deepseek-v4.1-flash", "high"),
  mid: await check("mid", "ocg-explore", "ocg-explore", "volcengine-coding", "kimi-k2.7-code", "default"),
  high: await check("high", "ocg-verify", "ocg-verify", "opencode-go", "glm-5.3-flash", "high"),
};
// A request whose agent cannot be established must be left alone rather than
// silently re-routed onto a Lead model.
const unknown = await check("high", "", "", "opencode-go", "deepseek-v4.1-flash", "high");
console.log(JSON.stringify({lead, reused, consumer, unknown}));
"#,
        );
        let value = run_plugin_script(&dir);
        // `ocg low` -> lead-low / Sol / medium, even with sticky mid state.
        assert_eq!(value["lead"]["low"]["agent"], json!("lead-low"));
        assert_eq!(
            value["lead"]["low"]["model"]["modelID"],
            json!("gpt-5.6-sol")
        );
        assert_eq!(value["lead"]["low"]["model"]["variant"], json!("medium"));
        // `ocg mid` -> lead-mid / Sol / high, even with sticky Sol medium.
        assert_eq!(value["lead"]["mid"]["agent"], json!("lead-mid"));
        assert_eq!(
            value["lead"]["mid"]["model"]["modelID"],
            json!("gpt-5.6-sol")
        );
        assert_eq!(value["lead"]["mid"]["model"]["variant"], json!("high"));
        // `ocg high` -> lead-high / Astra / high, never a Sol fallback.
        assert_eq!(value["lead"]["high"]["agent"], json!("lead-high"));
        assert_eq!(
            value["lead"]["high"]["model"]["modelID"],
            json!("gpt-6-astra")
        );
        assert_eq!(value["lead"]["high"]["model"]["variant"], json!("high"));
        // A reused session cannot silently override the explicit throttle.
        assert_eq!(value["reused"]["agent"], json!("lead-high"));
        assert_eq!(value["reused"]["model"]["modelID"], json!("gpt-6-astra"));
        assert_eq!(value["reused"]["model"]["variant"], json!("high"));
        // Consumer routing is unchanged for every throttle level.
        for (level, agent, model) in [
            ("low", "ocg-build", "deepseek-v4.1-flash"),
            ("mid", "ocg-explore", "kimi-k2.7-code"),
            ("high", "ocg-verify", "glm-5.3-flash"),
        ] {
            assert_eq!(value["consumer"][level]["agent"], json!(agent));
            assert_eq!(value["consumer"][level]["model"]["modelID"], json!(model));
        }
        // An unidentifiable agent is not a Lead and must not be rewritten.
        assert_eq!(value["unknown"]["agent"], json!(""));
        assert_eq!(
            value["unknown"]["model"]["modelID"],
            json!("deepseek-v4.1-flash")
        );
        assert_eq!(value["unknown"]["model"]["variant"], json!("high"));
    }

    #[test]
    fn lead_contract_survives_the_observed_upstream_save_then_request_path() {
        if !node_or_skip("lead_contract_survives_the_upstream_save_then_request_path") {
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        // A faithful reduction of the OpenCode 1.18.31 request lifecycle
        // observed in the installed runtime: `createUserMessage` builds the
        // user message (an explicitly selected input variant wins over the
        // agent variant), `chat.message` runs on that message object, the
        // object is then persisted, and `SessionPrompt.run` derives the actual
        // provider request from the persisted message. The test asserts the
        // request — not just the hook output — satisfies the throttle contract.
        write_plugin(
            &dir,
            r#"import { server } from "./plugin.mjs";
const hooks = await server({});
const CONTRACTS = {
  low: {agent: "lead-low", provider_id: "openai", model_id: "gpt-5.6-sol", variant: "medium"},
  mid: {agent: "lead-mid", provider_id: "openai", model_id: "gpt-5.6-sol", variant: "high"},
  high: {agent: "lead-high", provider_id: "openai", model_id: "gpt-6-astra", variant: "high"},
};
// OpenCode resolves an explicit/selected variant ahead of the agent variant.
function createUserMessage(level, selectedVariant) {
  const contract = CONTRACTS[level];
  const agentVariant = level === "low" ? "medium" : "high";
  const variant = selectedVariant ?? agentVariant;
  return {agent: contract.agent, model: {providerID: "openai", modelID: contract.model_id, variant}};
}
async function request(level, selectedVariant) {
  const output = {message: createUserMessage(level, selectedVariant), parts: []};
  process.env.OPENCODE_GEAR_LEAD_CONTRACT = JSON.stringify(CONTRACTS[level]);
  process.env.OPENCODE_GEAR_ORCHESTRATION_ENABLED = "0";
  await hooks["chat.message"]({sessionID: "session", agent: output.message.agent}, output);
  const saved = output.message; // SessionPrompt.updateMessage(j)
  // SessionPrompt.run derives the request from the saved message.
  return {agent: saved.agent, model: saved.model.modelID, variant: saved.model.variant};
}
const requests = {
  low_sticky_high: await request("low", "high"),
  mid_sticky_medium: await request("mid", "medium"),
  high_sticky_medium: await request("high", "medium"),
  low_no_selection: await request("low", null),
  mid_no_selection: await request("mid", null),
  high_no_selection: await request("high", null),
};
console.log(JSON.stringify(requests));
"#,
        );
        let value = run_plugin_script(&dir);
        for (name, agent, model, variant) in [
            ("low_sticky_high", "lead-low", "gpt-5.6-sol", "medium"),
            ("mid_sticky_medium", "lead-mid", "gpt-5.6-sol", "high"),
            ("high_sticky_medium", "lead-high", "gpt-6-astra", "high"),
            ("low_no_selection", "lead-low", "gpt-5.6-sol", "medium"),
            ("mid_no_selection", "lead-mid", "gpt-5.6-sol", "high"),
            ("high_no_selection", "lead-high", "gpt-6-astra", "high"),
        ] {
            assert_eq!(value[name]["agent"], json!(agent), "{name}");
            assert_eq!(value[name]["model"], json!(model), "{name}");
            assert_eq!(value[name]["variant"], json!(variant), "{name}");
        }
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

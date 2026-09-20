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

/// OpenCode 2 discovers local plugins from the `plugins/` child of its custom
/// config directory. Keep it inside OCG's ignored state rather than writing an
/// untracked `.opencode/plugins` file into the project.
pub fn v2_plugin_path(root: &Path) -> PathBuf {
    crate::orchestration::state::state_dir(root)
        .join("plugins")
        .join(PLUGIN_FILE)
}

/// The custom config directory which makes [`v2_plugin_path`] discoverable.
pub fn v2_config_dir(root: &Path) -> PathBuf {
    crate::orchestration::state::state_dir(root)
}

/// The `file://` URL injected into the OpenCode config.
pub fn plugin_uri(root: &Path) -> Option<String> {
    let path = plugin_path(root);
    let absolute = path
        .canonicalize()
        .unwrap_or_else(|_| absolutize(root).join(relative_from_state()));
    Some(format!("file://{}", absolute.to_string_lossy()))
}

/// The canonical, absolute `file://` URL for the generated adapter.
///
/// OpenCode 2 requires a canonical local plugin URI; a URI that could not be
/// made absolute is refused rather than silently passed through.
pub fn canonical_plugin_uri(root: &Path) -> Option<String> {
    let uri = plugin_uri(root)?;
    let absolute = uri
        .strip_prefix("file://")
        .map(Path::new)
        .map(Path::is_absolute)
        .unwrap_or(false);
    absolute.then_some(uri)
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

/// Materialize the v1 generated adapter. Writes only under ignored local state.
pub fn materialize(root: &Path) -> Result<PathBuf> {
    materialize_with(root, plugin_source())
}

/// Materialize a generated adapter from an explicit source. Writes only under
/// ignored local state.
pub fn materialize_with(root: &Path, source: &str) -> Result<PathBuf> {
    crate::runtime::install::ensure_gitignore(root)?;
    let path = plugin_path(root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| GearError::io(format!("cannot create {}", parent.display()), error))?;
    }
    std::fs::write(&path, source).map_err(|error| GearError::write(&path, error))?;
    Ok(path)
}

/// Materialize the adapter at the local-discovery path used by OpenCode 2.
pub fn materialize_v2_with(root: &Path, source: &str) -> Result<PathBuf> {
    crate::runtime::install::ensure_gitignore(root)?;
    let path = v2_plugin_path(root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| GearError::io(format!("cannot create {}", parent.display()), error))?;
    }
    std::fs::write(&path, source).map_err(|error| GearError::write(&path, error))?;
    Ok(path)
}

/// Inject the plugin URL under the given config array key (`plugin` for
/// OpenCode 1.x, `plugins` for OpenCode 2.x), preserving every existing entry
/// and never adding a duplicate.
pub fn inject_plugin_for(config: &mut Value, key: &str, uri: &str) {
    let Some(object) = config.as_object_mut() else {
        return;
    };
    let mut entries: Vec<Value> = match object.get(key) {
        Some(Value::Array(values)) => values
            .iter()
            .filter(|value| !entry_is_uri(value, uri))
            .cloned()
            .collect(),
        _ => Vec::new(),
    };
    entries.push(Value::String(uri.to_string()));
    object.insert(key.to_string(), Value::Array(entries));
}

/// Inject the plugin URL under the historical singular `plugin` key.
pub fn inject_plugin(config: &mut Value, uri: &str) {
    inject_plugin_for(config, "plugin", uri);
}

/// Whether one configured plugin entry already names `uri`.
fn entry_is_uri(value: &Value, uri: &str) -> bool {
    match value {
        Value::String(text) => text == uri,
        Value::Array(items) => items.first().and_then(Value::as_str) == Some(uri),
        _ => false,
    }
}

/// Whether a config already contains an OCG plugin entry under `key` (used by
/// diagnostics and by tests that assert disabled orchestration emits nothing).
pub fn has_ocg_plugin_for(config: &Value, key: &str) -> bool {
    config
        .get(key)
        .and_then(Value::as_array)
        .map(|entries| entries.iter().any(entry_is_ocg))
        .unwrap_or(false)
}

/// Whether a config already contains an OCG plugin entry under the historical
/// singular `plugin` key.
pub fn has_ocg_plugin(config: &Value) -> bool {
    has_ocg_plugin_for(config, "plugin")
}

/// Remove the generated OCG plugin entry from `key` while preserving every user
/// plugin. Used for non-coding sessions (`ocg models`) that must not depend on
/// the adapter being materialized.
pub fn remove_ocg_plugin_for(config: &mut Value, key: &str) {
    let Some(object) = config.as_object_mut() else {
        return;
    };
    let Some(Value::Array(entries)) = object.get_mut(key) else {
        return;
    };
    entries.retain(|entry| !entry_is_ocg(entry));
    if entries.is_empty() {
        object.remove(key);
    }
}

/// Remove the generated OCG plugin entry from the historical singular `plugin`
/// key.
pub fn remove_ocg_plugin(config: &mut Value) {
    remove_ocg_plugin_for(config, "plugin");
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
  for (const key of ["agent", "provider_id", "model_id"]) {
    if (typeof value[key] !== "string" || value[key].length === 0) {
      throw new Error("OCG Lead contract is incomplete");
    }
  }
  // A reasoning variant is optional and provider-specific. When the resolved
  // Lead declares none, it is absent (or null) and the request stays at the
  // provider default; it is never fabricated. A present variant must be a
  // non-empty string.
  if (value.variant !== undefined && value.variant !== null) {
    if (typeof value.variant !== "string" || value.variant.length === 0) {
      throw new Error("OCG Lead contract is incomplete");
    }
  } else {
    value.variant = undefined;
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
  // Assign a fresh model object so a stale sticky variant cannot survive a
  // provider-default contract: absent variant means no `variant` key at all.
  const model = {
    providerID: contract.provider_id,
    modelID: contract.model_id,
  };
  if (contract.variant !== undefined) {
    model.variant = contract.variant;
  }
  output.message.model = model;
  const actual = output.message.model;
  if (
    output.message.agent !== contract.agent ||
    !actual ||
    actual.providerID !== contract.provider_id ||
    actual.modelID !== contract.model_id ||
    (contract.variant !== undefined && actual.variant !== contract.variant)
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
    // A missing delivery (a cancelled subagent, or an absent hook output) is
    // ignored: the bridge is optional and must never break the session.
    if (!input || input.tool !== "task") return;
    if (!output) return;
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

/// The generated OpenCode 2 adapter source. OpenCode 2 selects the Lead at the
/// session level in Rust, so this adapter is even thinner than v1: it never
/// rewrites the request model or agent.
pub fn v2_plugin_source() -> &'static str {
    PLUGIN_SOURCE_V2
}

const PLUGIN_SOURCE_V2: &str = r#"// Generated by OpenCode Gear (ocg) for OpenCode 2.x. Do not edit by hand.
// Thin adapter: all ranking, projection and policy live in the `ocg` Rust
// bridge. Lead agent/model/variant selection is session-level and performed in
// Rust; this adapter never rewrites the request model or agent.
//
// Hooks:
//   session.prompt        -> append dynamic context for the root Lead session
//   tool.execute.before   -> append the role hand-off to a `subagent` prompt
//   tool.execute.after    -> append verification feedback to a `subagent` result
//
// Runtime: OpenCode 2 ships both a Bun build and a Node build, so the bridge is
// spawned with `node:child_process` (supported by Node and Bun) using
// `shell: false` and a direct argv vector, never a shell.
//
// Fail soft: every bridge failure is swallowed so a broken bridge cannot break
// a session. A missing `tool.execute.after` delivery is ignored as well.

import { spawn } from "node:child_process";

const START = "<<<OCG:DYNAMIC_CONTEXT v1>>>";
const END = "<<<OCG:END>>>";

// The generated OCG consumer agents. Only a known consumer `subagent` launched
// by a Lead session is bridged; any other tool, subagent or caller is ignored.
// `ocg-explore-deep` is a distinct generated agent, not a variant of
// `ocg-explore`.
const CONSUMER_AGENTS = new Set([
  "ocg-explore",
  "ocg-explore-deep",
  "ocg-build",
  "ocg-verify",
  "ocg-debug",
  "ocg-docs",
]);

function executable() {
  return process.env.OPENCODE_GEAR_OCG || "ocg";
}

function project() {
  return process.env.OPENCODE_GEAR_PROJECT || process.cwd();
}

function orchestrationEnabled() {
  return process.env.OPENCODE_GEAR_ORCHESTRATION_ENABLED === "1";
}

function isLead(agent) {
  return typeof agent === "string" && agent.startsWith("lead-");
}

function isConsumer(agent) {
  return typeof agent === "string" && CONSUMER_AGENTS.has(agent);
}

// Spawn `ocg __bridge <event> --project <project>` with an exact argv vector and
// a JSON payload on stdin. The child inherits the exact process environment.
// Absence, a spawn error, a non-zero exit and invalid JSON all resolve to null:
// the caller then leaves the session untouched.
async function bridge(event, payload) {
  try {
    const child = spawn(executable(), ["__bridge", event, "--project", project()], {
      shell: false,
      stdio: ["pipe", "pipe", "ignore"],
      env: process.env,
    });
    const finished = new Promise((resolve) => {
      let stdout = "";
      child.stdout.setEncoding("utf8");
      child.stdout.on("data", (chunk) => {
        stdout += chunk;
      });
      child.on("error", () => resolve(null));
      child.on("close", (code) => resolve(code === 0 ? stdout : null));
    });
    try {
      // A spawn failure can close stdin before the write; that must stay soft.
      child.stdin.on("error", () => {});
      child.stdin.end(JSON.stringify(payload));
    } catch (_) {
      // stdin may already be closed; the bridge still answers or fails soft.
    }
    const text = await finished;
    if (!text) return null;
    try {
      return JSON.parse(text);
    } catch (_) {
      return null;
    }
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

function appendToPrompt(input, context) {
  if (!input || typeof input.prompt !== "string" || hasContext(input.prompt)) return;
  input.prompt = input.prompt + suffix(context);
}

// A completed foreground delivery carries both a structured nested output
// (`result.output.output`) and a visible content body (a string or content
// parts). Feedback is appended to both. Metadata and non-text content parts are
// never replaced or dropped.
function appendFeedback(delivery, context) {
  if (!delivery || typeof delivery !== "object") return;
  const nested = delivery.output;
  if (nested && typeof nested === "object" && typeof nested.output === "string") {
    if (!hasContext(nested.output)) nested.output = nested.output + suffix(context);
  }
  if (typeof delivery.content === "string") {
    if (!hasContext(delivery.content)) delivery.content = delivery.content + suffix(context);
  } else if (Array.isArray(delivery.content)) {
    appendToParts(delivery.content, context);
  }
}

function deliveryHasContext(delivery) {
  if (!delivery || typeof delivery !== "object") return false;
  const nested = delivery.output;
  if (nested && typeof nested === "object" && hasContext(nested.output)) return true;
  if (hasContext(delivery.content)) return true;
  if (Array.isArray(delivery.content)) {
    return textParts(delivery.content).some((part) => hasContext(part.text));
  }
  return false;
}

// A dependency-free structural default export. OpenCode 2 only requires a
// default `{ id, setup }` (or `{ id, effect }`) object, so the adapter imports
// no SDK package and keeps working across the renamed `@opencode/plugin`
// package layouts (the V2 tag has no `@opencode-ai/plugin/v2/promise` alias).
export default {
  id: "opencode-gear-orchestration",
  setup: async (ctx) => {
    const registrations = [];

    // The prompt event carries no agent, so the root Lead is resolved through
    // the session. A consumer session or a child session (`parentID`) is not
    // the root Lead and is left untouched. `event.prompt.text` is mutated
    // directly; the core prompt path consumes it after the hook returns.
    registrations.push(await ctx.session.hook("prompt", async (event) => {
      if (!orchestrationEnabled()) return;
      if (!event || !event.sessionID) return;
      let session;
      try {
        session = await ctx.session.get({ sessionID: event.sessionID });
      } catch (_) {
        return;
      }
      if (!session || session.parentID) return;
      const agent = typeof session.agent === "string" ? session.agent : "";
      if (!isLead(agent)) return;
      if (!event.prompt || typeof event.prompt.text !== "string") return;
      const text = event.prompt.text;
      if (!text || hasContext(text)) return;
      const result = await bridge("chat.message", { session_id: event.sessionID, agent, text });
      if (result && result.context) event.prompt.text = text + suffix(result.context);
    }));

    // The generic Tool boundary consumes a direct mutation of
    // `event.input.prompt`. The delimiter is checked before the bridge so a
    // repeated hook can never advance the controller twice.
    registrations.push(await ctx.tool.hook("execute.before", async (event) => {
      if (!orchestrationEnabled()) return;
      if (!event || event.tool !== "subagent") return;
      if (!isLead(event.agent) || !isConsumer(event.input && event.input.agent)) return;
      const input = event.input && typeof event.input === "object" ? event.input : {};
      if (typeof input.prompt !== "string" || input.prompt.length === 0) return;
      if (hasContext(input.prompt)) return;
      const result = await bridge("tool.execute.before", {
        session_id: event.sessionID,
        tool: event.tool,
        args: input,
      });
      if (result && result.context) appendToPrompt(input, result.context);
    }));

    // Only a completed foreground subagent has `result.output` with a
    // `completed` status. A background subagent is still `running` when the
    // tool returns, so asynchronous completion feedback is not supported: the
    // hook has no later delivery to consume and OCG must not guess that a
    // still-running task finished. Errors and missing deliveries are ignored.
    // As above, the delimiter is checked before the bridge.
    registrations.push(await ctx.tool.hook("execute.after", async (event) => {
      if (!orchestrationEnabled()) return;
      if (!event || event.tool !== "subagent") return;
      if (!isLead(event.agent) || !isConsumer(event.input && event.input.agent)) return;
      if (event.status !== "completed") return;
      const delivery = event.result;
      if (!delivery || typeof delivery !== "object") return;
      const nested = delivery.output;
      if (!nested || typeof nested !== "object") return;
      if (nested.status !== "completed") return;
      if (typeof nested.output !== "string") return;
      if (deliveryHasContext(delivery)) return;
      const result = await bridge("tool.execute.after", {
        session_id: event.sessionID,
        tool: event.tool,
        args: event.input,
        result: nested.output,
      });
      if (result && result.context) appendFeedback(delivery, result.context);
    }));

    return async () => {
      for (const registration of registrations) await registration.dispose();
    };
  },
};
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
        // The variant is optional: it is only written when the contract has one.
        assert!(source.contains("if (contract.variant !== undefined)"));
        assert!(source.contains("model.variant = contract.variant"));
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
  low: {agent: "lead-low", provider_id: "openai", model_id: "gpt-5.6-sol", variant: "low"},
  mid: {agent: "lead-mid", provider_id: "openai", model_id: "gpt-5.6-sol", variant: "medium"},
  high: {agent: "lead-high", provider_id: "openai", model_id: "gpt-6-astra", variant: "low"},
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
  // A session stuck on the old Sol/medium variant must not satisfy mid.
  mid: await check("mid", "lead-low", "lead-low", "openai", "gpt-5.6-sol", "low"),
  // A stale UI variant selection must not satisfy high, which needs Astra low.
  high: await check("high", "lead-low", "lead-low", "openai", "gpt-5.6-sol", "high"),
};
// A reused session whose sticky state is a different Lead tier must be corrected.
const reused = await check("high", "lead-low", "lead-low", "openai", "gpt-5.6-sol", "medium");
// Consumers keep their own configured route at every throttle level.
const consumer = {
  low: await check("low", "ocg-build", "ocg-build", "opencode-go", "deepseek-v4.1-flash", "high"),
  mid: await check("mid", "ocg-explore", "ocg-explore", "volcengine-coding-plan", "kimi-k2.7-code", "default"),
  high: await check("high", "ocg-verify", "ocg-verify", "opencode-go", "glm-5.3-flash", "high"),
};
// A request whose agent cannot be established must be left alone rather than
// silently re-routed onto a Lead model.
const unknown = await check("high", "", "", "opencode-go", "deepseek-v4.1-flash", "high");
console.log(JSON.stringify({lead, reused, consumer, unknown}));
"#,
        );
        let value = run_plugin_script(&dir);
        // `ocg low` -> lead-low / Sol / low, even with sticky mid state.
        assert_eq!(value["lead"]["low"]["agent"], json!("lead-low"));
        assert_eq!(
            value["lead"]["low"]["model"]["modelID"],
            json!("gpt-5.6-sol")
        );
        assert_eq!(value["lead"]["low"]["model"]["variant"], json!("low"));
        // `ocg mid` -> lead-mid / Sol / medium, even with a sticky Sol low.
        assert_eq!(value["lead"]["mid"]["agent"], json!("lead-mid"));
        assert_eq!(
            value["lead"]["mid"]["model"]["modelID"],
            json!("gpt-5.6-sol")
        );
        assert_eq!(value["lead"]["mid"]["model"]["variant"], json!("medium"));
        // `ocg high` -> lead-high / Astra / low, never a Sol fallback.
        assert_eq!(value["lead"]["high"]["agent"], json!("lead-high"));
        assert_eq!(
            value["lead"]["high"]["model"]["modelID"],
            json!("gpt-6-astra")
        );
        assert_eq!(value["lead"]["high"]["model"]["variant"], json!("low"));
        // A reused session cannot silently override the explicit throttle.
        assert_eq!(value["reused"]["agent"], json!("lead-high"));
        assert_eq!(value["reused"]["model"]["modelID"], json!("gpt-6-astra"));
        assert_eq!(value["reused"]["model"]["variant"], json!("low"));
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
    fn chat_message_omits_a_variant_when_the_contract_has_none() {
        if !node_or_skip("chat_message_omits_a_variant_when_the_contract_has_none") {
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        write_plugin(
            &dir,
            r#"import { server } from "./plugin.mjs";
const hooks = await server({});
process.env.OPENCODE_GEAR_ORCHESTRATION_ENABLED = "0";
async function check(contract, providerID, modelID, variant) {
  process.env.OPENCODE_GEAR_LEAD_CONTRACT = JSON.stringify(contract);
  const output = {message: {agent: "lead-custom", model: {providerID, modelID, variant}}, parts: []};
  await hooks["chat.message"]({sessionID: "session", agent: "lead-custom"}, output);
  return output.message;
}
// A provider-default contract (no variant) must not inherit a sticky variant
// and must not fabricate one.
const providerDefault = await check(
  {agent: "lead-custom", provider_id: "acme", model_id: "widget"},
  "acme", "widget", "high",
);
// An explicit variant is still enforced over any sticky selection.
const explicit = await check(
  {agent: "lead-custom", provider_id: "acme", model_id: "widget", variant: "max"},
  "acme", "widget", "low",
);
console.log(JSON.stringify({providerDefault, explicit}));
"#,
        );
        let value = run_plugin_script(&dir);
        assert_eq!(
            value["providerDefault"]["model"]["providerID"],
            json!("acme")
        );
        assert_eq!(
            value["providerDefault"]["model"]["modelID"],
            json!("widget")
        );
        assert!(
            value["providerDefault"]["model"].get("variant").is_none(),
            "provider-default contract must not carry a variant: {}",
            value["providerDefault"]["model"]
        );
        assert_eq!(value["explicit"]["model"]["variant"], json!("max"));
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
  low: {agent: "lead-low", provider_id: "openai", model_id: "gpt-5.6-sol", variant: "low"},
  mid: {agent: "lead-mid", provider_id: "openai", model_id: "gpt-5.6-sol", variant: "medium"},
  high: {agent: "lead-high", provider_id: "openai", model_id: "gpt-6-astra", variant: "low"},
};
// OpenCode resolves an explicit/selected/sticky variant ahead of the agent
// variant. The historical "stuck on medium" symptom is a session carrying a
// previous level's variant; every case below must still land on the contract.
function createUserMessage(level, selectedVariant) {
  const contract = CONTRACTS[level];
  const agentVariant = contract.variant;
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
  low_sticky_medium: await request("low", "medium"),
  mid_sticky_high: await request("mid", "high"),
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
            ("low_sticky_medium", "lead-low", "gpt-5.6-sol", "low"),
            ("mid_sticky_high", "lead-mid", "gpt-5.6-sol", "medium"),
            ("high_sticky_medium", "lead-high", "gpt-6-astra", "low"),
            ("low_no_selection", "lead-low", "gpt-5.6-sol", "low"),
            ("mid_no_selection", "lead-mid", "gpt-5.6-sol", "medium"),
            ("high_no_selection", "lead-high", "gpt-6-astra", "low"),
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

    #[test]
    fn injection_uses_the_plural_key_for_v2_and_preserves_user_plugins() {
        let mut config = json!({"plugins": ["my-plugin"]});
        let uri = "file:///tmp/project/.opencode-gear/orchestration/plugin/ocg-orchestration.js";
        inject_plugin_for(&mut config, "plugins", uri);
        assert!(config.get("plugin").is_none());
        let entries = config["plugins"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0], json!("my-plugin"));
        assert_eq!(entries[1], json!(uri));
        assert!(has_ocg_plugin_for(&config, "plugins"));
        assert!(!has_ocg_plugin(&config));
        remove_ocg_plugin_for(&mut config, "plugins");
        assert_eq!(config["plugins"], json!(["my-plugin"]));
        assert!(!has_ocg_plugin_for(&config, "plugins"));
    }

    #[test]
    fn v2_source_uses_a_node_spawn_and_never_rewrites_the_request() {
        let source = v2_plugin_source();
        // The V2 runtime may be Node, so the bridge must not depend on Bun.
        assert!(source.contains("import { spawn } from \"node:child_process\""));
        assert!(source.contains("shell: false"));
        assert!(!source.contains("Bun.spawn"));
        assert!(source.contains("export default {"));
        assert!(source.contains("id: \"opencode-gear-orchestration\""));
        assert!(source.contains("setup: async (ctx)"));
        assert!(source.contains("ctx.tool.hook(\"execute.before\""));
        assert!(source.contains("ctx.tool.hook(\"execute.after\""));
        assert!(source.contains("ctx.session.hook(\"prompt\""));
        assert!(source.contains("event.tool !== \"subagent\""));
        // The exact direct-argv contract: `__bridge <event> --project <project>`.
        assert!(source.contains("[\"__bridge\", event, \"--project\", project()]"));
        // Only known OCG consumer agents launched from a Lead are bridged.
        assert!(source.contains("isConsumer(event.input && event.input.agent)"));
        assert!(source.contains("isLead(event.agent)"));
        // The v2 adapter is thinner: no request-message Lead enforcement.
        assert!(!source.contains("enforceLeadContract"));
        assert!(!source.contains("output.message.agent"));
        assert!(!source.contains("providerID:"));
        assert!(!source.contains("export const server"));
    }

    #[test]
    fn tool_after_registration_ignores_a_missing_delivery() {
        let v1 = plugin_source();
        assert!(v1.contains("\"tool.execute.after\": async"));
        assert!(v1.contains("if (!input || input.tool !=="));
        assert!(v1.contains("if (!output) return;"));
        let v2 = v2_plugin_source();
        assert!(v2.contains("ctx.tool.hook(\"execute.after\""));
        // A non-completed tool call, a background (running) delivery and a
        // missing delivery are all ignored without a bridge call.
        assert!(v2.contains("if (event.status !== \"completed\") return;"));
        assert!(v2.contains("if (nested.status !== \"completed\") return;"));
        assert!(v2.contains("if (!delivery || typeof delivery !== \"object\") return;"));
    }

    // ---- OpenCode 2 controlled hook tests ---------------------------------
    //
    // These execute the generated V2 adapter under `node` against a fixture
    // bridge *executable*. The fixture is a real child process, so the tests
    // prove the exact direct argv, the inherited environment and that no shell
    // is involved (its path contains a space). No OCG Rust bridge runs.

    /// A fixture `ocg __bridge` executable: it records every invocation and
    /// answers with a deterministic context, or fails in a controlled mode.
    #[cfg(unix)]
    const FIXTURE_BRIDGE: &str = r#"#!/usr/bin/env node
const fs = require("node:fs");
const args = process.argv.slice(2);
let stdin = "";
process.stdin.setEncoding("utf8");
process.stdin.on("data", (chunk) => { stdin += chunk; });
process.stdin.on("end", () => {
  const record = process.env.OCG_RECORD;
  if (record) {
    fs.appendFileSync(
      record,
      JSON.stringify({
        argv: args,
        project: process.env.OPENCODE_GEAR_PROJECT || null,
        marker: process.env.OCG_FIXTURE_MARKER || null,
        inherited: process.env.OPENCODE_GEAR_ORCHESTRATION_ENABLED || null,
        stdin,
      }) + "\n",
    );
  }
  const mode = process.env.OCG_FIXTURE_MODE || "context";
  if (mode === "nonzero") { process.exitCode = 7; return; }
  if (mode === "invalid") { process.stdout.write("this is not json"); return; }
  if (mode === "silent") return;
  let payload = {};
  try { payload = JSON.parse(stdin); } catch (_) {}
  let context = "fixture context for " + (args[1] || "unknown");
  if (args[1] === "tool.execute.after" && typeof payload.result === "string") {
    context = "fixture verification feedback";
  }
  process.stdout.write(JSON.stringify({ ok: true, context }));
});
"#;

    /// The shared adapter harness: imports the generated V2 default export,
    /// builds a mock hook context and points the bridge at the fixture.
    #[cfg(unix)]
    const JS_PRELUDE: &str = r#"import fs from "node:fs";
import plugin from "./plugin.mjs";

process.env.OPENCODE_GEAR_OCG = process.env.OCG_BRIDGE_PATH;
process.env.OPENCODE_GEAR_PROJECT = process.cwd();
process.env.OPENCODE_GEAR_ORCHESTRATION_ENABLED = "1";
process.env.OCG_FIXTURE_RECORD = process.env.OCG_RECORD;

function makeCtx(session) {
  const hooks = {};
  const ctx = {
    session: {
      hook: async (name, cb) => { hooks[name] = cb; return { dispose: async () => {} }; },
      get: async () => session,
    },
    tool: {
      hook: async (name, cb) => { hooks[name] = cb; return { dispose: async () => {} }; },
    },
  };
  return { ctx, hooks };
}

function lines() {
  try {
    return fs.readFileSync(process.env.OCG_RECORD, "utf8").trim().split("\n").filter(Boolean).length;
  } catch (_) { return 0; }
}
"#;

    #[cfg(unix)]
    fn write_v2_plugin(dir: &tempfile::TempDir) {
        std::fs::write(dir.path().join("plugin.mjs"), v2_plugin_source()).unwrap();
    }

    /// Write the fixture bridge into a directory whose name contains a space.
    /// A shell-based spawn would split that path; a direct argv spawn does not.
    #[cfg(unix)]
    fn write_fixture_bridge(dir: &tempfile::TempDir) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let bridge_dir = dir.path().join("bridge dir");
        std::fs::create_dir_all(&bridge_dir).unwrap();
        let path = bridge_dir.join("fixture-bridge.js");
        std::fs::write(&path, FIXTURE_BRIDGE).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    fn write_check(dir: &tempfile::TempDir, script: &str) {
        std::fs::write(dir.path().join("check.mjs"), script).unwrap();
    }

    #[cfg(unix)]
    fn run_check(dir: &tempfile::TempDir, bridge: &Path, record: &Path) -> Value {
        let output = std::process::Command::new("node")
            .arg("check.mjs")
            .current_dir(dir.path())
            .env("OCG_BRIDGE_PATH", bridge)
            .env("OCG_RECORD", record)
            .env("OCG_FIXTURE_MARKER", "inherited-env")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    #[cfg(unix)]
    fn records(record: &Path) -> Vec<Value> {
        let text = std::fs::read_to_string(record).unwrap_or_default();
        text.lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[cfg(unix)]
    const PROMPT_BODY: &str = r#"
const leadSession = { agent: "lead-mid" };
const { ctx, hooks } = makeCtx(leadSession);
await plugin.setup(ctx);

const event = { sessionID: "s1", messageID: "m1", prompt: { text: "hello lead" } };
await hooks["prompt"](event);
const mutated = event.prompt.text;
const countAfterFirst = lines();

// The prompt is now delimited: a repeated hook must not spawn the bridge again.
await hooks["prompt"](event);
const countAfterSecond = lines();

// A consumer session is not the root Lead.
leadSession.agent = "ocg-build";
const consumer = { sessionID: "s2", prompt: { text: "consumer" } };
await hooks["prompt"](consumer);

// A child session (parentID) is not the root Lead.
leadSession.agent = "lead-mid";
leadSession.parentID = "parent";
const child = { sessionID: "s3", prompt: { text: "child" } };
await hooks["prompt"](child);
delete leadSession.parentID;

console.log(JSON.stringify({
  mutated,
  countAfterFirst,
  countAfterSecond,
  totalLines: lines(),
  consumerText: consumer.prompt.text,
  childText: child.prompt.text,
}));
"#;

    #[test]
    fn v2_prompt_hook_bridges_only_the_root_lead_and_is_idempotent() {
        if !node_or_skip("v2_prompt_hook_bridges_only_the_root_lead") {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        write_v2_plugin(&dir);
        let bridge = write_fixture_bridge(&dir);
        let record = dir.path().join("record.jsonl");
        write_check(&dir, &format!("{JS_PRELUDE}{PROMPT_BODY}"));
        let value = run_check(&dir, &bridge, &record);
        let records = records(&record);
        assert_eq!(
            records.len(),
            1,
            "a repeated, consumer or child prompt must not bridge"
        );
        assert_eq!(value["countAfterFirst"], json!(1));
        assert_eq!(value["countAfterSecond"], json!(1));
        assert_eq!(value["totalLines"], json!(1));
        let mutated = value["mutated"].as_str().unwrap();
        assert!(mutated.starts_with("hello lead"), "{mutated}");
        assert!(mutated.contains(CONTEXT_START) && mutated.contains(CONTEXT_END));
        assert!(mutated.contains("fixture context for chat.message"));
        assert_eq!(value["consumerText"], json!("consumer"));
        assert_eq!(value["childText"], json!("child"));
        // Exact direct argv and inherited environment. `process.cwd()` is the
        // canonical path, so compare against the canonicalized fixture root.
        let root = dir.path().canonicalize().unwrap();
        let argv: Vec<&str> = records[0]["argv"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item.as_str().unwrap())
            .collect();
        assert_eq!(
            argv,
            vec![
                "__bridge",
                "chat.message",
                "--project",
                root.to_str().unwrap()
            ]
        );
        assert_eq!(records[0]["project"], json!(root.to_str().unwrap()));
        assert_eq!(records[0]["marker"], json!("inherited-env"));
        assert_eq!(records[0]["inherited"], json!("1"));
        let stdin: Value = serde_json::from_str(records[0]["stdin"].as_str().unwrap()).unwrap();
        assert_eq!(stdin["session_id"], json!("s1"));
        assert_eq!(stdin["agent"], json!("lead-mid"));
        assert_eq!(stdin["text"], json!("hello lead"));
    }

    #[cfg(unix)]
    const BEFORE_BODY: &str = r#"
const { ctx, hooks } = makeCtx({ agent: "lead-mid" });
await plugin.setup(ctx);

const event = {
  tool: "subagent",
  sessionID: "s1",
  agent: "lead-mid",
  messageID: "m1",
  id: "t1",
  input: { agent: "ocg-explore", description: "explore it", prompt: "explore the parser" },
};
await hooks["execute.before"](event);
const mutated = event.input.prompt;
const firstStdin = JSON.parse(fs.readFileSync(process.env.OCG_RECORD, "utf8").trim().split("\n")[0]).stdin;
// Repeating the hook must not advance the controller a second time.
await hooks["execute.before"](event);

// A non-OCG subagent launched from a Lead is never bridged.
const unknown = { tool: "subagent", sessionID: "s2", agent: "lead-mid", input: { agent: "general", prompt: "x" } };
await hooks["execute.before"](unknown);
// A known consumer launched by a non-Lead caller is never bridged.
const consumerCaller = { tool: "subagent", sessionID: "s3", agent: "ocg-debug", input: { agent: "ocg-build", prompt: "y" } };
await hooks["execute.before"](consumerCaller);
// Any other tool is ignored.
const otherTool = { tool: "bash", sessionID: "s4", agent: "lead-mid", input: {} };
await hooks["execute.before"](otherTool);

console.log(JSON.stringify({
  mutated,
  firstStdin,
  countAfterRepeat: lines(),
  unknownPrompt: unknown.input.prompt,
  consumerCallerPrompt: consumerCaller.input.prompt,
}));
"#;

    #[test]
    fn v2_before_hook_bridges_only_known_consumers_from_a_lead() {
        if !node_or_skip("v2_before_hook_bridges_only_known_consumers") {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        write_v2_plugin(&dir);
        let bridge = write_fixture_bridge(&dir);
        let record = dir.path().join("record.jsonl");
        write_check(&dir, &format!("{JS_PRELUDE}{BEFORE_BODY}"));
        let value = run_check(&dir, &bridge, &record);
        let records = records(&record);
        assert_eq!(records.len(), 1, "only the Lead/consumer pair may bridge");
        let mutated = value["mutated"].as_str().unwrap();
        assert!(mutated.starts_with("explore the parser"));
        assert!(mutated.contains(CONTEXT_START) && mutated.contains(CONTEXT_END));
        assert!(mutated.contains("fixture context for tool.execute.before"));
        // The bridge saw the pre-mutation prompt and the raw subagent input.
        let stdin: Value = serde_json::from_str(value["firstStdin"].as_str().unwrap()).unwrap();
        assert_eq!(stdin["args"]["agent"], json!("ocg-explore"));
        assert_eq!(stdin["args"]["prompt"], json!("explore the parser"));
        assert_eq!(stdin["session_id"], json!("s1"));
        // Role exclusion leaves every other call untouched.
        assert_eq!(value["unknownPrompt"], json!("x"));
        assert_eq!(value["consumerCallerPrompt"], json!("y"));
    }

    #[cfg(unix)]
    const AFTER_BODY: &str = r#"
const { ctx, hooks } = makeCtx({ agent: "lead-mid" });
await plugin.setup(ctx);

const event = {
  tool: "subagent",
  sessionID: "s1",
  agent: "lead-mid",
  messageID: "m1",
  id: "t1",
  status: "completed",
  input: { agent: "ocg-build", description: "build it", prompt: "build" },
  result: {
    output: { sessionID: "sub1", status: "completed", output: "subagent finished" },
    content: "visible body",
    metadata: { keep: true, tokens: 12 },
  },
};
await hooks["execute.after"](event);
const nested = event.result.output.output;
const content = event.result.content;
const metadata = event.result.metadata;
const firstStdin = JSON.parse(fs.readFileSync(process.env.OCG_RECORD, "utf8").trim().split("\n")[0]).stdin;
// Idempotent: the delimiter check runs before the bridge.
await hooks["execute.after"](event);

// A content-part array appends to its last text part, never to the non-text.
const partsEvent = {
  tool: "subagent",
  sessionID: "s2",
  agent: "lead-mid",
  status: "completed",
  input: { agent: "ocg-verify", prompt: "verify" },
  result: {
    output: { status: "completed", output: "done" },
    content: [
      { type: "text", text: "first" },
      { type: "image", url: "x" },
      { type: "text", text: "last" },
    ],
  },
};
await hooks["execute.after"](partsEvent);

console.log(JSON.stringify({
  nested,
  content,
  metadata,
  firstStdin,
  count: lines(),
  parts: partsEvent.result.content,
}));
"#;

    #[test]
    fn v2_after_hook_appends_feedback_to_output_and_visible_content() {
        if !node_or_skip("v2_after_hook_appends_feedback") {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        write_v2_plugin(&dir);
        let bridge = write_fixture_bridge(&dir);
        let record = dir.path().join("record.jsonl");
        write_check(&dir, &format!("{JS_PRELUDE}{AFTER_BODY}"));
        let value = run_check(&dir, &bridge, &record);
        let records = records(&record);
        // One completion, plus the text-part completion; the repeat adds none.
        assert_eq!(records.len(), 2);
        assert_eq!(value["count"], json!(2));
        let nested = value["nested"].as_str().unwrap();
        assert!(nested.starts_with("subagent finished"));
        assert!(nested.contains("fixture verification feedback"));
        let content = value["content"].as_str().unwrap();
        assert!(content.starts_with("visible body"));
        assert!(content.contains("fixture verification feedback"));
        // Metadata is preserved untouched.
        assert_eq!(value["metadata"], json!({"keep": true, "tokens": 12}));
        // The bridge received the bare nested output string as `result`.
        let stdin: Value = serde_json::from_str(value["firstStdin"].as_str().unwrap()).unwrap();
        assert_eq!(stdin["result"], json!("subagent finished"));
        assert_eq!(stdin["args"]["agent"], json!("ocg-build"));
        // The first text part is untouched, the non-text part survives and the
        // last text part carries the feedback.
        let parts = value["parts"].as_array().unwrap();
        assert_eq!(parts[0]["text"], json!("first"));
        assert_eq!(parts[1]["type"], json!("image"));
        assert!(parts[2]["text"].as_str().unwrap().starts_with("last"));
        assert!(parts[2]["text"]
            .as_str()
            .unwrap()
            .contains("fixture verification feedback"));
    }

    #[cfg(unix)]
    const IGNORE_BODY: &str = r#"
const { ctx, hooks } = makeCtx({ agent: "lead-mid" });
await plugin.setup(ctx);

const running = {
  tool: "subagent", sessionID: "s1", agent: "lead-mid", status: "completed",
  input: { agent: "ocg-build", prompt: "b" },
  result: { output: { status: "running", output: "background" }, content: "bg" },
};
await hooks["execute.after"](running);

const errorStatus = {
  tool: "subagent", sessionID: "s2", agent: "lead-mid", status: "error", error: "boom",
  input: { agent: "ocg-build", prompt: "b" },
  result: { output: { status: "completed", output: "never" }, content: "never" },
};
await hooks["execute.after"](errorStatus);

const missing = { tool: "subagent", sessionID: "s3", agent: "lead-mid", status: "completed", input: { agent: "ocg-build", prompt: "b" } };
await hooks["execute.after"](missing);

const unknownRole = {
  tool: "subagent", sessionID: "s4", agent: "lead-mid", status: "completed",
  input: { agent: "general", prompt: "g" },
  result: { output: { status: "completed", output: "x" }, content: "x" },
};
await hooks["execute.after"](unknownRole);

const consumerCaller = {
  tool: "subagent", sessionID: "s5", agent: "ocg-debug", status: "completed",
  input: { agent: "ocg-build", prompt: "b" },
  result: { output: { status: "completed", output: "x" }, content: "x" },
};
await hooks["execute.after"](consumerCaller);

await hooks["execute.after"](undefined);
await hooks["execute.after"](null);

console.log(JSON.stringify({
  count: lines(),
  running: running.result,
  errorResult: errorStatus.result,
  missing: missing.result === undefined,
  unknownRole: unknownRole.result,
  consumerCaller: consumerCaller.result,
}));
"#;

    #[test]
    fn v2_after_hook_ignores_running_errors_and_missing_deliveries() {
        if !node_or_skip("v2_after_hook_ignores_running_errors") {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        write_v2_plugin(&dir);
        let bridge = write_fixture_bridge(&dir);
        let record = dir.path().join("record.jsonl");
        write_check(&dir, &format!("{JS_PRELUDE}{IGNORE_BODY}"));
        let value = run_check(&dir, &bridge, &record);
        assert_eq!(records(&record).len(), 0);
        assert_eq!(value["count"], json!(0));
        // A running background subagent is not a completion and is untouched.
        assert_eq!(value["running"]["output"]["output"], json!("background"));
        assert_eq!(value["running"]["content"], json!("bg"));
        // An errored tool call is untouched.
        assert_eq!(value["errorResult"]["output"]["output"], json!("never"));
        assert_eq!(value["errorResult"]["content"], json!("never"));
        assert_eq!(value["missing"], json!(true));
        assert_eq!(value["unknownRole"]["output"]["output"], json!("x"));
        assert_eq!(value["consumerCaller"]["content"], json!("x"));
    }

    #[test]
    fn v2_hooks_do_nothing_when_orchestration_is_disabled() {
        if !node_or_skip("v2_hooks_do_nothing_when_disabled") {
            return;
        }
        #[cfg(unix)]
        {
            let dir = tempfile::tempdir().unwrap();
            write_v2_plugin(&dir);
            let bridge = write_fixture_bridge(&dir);
            let record = dir.path().join("record.jsonl");
            let body = r#"
const { ctx, hooks } = makeCtx({ agent: "lead-mid" });
await plugin.setup(ctx);
process.env.OPENCODE_GEAR_ORCHESTRATION_ENABLED = "0";

const prompt = { sessionID: "s1", prompt: { text: "hello" } };
await hooks["prompt"](prompt);
const before = { tool: "subagent", sessionID: "s2", agent: "lead-mid", input: { agent: "ocg-build", prompt: "build" } };
await hooks["execute.before"](before);
const after = {
  tool: "subagent", sessionID: "s3", agent: "lead-mid", status: "completed",
  input: { agent: "ocg-build", prompt: "build" },
  result: { output: { status: "completed", output: "done" }, content: "visible" },
};
await hooks["execute.after"](after);

console.log(JSON.stringify({
  count: lines(),
  promptText: prompt.prompt.text,
  beforePrompt: before.input.prompt,
  afterOutput: after.result.output.output,
  afterContent: after.result.content,
}));
"#;
            write_check(&dir, &format!("{JS_PRELUDE}{body}"));
            let value = run_check(&dir, &bridge, &record);
            assert_eq!(records(&record).len(), 0);
            assert_eq!(value["count"], json!(0));
            assert_eq!(value["promptText"], json!("hello"));
            assert_eq!(value["beforePrompt"], json!("build"));
            assert_eq!(value["afterOutput"], json!("done"));
            assert_eq!(value["afterContent"], json!("visible"));
        }
    }

    #[test]
    fn v2_bridge_fails_soft_on_absence_errors_and_invalid_json() {
        if !node_or_skip("v2_bridge_fails_soft") {
            return;
        }
        #[cfg(unix)]
        {
            let dir = tempfile::tempdir().unwrap();
            write_v2_plugin(&dir);
            let bridge = write_fixture_bridge(&dir);
            let record = dir.path().join("record.jsonl");
            let body = r#"
const { ctx, hooks } = makeCtx({ agent: "lead-mid" });
await plugin.setup(ctx);

async function runPrompt() {
  const event = { sessionID: "s", prompt: { text: "hello" } };
  await hooks["prompt"](event);
  return event.prompt.text;
}

// Missing executable: spawn emits an error.
process.env.OPENCODE_GEAR_OCG = process.env.OCG_RECORD + ".does-not-exist";
const absent = await runPrompt();

// Non-zero exit.
process.env.OPENCODE_GEAR_OCG = process.env.OCG_BRIDGE_PATH;
process.env.OCG_FIXTURE_MODE = "nonzero";
const nonzero = await runPrompt();

// Invalid JSON on stdout.
process.env.OCG_FIXTURE_MODE = "invalid";
const invalid = await runPrompt();

// Empty stdout.
process.env.OCG_FIXTURE_MODE = "silent";
const silent = await runPrompt();

console.log(JSON.stringify({ absent, nonzero, invalid, silent }));
"#;
            write_check(&dir, &format!("{JS_PRELUDE}{body}"));
            let value = run_check(&dir, &bridge, &record);
            // Every failure resolves to null, so the prompt is never mutated.
            assert_eq!(value["absent"], json!("hello"));
            assert_eq!(value["nonzero"], json!("hello"));
            assert_eq!(value["invalid"], json!("hello"));
            assert_eq!(value["silent"], json!("hello"));
        }
    }
}

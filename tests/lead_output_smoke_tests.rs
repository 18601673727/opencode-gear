//! Bounded end-to-end smoke for the latest-Lead-output capture.
//!
//! This drives the *real* generated OpenCode 2 adapter (the exact file
//! `ocg` materializes at launch) under `node`, with the real `ocg` test binary
//! as the bridge executable. The adapter reports a completed root Lead message
//! through `ocg __bridge lead.output --project ...`, and the assertion is the
//! real artifact on disk: `<project>/.opencode-gear/reports/latest-lead-output.md`
//! containing exactly the Lead's raw text.
//!
//! Everything except the OpenCode host process is production code: the
//! adapter source, the bridge spawn (direct argv, JSON on stdin), the reports
//! policy, and the atomic write. The OpenCode host itself is not started (a
//! nested runtime cannot be isolated on this host; that limitation is recorded
//! in the closeout report), so the event objects below mirror the documented
//! OpenCode 2 event shapes (`message.updated` / `message.part.updated`).

mod common;

use common::TestDir;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn node_or_skip(test: &str) -> bool {
    let available = Command::new("node")
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
        "node is required for the latest-Lead-output smoke test ({test}) but was not found"
    );
    eprintln!("skipping latest-Lead-output smoke ({test}): node is unavailable");
    false
}

fn write_check(dir: &Path, script: &str) {
    fs::write(dir.join("check.mjs"), script).expect("write check.mjs");
}

const PRELUDE: &str = r#"import fs from "node:fs";
import plugin from "./plugin.mjs";

function makeStream() {
  const queue = [];
  let pending = null;
  let closed = false;
  const iterator = {
    [Symbol.asyncIterator]() { return this; },
    next() {
      if (queue.length) return Promise.resolve({ value: queue.shift(), done: false });
      if (closed) return Promise.resolve({ value: undefined, done: true });
      return new Promise((resolve) => { pending = resolve; });
    },
    return() {
      closed = true;
      if (pending) { const resolve = pending; pending = null; resolve({ value: undefined, done: true }); }
      return Promise.resolve({ value: undefined, done: true });
    },
  };
  return {
    iterator,
    push(event) {
      if (pending) { const resolve = pending; pending = null; resolve({ value: event, done: false }); }
      else queue.push(event);
    },
  };
}

const stream = makeStream();
const ctx = {
  event: { subscribe: () => stream.iterator },
  session: { hook: async () => ({ dispose: async () => {} }) },
  tool: { hook: async () => ({ dispose: async () => {} }) },
};
"#;

fn project(dir: &TestDir) -> PathBuf {
    let project = dir.project();
    fs::create_dir_all(project.join("src")).expect("create project");
    fs::write(
        project.join("src/parser.rs"),
        "pub fn parse() -> u32 { 1 }\n",
    )
    .expect("write source");
    fs::write(project.join(".opencode-gear.yaml"), "{}\n").expect("write project config");
    project
}

#[test]
fn the_generated_v2_adapter_produces_the_raw_latest_lead_output_file() {
    if !node_or_skip("the_generated_v2_adapter_produces_the_raw_latest_lead_output_file") {
        return;
    }
    let dir = TestDir::new();
    let project = project(&dir);

    // Materialize the real adapter through the exact launch path.
    let plugin_path = opencode_gear::orchestration::plugin::materialize_v2_with(
        &project,
        opencode_gear::runtime::compat::v2_adapter().plugin_source(),
    )
    .expect("materialize the V2 adapter");
    assert!(plugin_path.is_file(), "adapter must be materialized");
    let source = fs::read_to_string(&plugin_path).expect("read adapter");
    // Node needs an explicit ESM extension; the bytes are the materialized ones.
    fs::write(dir.join("plugin.mjs"), source).expect("stage adapter for node");

    let raw_text = "# Final answer\n\n- one\n- two\n\n```rust\nfn done() {}\n```\n";
    let body = format!(
        r#"{PRELUDE}
const dispose = await plugin.setup(ctx);
stream.push({{ type: "message.part.updated", data: {{ sessionID: "root", part: {{ id: "p1", messageID: "m1", type: "text", text: "{escaped}" }} }} }});
stream.push({{ type: "message.part.updated", data: {{ sessionID: "root", part: {{ id: "r1", messageID: "m1", type: "reasoning", text: "thinking" }} }} }});
// Still streaming: no completion timestamp yet.
stream.push({{ type: "message.updated", data: {{ sessionID: "root", info: {{ id: "m1", role: "assistant", agent: "lead-high", time: {{ created: 1 }} }} }} }});
// Completed root Lead message.
stream.push({{ type: "message.updated", data: {{ sessionID: "root", info: {{ id: "m1", role: "assistant", agent: "lead-high", time: {{ created: 1, completed: 2 }} }} }} }});
const target = process.env.OCG_EXPECTED_FILE;
const deadline = Date.now() + 10000;
while (!fs.existsSync(target) && Date.now() < deadline) {{
  await new Promise((resolve) => setTimeout(resolve, 20));
}}
await dispose();
console.log(JSON.stringify({{ exists: fs.existsSync(target) }}));
"#,
        escaped = raw_text
            .replace('\\', "\\\\")
            .replace('\n', "\\n")
            .replace('"', "\\\"")
    );
    write_check(dir.path(), &body);

    let expected = opencode_gear::reports::latest_lead_output_path(&project);
    let output = Command::new("node")
        .arg("check.mjs")
        .current_dir(dir.path())
        // The bridge is the real binary under test.
        .env("OPENCODE_GEAR_OCG", env!("CARGO_BIN_EXE_ocg"))
        .env("OPENCODE_GEAR_PROJECT", &project)
        .env("OPENCODE_GEAR_ORCHESTRATION_ENABLED", "1")
        .env("OPENCODE_GEAR_REPORTS_LATEST_LEAD_OUTPUT", "1")
        // Never read the developer's real configuration from the bridge child.
        .env("OPENCODE_GEAR_USER_CONFIG", dir.join("no-user.yaml"))
        .env(
            "OPENCODE_GEAR_PROJECT_CONFIG",
            project.join(".opencode-gear.yaml"),
        )
        .env("HOME", dir.join("home"))
        .env("OCG_EXPECTED_FILE", &expected)
        .output()
        .expect("run the node adapter check");
    assert!(
        output.status.success(),
        "node check failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(
        expected.is_file(),
        "the report file must exist: {expected:?}"
    );
    assert_eq!(
        fs::read_to_string(&expected).expect("read report"),
        raw_text,
        "the report must be the Lead's raw text, byte-verbatim"
    );
    // The report lives under the ignored project state directory.
    assert_eq!(
        expected,
        project
            .join(".opencode-gear")
            .join("reports")
            .join("latest-lead-output.md")
    );
}

#[test]
fn the_switch_disables_the_capture_end_to_end() {
    if !node_or_skip("the_switch_disables_the_capture_end_to_end") {
        return;
    }
    let dir = TestDir::new();
    let project = project(&dir);
    let plugin_path = opencode_gear::orchestration::plugin::materialize_v2_with(
        &project,
        opencode_gear::runtime::compat::v2_adapter().plugin_source(),
    )
    .expect("materialize the V2 adapter");
    fs::write(
        dir.join("plugin.mjs"),
        fs::read_to_string(&plugin_path).expect("read adapter"),
    )
    .expect("stage adapter");

    let body = r#"import fs from "node:fs";
import plugin from "./plugin.mjs";
const ctx = {
  event: { subscribe: () => ({ [Symbol.asyncIterator]() { return this; }, next: () => new Promise(() => {}) }) },
  session: { hook: async () => ({ dispose: async () => {} }) },
  tool: { hook: async () => ({ dispose: async () => {} }) },
};
await (await plugin.setup(ctx))();
console.log(JSON.stringify({ exists: fs.existsSync(process.env.OCG_EXPECTED_FILE) }));
"#;
    write_check(dir.path(), body);

    let expected = opencode_gear::reports::latest_lead_output_path(&project);
    let output = Command::new("node")
        .arg("check.mjs")
        .current_dir(dir.path())
        .env("OPENCODE_GEAR_OCG", env!("CARGO_BIN_EXE_ocg"))
        .env("OPENCODE_GEAR_PROJECT", &project)
        .env("OPENCODE_GEAR_ORCHESTRATION_ENABLED", "1")
        .env("OPENCODE_GEAR_REPORTS_LATEST_LEAD_OUTPUT", "0")
        .env("OPENCODE_GEAR_USER_CONFIG", dir.join("no-user.yaml"))
        .env(
            "OPENCODE_GEAR_PROJECT_CONFIG",
            project.join(".opencode-gear.yaml"),
        )
        .env("HOME", dir.join("home"))
        .env("OCG_EXPECTED_FILE", &expected)
        .output()
        .expect("run the node adapter check");
    assert!(
        output.status.success(),
        "node check failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !expected.exists(),
        "a disabled switch must not write anything"
    );
}

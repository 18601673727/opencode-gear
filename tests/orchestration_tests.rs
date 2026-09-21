//! End-to-end orchestration controller tests.
//!
//! These drive the Rust controller with deterministic fakes: a `FakeGitHost`,
//! a `FixedClock` and a `FakeCaptureRunner`. No model, network or real OpenCode
//! process is involved.

use opencode_gear::capabilities::CapabilityConfig;
use opencode_gear::clock::FixedClock;
use opencode_gear::context::ContextConfig;
use opencode_gear::orchestration::bridge::BridgeContext;
use opencode_gear::orchestration::config::OrchestrationConfig;
use opencode_gear::orchestration::controller::{BuildDecision, Controller};
use opencode_gear::orchestration::handoff::{Role, Severity};
use opencode_gear::orchestration::state::OrchestrationPhase;
use opencode_gear::process::{CapturedOutput, FakeCaptureRunner, FakeGitHost};
use opencode_gear::telemetry::{TelemetryConfig, TelemetryStats, TelemetryStore};
use opencode_gear::verification::config::VerificationConfig;
use serde_json::json;
use std::fs;
use std::path::Path;

const TASK: &str = "update module_1 parser";

fn fixture(root: &Path) {
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/module_1.rs"),
        "pub fn parse_1(value: u32) -> u32 {\n    value + 1\n}\n",
    )
    .unwrap();
    fs::write(root.join("src/module_2.rs"), "pub fn other() {}\n").unwrap();
    // Enough repository material that the 40% ratio cap is not trivially
    // binding on the required fields.
    for index in 3..40 {
        fs::write(
            root.join(format!("src/module_{index}.rs")),
            format!("pub fn helper_{index}() -> u32 {{\n    {index}\n}}\n"),
        )
        .unwrap();
    }
    fs::write(root.join("Cargo.toml"), "[package]\nname = \"fixture\"\n").unwrap();
}

fn verification() -> VerificationConfig {
    VerificationConfig::from_config(&json!({
        "verification": {
            "stages": {"normal": {"commands": [{"program": "cargo", "args": ["test"]}]}},
            "defaultStage": "normal"
        }
    }))
    .unwrap()
}

fn explore_json() -> String {
    json!({
        "goal": "make the parser safe",
        "constraints": ["never break the public API"],
        "findings": [
            {"summary": "parse_1 is recursive", "severity": "critical", "source": "src/module_1.rs"}
        ],
        "files": ["src/module_1.rs"],
        "symbols": [{"name": "parse_1"}]
    })
    .to_string()
}

fn failing_runner() -> FakeCaptureRunner {
    FakeCaptureRunner::new().with_failure(
        "cargo",
        &["test"],
        101,
        "error[E0308]: mismatched types\n --> src/module_1.rs:4:5\n",
    )
}

fn passing_runner() -> FakeCaptureRunner {
    FakeCaptureRunner::new().with_success(
        "cargo",
        &["test"],
        "test result: ok. 3 passed; 0 failed\n",
    )
}

#[test]
fn ordinary_activation_prepares_context_and_state() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let git = FakeGitHost::new();
    let clock = FixedClock::new(1_000);
    let controller = Controller::new(
        dir.path(),
        OrchestrationConfig::default(),
        ContextConfig::default(),
        CapabilityConfig::default(),
        verification(),
        &git,
        &clock,
    );
    let context = controller
        .prepare_lead_context("session-activation", TASK)
        .unwrap();
    assert!(context.dynamic_context.contains(TASK));
    assert!(context.metrics.model_dynamic_context_bytes > 0);
    let loaded = controller.load_state();
    assert!(loaded.exists);
    assert!(loaded.state.session("session-activation").is_some());
    let state_dir = opencode_gear::orchestration::state::state_dir(dir.path());
    assert!(state_dir.is_dir());
}

#[test]
fn explore_to_build_preserves_required_fields_and_checkpoints() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let git = FakeGitHost::new();
    let clock = FixedClock::new(1_000);
    let controller = Controller::new(
        dir.path(),
        OrchestrationConfig::default(),
        ContextConfig::default(),
        CapabilityConfig::default(),
        verification(),
        &git,
        &clock,
    );
    controller.prepare_lead_context("s", TASK).unwrap();
    let explore = controller
        .prepare_handoff("s", Role::Explore, TASK)
        .unwrap();
    assert_eq!(explore.destination, Role::Explore);
    let digest = controller
        .consume_explore_result("s", &explore_json())
        .unwrap();
    assert!(digest.structured);
    assert!(
        digest.checkpoint_id.is_some(),
        "an Explore→Build checkpoint must exist"
    );

    let build = controller.prepare_handoff("s", Role::Build, TASK).unwrap();
    assert_eq!(build.source, Role::Explore);
    assert_eq!(build.destination, Role::Build);
    assert_eq!(build.capsule.transition.as_str(), "explore_to_build");
    assert_eq!(build.capsule.goal.as_deref(), Some("make the parser safe"));
    assert!(build
        .capsule
        .hard_constraints
        .iter()
        .any(|constraint| constraint == "never break the public API"));
    assert!(build
        .capsule
        .findings
        .iter()
        .any(|finding| finding.severity == Severity::Critical));
    assert!(build
        .capsule
        .symbols
        .iter()
        .any(|symbol| symbol.name == "parse_1"));
    // Bounded by the configured runtime optimization envelope. Required
    // evidence is never dropped to satisfy it; the tight 4096/40 gate is a
    // fixture regression check, not a runtime rule.
    let envelope = OrchestrationConfig::default();
    assert!(build.capsule.measured_bytes() <= envelope.max_handoff_bytes);
    assert!(
        build.capsule.measured_bytes()
            <= build.metrics.rich_capsule_bytes as usize * envelope.max_handoff_ratio_percent / 100,
        "capsule {} rich {}",
        build.capsule.measured_bytes(),
        build.metrics.rich_capsule_bytes
    );
}

#[test]
fn build_to_verify_pass_has_no_debug() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let git = FakeGitHost::new();
    let clock = FixedClock::new(1_000);
    let controller = Controller::new(
        dir.path(),
        OrchestrationConfig::default(),
        ContextConfig::default(),
        CapabilityConfig::default(),
        verification(),
        &git,
        &clock,
    );
    controller.prepare_handoff("s", Role::Build, TASK).unwrap();
    let runner = passing_runner();
    let outcome = controller.after_build("s", &runner, None).unwrap();
    match outcome.decision {
        BuildDecision::Passed { verification, .. } => {
            assert_eq!(verification.outcome, "passed");
        }
        other => panic!("expected pass, got {other:?}"),
    }
    assert!(outcome.checkpoint_id.is_some());
    let state = controller.load_state();
    assert_eq!(
        state.state.session("s").unwrap().phase,
        OrchestrationPhase::Done
    );
}

#[test]
fn failure_escalates_through_bounded_retry_to_debug() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let git = FakeGitHost::new();
    let clock = FixedClock::new(1_000);
    let controller = Controller::new(
        dir.path(),
        OrchestrationConfig::default(),
        ContextConfig::default(),
        CapabilityConfig::default(),
        verification(),
        &git,
        &clock,
    );
    controller.prepare_handoff("s", Role::Build, TASK).unwrap();
    let runner = failing_runner();

    // Attempt 1 fails: bounded retry.
    let first = controller.after_build("s", &runner, None).unwrap();
    assert!(matches!(first.decision, BuildDecision::RetryBuild { .. }));
    // Attempt 2 fails: bounded retry.
    let second = controller.after_build("s", &runner, None).unwrap();
    assert!(matches!(second.decision, BuildDecision::RetryBuild { .. }));
    // Attempt 3 fails: budget exhausted, Debug is recommended with a reason.
    let third = controller.after_build("s", &runner, None).unwrap();
    match third.decision {
        BuildDecision::Debug {
            reason, handoff, ..
        } => {
            // The reason is explainable but never contains a configured
            // command string.
            assert!(reason.contains("failed"));
            assert!(
                !reason.contains("cargo"),
                "debug reason leaked a command: {reason}"
            );
            assert!(reason.contains("src/module_1.rs"), "{reason}");
            assert_eq!(handoff.destination, Role::Debug);
            // Debug carries failures/evidence/raw-log refs and a failing location.
            assert!(!handoff.capsule.failures.is_empty());
            assert!(handoff.capsule.findings.is_empty());
            assert!(handoff.capsule.files.is_empty());
            assert!(handoff
                .capsule
                .first_failing_location()
                .map(|location| location.display())
                .unwrap_or_default()
                .contains("src/module_1.rs"));
        }
        other => panic!("expected debug, got {other:?}"),
    }
}

#[test]
fn debug_retry_budget_escalates() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let git = FakeGitHost::new();
    let clock = FixedClock::new(1_000);
    let controller = Controller::new(
        dir.path(),
        OrchestrationConfig::default(),
        ContextConfig::default(),
        CapabilityConfig::default(),
        verification(),
        &git,
        &clock,
    );
    controller.prepare_handoff("s", Role::Build, TASK).unwrap();
    controller
        .after_build("s", &failing_runner(), None)
        .unwrap();
    // The default debug budget is one hand-off; a second escalates.
    let first = controller.debug_handoff("s").unwrap();
    assert_eq!(first.destination, Role::Debug);
    let second = controller.debug_handoff("s").unwrap();
    assert!(second
        .stale_reasons
        .iter()
        .any(|reason| reason.contains("budget exhausted")));
}

#[test]
fn fail_then_success_is_a_pass() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let git = FakeGitHost::new();
    let clock = FixedClock::new(1_000);
    let controller = Controller::new(
        dir.path(),
        OrchestrationConfig::default(),
        ContextConfig::default(),
        CapabilityConfig::default(),
        verification(),
        &git,
        &clock,
    );
    controller.prepare_handoff("s", Role::Build, TASK).unwrap();
    let first = controller
        .after_build("s", &failing_runner(), None)
        .unwrap();
    assert!(matches!(first.decision, BuildDecision::RetryBuild { .. }));
    let second = controller
        .after_build("s", &passing_runner(), None)
        .unwrap();
    assert!(matches!(second.decision, BuildDecision::Passed { .. }));
}

#[test]
fn no_configured_verification_reports_not_configured() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let git = FakeGitHost::new();
    let clock = FixedClock::new(1_000);
    let empty =
        VerificationConfig::from_config(&json!({"verification": {"enabled": true}})).unwrap();
    let controller = Controller::new(
        dir.path(),
        OrchestrationConfig::default(),
        ContextConfig::default(),
        CapabilityConfig::default(),
        empty,
        &git,
        &clock,
    );
    controller.prepare_handoff("s", Role::Build, TASK).unwrap();
    let outcome = controller
        .after_build("s", &passing_runner(), None)
        .unwrap();
    match outcome.decision {
        BuildDecision::NotConfigured { note } => assert!(note.contains("no trusted verification")),
        other => panic!("expected not-configured, got {other:?}"),
    }
}

#[test]
fn stale_checkpoint_is_marked_not_reused() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let git = FakeGitHost::new();
    let clock = FixedClock::new(1_000);
    let controller = Controller::new(
        dir.path(),
        OrchestrationConfig::default(),
        ContextConfig::default(),
        CapabilityConfig::default(),
        verification(),
        &git,
        &clock,
    );
    controller
        .prepare_handoff("s", Role::Explore, TASK)
        .unwrap();
    let digest = controller
        .consume_explore_result("s", &explore_json())
        .unwrap();
    assert!(digest.checkpoint_id.is_some());
    // Change a source the checkpoint fingerprinted (the explore capsule only
    // records the task, so touch a file and force a git change instead).
    fs::write(
        dir.path().join("src/module_1.rs"),
        "pub fn parse_1() -> u32 { 2 }\n",
    )
    .unwrap();
    let build = controller.prepare_handoff("s", Role::Build, TASK).unwrap();
    // A stale checkpoint is surfaced as a projection note, never hidden.
    let stale_note = build
        .stale_reasons
        .iter()
        .chain(build.capsule.omitted.iter())
        .any(|note| {
            note.contains("stale") || note.contains("changed") || note.contains("git state")
        });
    assert!(
        stale_note,
        "stale checkpoint must be visible: {:?}",
        build.stale_reasons
    );
}

#[test]
fn corrupt_state_and_checkpoint_recover_fail_soft() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let state_path = opencode_gear::orchestration::state::state_path(dir.path());
    fs::create_dir_all(state_path.parent().unwrap()).unwrap();
    fs::write(&state_path, "{ this is not json").unwrap();

    let git = FakeGitHost::new();
    let clock = FixedClock::new(1_000);
    let controller = Controller::new(
        dir.path(),
        OrchestrationConfig::default(),
        ContextConfig::default(),
        CapabilityConfig::default(),
        verification(),
        &git,
        &clock,
    );
    let loaded = controller.load_state();
    assert!(loaded.corrupt);
    // The controller still works and rewrites valid state.
    let context = controller.prepare_lead_context("s", TASK).unwrap();
    assert!(!context.dynamic_context.is_empty());
    assert!(!controller.load_state().corrupt);

    // A corrupt checkpoint referenced by a session is ignored, not fatal.
    let checkpoints = opencode_gear::orchestration::checkpoint::checkpoints_dir(dir.path());
    fs::create_dir_all(&checkpoints).unwrap();
    fs::write(checkpoints.join("cp-bad.json"), "{not json").unwrap();
    let loaded = controller.load_state();
    let mut state = loaded.state;
    if let Some(session) = state.session("s").cloned() {
        let mut session = session;
        session.checkpoints.push("cp-bad".to_string());
        state.upsert(session, 1_001);
    }
    opencode_gear::orchestration::state::save(dir.path(), &state).unwrap();
    let handoff = controller.prepare_handoff("s", Role::Build, TASK).unwrap();
    assert!(!handoff.dynamic_context.is_empty());
}

#[test]
fn disabled_orchestration_emits_no_plugin_and_no_state() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let config = OrchestrationConfig {
        enabled: false,
        ..OrchestrationConfig::default()
    };
    let git = FakeGitHost::new();
    let clock = FixedClock::new(1_000);
    let controller = Controller::new(
        dir.path(),
        config.clone(),
        ContextConfig::default(),
        CapabilityConfig::default(),
        verification(),
        &git,
        &clock,
    );
    let runner = FakeCaptureRunner::new();
    let bridge = BridgeContext::new(&controller, &runner, TelemetryConfig::disabled());
    let value = bridge.dispatch("chat.message", &json!({"session_id": "s", "text": TASK}));
    assert_eq!(value["ok"], json!(false));
    assert_eq!(value["disabled"], json!(true));
    assert!(!opencode_gear::orchestration::state::state_dir(dir.path()).exists());

    // The generated config has no plugin entry when orchestration is disabled.
    let mut config_value = json!({});
    assert!(!opencode_gear::orchestration::plugin::has_ocg_plugin(
        &config_value
    ));
    opencode_gear::orchestration::plugin::inject_plugin(
        &mut config_value,
        "file:///tmp/ocg-orchestration.js",
    );
    assert!(opencode_gear::orchestration::plugin::has_ocg_plugin(
        &config_value
    ));
}

#[test]
fn v2_plugin_contract_uses_local_discovery_and_subagent_tool() {
    use opencode_gear::orchestration::plugin;

    let dir = tempfile::tempdir().unwrap();
    let path = plugin::materialize_v2_with(dir.path(), plugin::v2_plugin_source()).unwrap();
    assert_eq!(path, plugin::v2_plugin_path(dir.path()));
    assert!(path.is_file());
    assert_eq!(
        path.parent().unwrap().parent().unwrap(),
        plugin::v2_config_dir(dir.path())
    );

    // The v2 adapter is thin: no request-message Lead rewrite, and the
    // delegation hooks target the renamed `subagent` tool through the V2
    // `ctx.tool.hook("execute.before")` registration (the event carries the
    // tool name, unlike the V1 hook's `input`).
    let source = plugin::v2_plugin_source();
    assert!(source.contains("event.tool !== \"subagent\""));
    assert!(source.contains("ctx.tool.hook(\"execute.before\""));
    assert!(source.contains("ctx.session.hook(\"prompt\""));
    // The V2 runtime may be Node, so the bridge is spawned via
    // `node:child_process` with an exact argv and no shell.
    assert!(source.contains("import { spawn } from \"node:child_process\""));
    assert!(source.contains("shell: false"));
    assert!(!source.contains("output.message.agent"));
    assert!(!source.contains("enforceLeadContract"));
}

/// Every file under a directory, recursively.
fn files_under(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut result = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                result.push(path);
            }
        }
    }
    result.sort();
    result
}

/// The exact VM-observed failure: a V1 plugin artifact exists, then a V2
/// launch/config materialization occurs, and OpenCode 2 discovers the stale V1
/// adapter through a shared config root. The V2 config dir is dedicated, so the
/// runtime-visible plugin namespace must contain only a valid V2 plugin, and
/// legacy generated artifacts from earlier layouts must be migrated away.
#[test]
fn v2_materialization_never_exposes_v1_or_legacy_artifacts_to_the_runtime() {
    use opencode_gear::orchestration::plugin;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let state = opencode_gear::orchestration::state::state_dir(root);

    // A V1 launch materialized the V1 adapter at its own path.
    plugin::materialize(root).unwrap();
    // Legacy generated artifacts from earlier 0.3.0 builds: one sharing the V1
    // state root (holding V1 bytes — the observed collision) and one from a
    // double-applied orchestration state path.
    let legacy_shared = state.join("plugins");
    let legacy_nested = state.join("orchestration").join("plugins");
    fs::create_dir_all(&legacy_shared).unwrap();
    fs::create_dir_all(&legacy_nested).unwrap();
    fs::write(
        legacy_shared.join("ocg-orchestration.js"),
        plugin::plugin_source(),
    )
    .unwrap();
    fs::write(
        legacy_nested.join("ocg-orchestration.js"),
        plugin::v2_plugin_source(),
    )
    .unwrap();

    // A V2 launch/config materialization occurs.
    let path = plugin::materialize_v2_with(root, plugin::v2_plugin_source()).unwrap();
    assert_eq!(path, plugin::v2_plugin_path(root));

    // The runtime-visible plugin namespace (everything OpenCode 2 can discover
    // under OPENCODE_CONFIG_DIR) contains exactly one file: the valid V2
    // adapter. No V1 artifact, no legacy artifact.
    let config_dir = plugin::v2_config_dir(root);
    assert_eq!(
        path.parent().unwrap().parent().unwrap(),
        config_dir,
        "the adapter must live at <config dir>/plugins/"
    );
    let discovered = files_under(&config_dir);
    assert_eq!(discovered, vec![path.clone()], "V2 discovered namespace");
    let source = fs::read_to_string(&path).unwrap();
    assert_eq!(source, plugin::v2_plugin_source());
    assert!(source.contains("export default {"));
    assert!(source.contains("id: \"opencode-gear-orchestration\""));
    assert!(!source.contains("export const server"));

    // The V1 config root is not inside (or equal to) the V2 config root, so a
    // V2 runtime can never reach the V1 adapter through local discovery.
    assert!(!config_dir.join("plugin").exists());
    assert_ne!(config_dir, state);

    // Legacy generated artifacts are migrated away, directories included.
    assert!(!state.join("plugins").exists());
    assert!(!state.join("orchestration").exists());

    // The V1 artifact is untouched and still satisfies a V1 launch.
    assert_eq!(
        fs::read_to_string(plugin::plugin_path(root)).unwrap(),
        plugin::plugin_source()
    );
}

/// Legacy cleanup is scoped to OCG-generated files: a user-owned file at a
/// legacy location is never deleted, and its directory is never pruned.
#[test]
fn v2_legacy_migration_preserves_user_owned_files() {
    use opencode_gear::orchestration::plugin;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let state = opencode_gear::orchestration::state::state_dir(root);
    let legacy_shared = state.join("plugins");
    fs::create_dir_all(&legacy_shared).unwrap();
    let user_file = legacy_shared.join("ocg-orchestration.js");
    fs::write(
        &user_file,
        "// hand-written by the user\nexport default {};\n",
    )
    .unwrap();
    let sibling = legacy_shared.join("user-notes.txt");
    fs::write(&sibling, "keep me\n").unwrap();

    plugin::materialize_v2_with(root, plugin::v2_plugin_source()).unwrap();

    assert!(user_file.is_file(), "user-owned file must survive");
    assert_eq!(
        fs::read_to_string(&user_file).unwrap(),
        "// hand-written by the user\nexport default {};\n"
    );
    assert!(sibling.is_file());
    // The dedicated V2 config root is unaffected by the legacy directory.
    assert_eq!(
        files_under(&plugin::v2_config_dir(root)),
        vec![plugin::v2_plugin_path(root)]
    );
}

#[test]
fn secret_shaped_task_never_enters_handoff_or_telemetry() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let git = FakeGitHost::new();
    let clock = FixedClock::new(1_000);
    let controller = Controller::new(
        dir.path(),
        OrchestrationConfig::default(),
        ContextConfig::default(),
        CapabilityConfig::default(),
        verification(),
        &git,
        &clock,
    );
    let secret = format!("{}{}", concat!("sk", "-"), "A".repeat(40));
    let output = format!("goal: {secret}\nfindings:\n- {secret}\n");
    controller
        .prepare_handoff("s", Role::Explore, TASK)
        .unwrap();
    let digest = controller.consume_explore_result("s", &output).unwrap();
    let handoff = controller.prepare_handoff("s", Role::Build, TASK).unwrap();
    let serialized = serde_json::to_string(&handoff.capsule).unwrap();
    assert!(!serialized.contains(&secret), "{serialized}");
    assert!(!digest
        .findings
        .iter()
        .any(|finding| finding.summary.contains(&secret)));
}

#[test]
fn telemetry_records_orchestration_transitions_without_raw_content() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let git = FakeGitHost::new();
    let clock = FixedClock::new(1_000);
    let controller = Controller::new(
        dir.path(),
        OrchestrationConfig::default(),
        ContextConfig::default(),
        CapabilityConfig::default(),
        verification(),
        &git,
        &clock,
    );
    let runner = FakeCaptureRunner::new();
    let bridge = BridgeContext::new(&controller, &runner, TelemetryConfig::default());
    let value = bridge.dispatch("chat.message", &json!({"session_id": "s", "text": TASK}));
    assert_eq!(value["ok"], json!(true));

    let store = TelemetryStore::new(dir.path(), TelemetryConfig::default());
    let log = store.read();
    assert_eq!(log.events.len(), 1);
    let event = &log.events[0];
    assert_eq!(event.task_type.as_deref(), Some("orchestration"));
    assert_eq!(event.orchestration.phase.as_deref(), Some("idle"));
    assert!(event.orchestration.model_dynamic_context_bytes > 0);
    // The raw task text never lands in the event.
    let raw = fs::read_to_string(store.path()).unwrap();
    assert!(!raw.contains(TASK), "{raw}");

    let stats = TelemetryStats::collect(&store);
    assert_eq!(stats.aggregate.orchestration.transitions, 1);
    assert!(stats.render().contains("orchestration"));
}

#[test]
fn dynamic_suffix_is_deterministic_and_stable() {
    let make = || {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let git = FakeGitHost::new();
        let clock = FixedClock::new(1_000);
        let controller = Controller::new(
            dir.path(),
            OrchestrationConfig::default(),
            ContextConfig::default(),
            CapabilityConfig::default(),
            verification(),
            &git,
            &clock,
        );
        controller
            .prepare_handoff("s", Role::Build, TASK)
            .unwrap()
            .dynamic_context
    };
    assert_eq!(make(), make());
}

#[test]
fn bridge_handles_explore_and_build_lifecycle() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let git = FakeGitHost::new();
    let clock = FixedClock::new(1_000);
    let controller = Controller::new(
        dir.path(),
        OrchestrationConfig::default(),
        ContextConfig::default(),
        CapabilityConfig::default(),
        verification(),
        &git,
        &clock,
    );
    let runner = failing_runner();
    let bridge = BridgeContext::new(&controller, &runner, TelemetryConfig::disabled());

    let before = bridge.dispatch(
        "tool.execute.before",
        &json!({"session_id": "s", "args": {"subagent_type": "ocg-explore", "prompt": TASK}}),
    );
    assert_eq!(before["ok"], json!(true));
    assert!(before["context"]
        .as_str()
        .unwrap()
        .contains("ocg role handoff"));

    let after_explore = bridge.dispatch(
        "tool.execute.after",
        &json!({
            "session_id": "s",
            "args": {"subagent_type": "ocg-explore", "prompt": TASK},
            "result": {"output": explore_json()}
        }),
    );
    assert_eq!(after_explore["ok"], json!(true));
    assert!(after_explore["checkpoint"].is_string());

    let build_before = bridge.dispatch(
        "tool.execute.before",
        &json!({"session_id": "s", "args": {"subagent_type": "ocg-build", "prompt": TASK}}),
    );
    assert_eq!(build_before["ok"], json!(true));

    let build_after = bridge.dispatch(
        "tool.execute.after",
        &json!({
            "session_id": "s",
            "args": {"subagent_type": "ocg-build", "prompt": TASK},
            "result": {"output": "done"}
        }),
    );
    assert_eq!(build_after["ok"], json!(true));
    // The fake runner fails, so the feedback must mention verification and a retry.
    let context = build_after["context"].as_str().unwrap();
    assert!(context.contains("verification"), "{context}");
}

#[test]
fn fake_capture_output_marks_truncation() {
    // A guard that the controller never treats truncated output as a pass.
    let output = CapturedOutput {
        exit: opencode_gear::process::ProcessExit::Code(101),
        success: false,
        stdout: Vec::new(),
        stderr: b"error: boom\n".to_vec(),
        stdout_truncated: false,
        stderr_truncated: true,
        duration_ms: 1,
    };
    assert!(!output.success);
    assert!(output.truncated());
}

#[test]
fn distinct_explore_and_build_prompts_keep_findings_and_retry_budget() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let git = FakeGitHost::new();
    let clock = FixedClock::new(1_000);
    let controller = Controller::new(
        dir.path(),
        OrchestrationConfig::default(),
        ContextConfig::default(),
        CapabilityConfig::default(),
        verification(),
        &git,
        &clock,
    );
    controller.prepare_lead_context("s", TASK).unwrap();
    // Explore and Build get genuinely different delegated prompts.
    controller
        .prepare_handoff("s", Role::Explore, "explore the parser call paths")
        .unwrap();
    controller
        .consume_explore_result("s", &explore_json())
        .unwrap();

    let build = controller
        .prepare_handoff("s", Role::Build, "implement the parse_1 fix now")
        .unwrap();
    // The Explore finding survived the different Build prompt.
    assert!(
        build
            .capsule
            .findings
            .iter()
            .any(|finding| finding.severity == Severity::Critical),
        "findings must survive a differing Build prompt: {:?}",
        build.capsule.findings
    );
    assert_eq!(build.capsule.task, "implement the parse_1 fix now");
    assert_eq!(build.capsule.task_id, Controller::task_id(TASK));

    // Repeated Build retries use differing prompts but share one retry budget.
    let runner = failing_runner();
    let first = controller.after_build("s", &runner, None).unwrap();
    assert!(matches!(
        first.decision,
        BuildDecision::RetryBuild { attempt: 1, .. }
    ));
    controller
        .prepare_handoff("s", Role::Build, "retry the parse_1 fix with logging")
        .unwrap();
    let second = controller.after_build("s", &runner, None).unwrap();
    assert!(matches!(
        second.decision,
        BuildDecision::RetryBuild { attempt: 2, .. }
    ));
    controller
        .prepare_handoff("s", Role::Build, "retry the parse_1 fix once more")
        .unwrap();
    let third = controller.after_build("s", &runner, None).unwrap();
    assert!(
        matches!(third.decision, BuildDecision::Debug { .. }),
        "a bounded budget must reach Debug, got {:?}",
        third.decision
    );
    // The Explore findings are still in the session, not reset by the prompts.
    let state = controller.load_state();
    assert!(
        !state.state.session("s").unwrap().findings.is_empty(),
        "session findings must survive retries"
    );
    assert_eq!(state.state.session("s").unwrap().attempts.build, 3);
}

#[test]
fn chat_message_with_a_new_task_id_resets_the_session() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let git = FakeGitHost::new();
    let clock = FixedClock::new(1_000);
    let controller = Controller::new(
        dir.path(),
        OrchestrationConfig::default(),
        ContextConfig::default(),
        CapabilityConfig::default(),
        verification(),
        &git,
        &clock,
    );
    controller.prepare_lead_context("s", TASK).unwrap();
    controller
        .prepare_handoff("s", Role::Explore, "explore")
        .unwrap();
    controller
        .consume_explore_result("s", &explore_json())
        .unwrap();
    assert!(!controller
        .load_state()
        .state
        .session("s")
        .unwrap()
        .findings
        .is_empty());

    // A different user message is a new overall task and must reset.
    controller
        .prepare_lead_context("s", "unrelated follow-up task")
        .unwrap();
    let session = controller.load_state().state.session("s").cloned().unwrap();
    assert!(session.findings.is_empty());
    assert_eq!(session.attempts.build, 0);
    // A later hand-off re-initializes from the existing (reset) session.
    let build = controller
        .prepare_handoff("s", Role::Build, "build the unrelated task")
        .unwrap();
    assert!(build.capsule.findings.is_empty());
}

#[test]
fn debug_delegation_is_narrow_and_has_no_selected_source() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let git = FakeGitHost::new();
    let clock = FixedClock::new(1_000);
    let controller = Controller::new(
        dir.path(),
        OrchestrationConfig::default(),
        ContextConfig::default(),
        CapabilityConfig::default(),
        verification(),
        &git,
        &clock,
    );
    controller.prepare_handoff("s", Role::Build, TASK).unwrap();
    let runner = failing_runner();
    controller.after_build("s", &runner, None).unwrap();
    controller.after_build("s", &runner, None).unwrap();
    let third = controller.after_build("s", &runner, None).unwrap();
    assert!(matches!(third.decision, BuildDecision::Debug { .. }));

    // A normal plugin delegation to Debug uses the narrow projection.
    let debug = controller
        .prepare_handoff("s", Role::Debug, "diagnose the failing test")
        .unwrap();
    assert_eq!(debug.destination, Role::Debug);
    assert!(debug.capsule.goal.is_none());
    assert!(debug.capsule.findings.is_empty());
    assert!(debug.capsule.files.is_empty());
    assert!(debug.capsule.symbols.is_empty());
    assert!(debug.capsule.decisions.is_empty());
    assert!(!debug.capsule.failures.is_empty());
    assert!(debug.capsule.verification.is_some());
    assert!(
        !debug.dynamic_context.contains("selected source"),
        "Debug delegation must not append selected source: {}",
        debug.dynamic_context
    );
}

#[test]
fn secret_shaped_task_goal_constraint_slice_and_verification_are_dropped() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let secret = format!("{}{}", concat!("sk", "-"), "A".repeat(40));
    // Put a secret-shaped literal inside a selected symbol body so the context
    // engine slices it into the rich input.
    fs::write(
        dir.path().join("src/module_1.rs"),
        format!("pub fn parse_1() {{\n    let key = \"{secret}\";\n    let _ = key;\n}}\n"),
    )
    .unwrap();

    let git = FakeGitHost::new();
    let clock = FixedClock::new(1_000);
    let controller = Controller::new(
        dir.path(),
        OrchestrationConfig::default(),
        ContextConfig::default(),
        CapabilityConfig::default(),
        verification(),
        &git,
        &clock,
    );
    // The delegated task itself is secret-shaped.
    controller
        .prepare_lead_context("s", "update module_1 parse_1")
        .unwrap();
    let handoff = controller
        .prepare_handoff("s", Role::Build, &format!("update parse_1 with {secret}"))
        .unwrap();
    let serialized = serde_json::to_string(&handoff.capsule).unwrap();
    assert!(!serialized.contains(&secret), "{serialized}");
    assert!(
        !handoff.dynamic_context.contains(&secret),
        "dynamic context leaked"
    );
    // The source slice containing the secret was dropped.
    assert!(
        handoff
            .capsule
            .omitted
            .iter()
            .any(|note| note.contains("source slice")),
        "{:?}",
        handoff.capsule.omitted
    );

    // A secret in an Explore goal/constraint is dropped before projection.
    let structured = format!(
        "{{\"goal\": \"{secret}\", \"constraints\": [\"{secret}\"], \"findings\": [{{\"summary\": \"ok\"}}]}}"
    );
    controller.consume_explore_result("s", &structured).unwrap();
    let build = controller
        .prepare_handoff("s", Role::Build, "build it")
        .unwrap();
    let text = serde_json::to_string(&build.capsule).unwrap();
    assert!(!text.contains(&secret), "{text}");

    // A secret in verification distilled output is dropped from the Build→Verify
    // capsule.
    let secret_runner = FakeCaptureRunner::new().with_failure(
        "cargo",
        &["test"],
        101,
        &format!("error: leaking {secret} at src/module_1.rs:4:5\n"),
    );
    controller
        .prepare_handoff("s", Role::Build, "build it")
        .unwrap();
    let outcome = controller.after_build("s", &secret_runner, None).unwrap();
    let verify = outcome
        .verify_handoff
        .as_ref()
        .expect("Build→Verify capsule");
    let verify_text = serde_json::to_string(verify).unwrap();
    assert!(!verify_text.contains(&secret), "{verify_text}");
}

#[test]
fn bridge_varying_prompts_participate_in_checkpoints_and_telemetry() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let git = FakeGitHost::new();
    let clock = FixedClock::new(1_000);
    let controller = Controller::new(
        dir.path(),
        OrchestrationConfig::default(),
        ContextConfig::default(),
        CapabilityConfig::default(),
        verification(),
        &git,
        &clock,
    );
    let runner = failing_runner();
    let bridge = BridgeContext::new(&controller, &runner, TelemetryConfig::default());

    bridge.dispatch("chat.message", &json!({"session_id": "s", "text": TASK}));
    bridge.dispatch(
        "tool.execute.before",
        &json!({"session_id": "s", "args": {"subagent_type": "ocg-explore", "prompt": "explore prompt"}}),
    );
    bridge.dispatch(
        "tool.execute.after",
        &json!({
            "session_id": "s",
            "args": {"subagent_type": "ocg-explore", "prompt": "explore prompt"},
            "result": {"output": explore_json()}
        }),
    );
    // Three Build attempts with three different prompts.
    for (index, prompt) in ["build one", "build two", "build three"].iter().enumerate() {
        let before = bridge.dispatch(
            "tool.execute.before",
            &json!({"session_id": "s", "args": {"subagent_type": "ocg-build", "prompt": prompt}}),
        );
        assert_eq!(before["ok"], json!(true), "build before {index}");
        let after = bridge.dispatch(
            "tool.execute.after",
            &json!({
                "session_id": "s",
                "args": {"subagent_type": "ocg-build", "prompt": prompt},
                "result": {"output": "done"}
            }),
        );
        assert_eq!(after["ok"], json!(true), "build after {index}");
    }

    // Build→Verify and Verify→Debug checkpoints must exist.
    let (summaries, corrupt) = opencode_gear::orchestration::checkpoint::list(dir.path());
    assert_eq!(corrupt, 0);
    let phases: Vec<&str> = summaries
        .iter()
        .map(|summary| summary.phase.as_str())
        .collect();
    assert!(phases.contains(&"build_to_verify"), "{phases:?}");
    assert!(phases.contains(&"verify_to_debug"), "{phases:?}");

    // Telemetry recorded the phase transitions (including build/verify/debug).
    let store = TelemetryStore::new(dir.path(), TelemetryConfig::default());
    let stats = TelemetryStats::collect(&store);
    assert!(stats.aggregate.orchestration.transitions >= 6);
    assert!(stats.aggregate.orchestration.build_attempts >= 1);
    assert!(stats.aggregate.orchestration.debug_attempts >= 1);
    let raw = fs::read_to_string(store.path()).unwrap();
    assert!(!raw.contains(TASK), "raw task text must not be stored");
}

/// A FakeGitHost describing a real repository with a status entry and a diff.
fn fake_repo(root: &Path, status: &str, diff: &str) -> FakeGitHost {
    FakeGitHost::new()
        .with_stdout(&["rev-parse", "--is-inside-work-tree"], "true\n")
        .with_stdout(
            &["rev-parse", "--show-toplevel"],
            &format!("{}\n", root.display()),
        )
        .with_stdout(&["rev-parse", "HEAD"], "abc123\n")
        .with_stdout(&["symbolic-ref", "--short", "-q", "HEAD"], "main\n")
        .with_stdout(
            &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
            status,
        )
        .with_stdout(&["ls-files", "-s", "-z"], "")
        .with_stdout(
            &["diff", "--no-color", "--no-ext-diff", "--unified=3"],
            diff,
        )
}

const REAL_DIFF: &str = "diff --git a/src/module_1.rs b/src/module_1.rs\n--- a/src/module_1.rs\n+++ b/src/module_1.rs\n@@ -1,2 +1,2 @@\n-old_parse()\n+new_parse()\n";

#[test]
fn debug_to_build_is_a_real_transition_with_evidence_and_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let git = fake_repo(dir.path(), " M src/module_1.rs\0", REAL_DIFF);
    let clock = FixedClock::new(1_000);
    let controller = Controller::new(
        dir.path(),
        OrchestrationConfig::default(),
        ContextConfig::default(),
        CapabilityConfig::default(),
        verification(),
        &git,
        &clock,
    );
    controller.prepare_lead_context("s", TASK).unwrap();
    controller
        .prepare_handoff("s", Role::Explore, "explore prompt")
        .unwrap();
    controller
        .consume_explore_result("s", &explore_json())
        .unwrap();

    // Fail Build three times to reach the Debug recommendation.
    controller
        .prepare_handoff("s", Role::Build, "build it")
        .unwrap();
    let runner = failing_runner();
    for _ in 0..3 {
        controller.after_build("s", &runner, None).unwrap();
    }
    let debug = controller
        .prepare_handoff("s", Role::Debug, "diagnose the failure")
        .unwrap();
    assert_eq!(debug.capsule.transition.as_str(), "verify_to_debug");

    // Returning to Build is a distinct typed transition.
    let back = controller
        .prepare_handoff("s", Role::Build, "apply the root-cause fix")
        .unwrap();
    assert_eq!(back.source, Role::Debug);
    assert_eq!(back.destination, Role::Build);
    assert_eq!(back.capsule.transition.as_str(), "debug_to_build");
    // Fix feedback (failures + verification) survives; raw unrelated logs do not.
    assert!(!back.capsule.failures.is_empty());
    assert!(back.capsule.verification.is_some());
    assert!(back.capsule.raw_log_refs.is_empty());
    // The recorded constraint survives as a fix constraint.
    assert!(back
        .capsule
        .hard_constraints
        .iter()
        .any(|constraint| constraint == "never break the public API"));

    let (summaries, corrupt) = opencode_gear::orchestration::checkpoint::list(dir.path());
    assert_eq!(corrupt, 0);
    assert!(
        summaries
            .iter()
            .any(|summary| summary.phase.as_str() == "debug_to_build"),
        "Debug→Build checkpoint must exist: {:?}",
        summaries
            .iter()
            .map(|s| s.phase.as_str())
            .collect::<Vec<_>>()
    );
}

#[test]
fn build_to_verify_carries_a_real_diff_and_excludes_unrelated_or_sensitive_content() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    // A real diff with a sensitive path and a secret-shaped body line alongside
    // the legitimate change.
    let secret = format!("{}{}", concat!("sk", "-"), "A".repeat(40));
    let status = " M src/module_1.rs\0 M .env\0";
    let diff = format!(
        "diff --git a/src/module_1.rs b/src/module_1.rs\n--- a/src/module_1.rs\n+++ b/src/module_1.rs\n@@ -1,2 +1,2 @@\n-old_parse()\n+new_parse()\ndiff --git a/.env b/.env\n--- a/.env\n+++ b/.env\n@@ -1,1 +1,1 @@\n+TOKEN={secret}\n"
    );
    let git = fake_repo(dir.path(), status, &diff);
    let clock = FixedClock::new(1_000);
    let controller = Controller::new(
        dir.path(),
        OrchestrationConfig::default(),
        ContextConfig::default(),
        CapabilityConfig::default(),
        verification(),
        &git,
        &clock,
    );
    controller.prepare_lead_context("s", TASK).unwrap();
    controller
        .prepare_handoff("s", Role::Explore, "explore prompt")
        .unwrap();
    // An unrelated exploratory finding that must not leak into Verify.
    let unrelated = "unrelated speculative exploration note";
    let structured = json!({
        "goal": "make the parser safe",
        "constraints": ["never break the public API"],
        "findings": [{"summary": unrelated, "severity": "critical"}],
    })
    .to_string();
    controller.consume_explore_result("s", &structured).unwrap();
    controller
        .prepare_handoff("s", Role::Build, "build it")
        .unwrap();

    let outcome = controller
        .after_build("s", &failing_runner(), None)
        .unwrap();
    let verify = outcome
        .verify_handoff
        .as_ref()
        .expect("Build→Verify capsule");
    // The real changed path and hunk survive.
    let diff_context = verify
        .diff_context
        .as_deref()
        .expect("Verify must carry a bounded diff");
    assert!(diff_context.contains("src/module_1.rs"), "{diff_context}");
    assert!(
        diff_context.contains("new_parse") || diff_context.contains("@@ -1,2 +1,2 @@"),
        "{diff_context}"
    );
    // A real content fingerprint, not a fabricated task id.
    assert!(verify
        .diff_ref
        .as_deref()
        .map(|reference| reference.starts_with("sha256:"))
        .unwrap_or(false));
    // The exploratory finding is excluded from Verify.
    let text = serde_json::to_string(verify).unwrap();
    assert!(!text.contains(unrelated), "{text}");
    assert!(verify.findings.is_empty());
    // Sensitive path and secret content are excluded.
    assert!(!diff_context.contains(".env"), "{diff_context}");
    assert!(!diff_context.contains(&secret), "{diff_context}");
    assert!(!text.contains(&secret), "{text}");
}

#[test]
fn explore_handoffs_include_the_response_contract_but_others_do_not() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let git = FakeGitHost::new();
    let clock = FixedClock::new(1_000);
    let controller = Controller::new(
        dir.path(),
        OrchestrationConfig::default(),
        ContextConfig::default(),
        CapabilityConfig::default(),
        verification(),
        &git,
        &clock,
    );
    controller.prepare_lead_context("s", TASK).unwrap();
    let explore = controller
        .prepare_handoff("s", Role::Explore, "explore prompt")
        .unwrap();
    assert!(
        explore
            .dynamic_context
            .contains("Explore response contract"),
        "{}",
        explore.dynamic_context
    );
    let deep = controller
        .prepare_handoff("s", Role::ExploreDeep, "deep explore prompt")
        .unwrap();
    assert!(deep.dynamic_context.contains("Explore response contract"));
    let build = controller
        .prepare_handoff("s", Role::Build, "build prompt")
        .unwrap();
    assert!(!build.dynamic_context.contains("Explore response contract"));
}

#[test]
fn debug_budget_is_enforced_through_the_bridge_path() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let git = FakeGitHost::new();
    let clock = FixedClock::new(1_000);
    let controller = Controller::new(
        dir.path(),
        OrchestrationConfig::default(),
        ContextConfig::default(),
        CapabilityConfig::default(),
        verification(),
        &git,
        &clock,
    );
    let runner = failing_runner();
    let bridge = BridgeContext::new(&controller, &runner, TelemetryConfig::disabled());

    bridge.dispatch("chat.message", &json!({"session_id": "s", "text": TASK}));
    bridge.dispatch(
        "tool.execute.before",
        &json!({"session_id": "s", "args": {"subagent_type": "ocg-build", "prompt": "build"}}),
    );
    // Fail the build budget so Debug is recommended.
    for _ in 0..3 {
        bridge.dispatch(
            "tool.execute.after",
            &json!({
                "session_id": "s",
                "args": {"subagent_type": "ocg-build", "prompt": "build"},
                "result": {"output": "done"}
            }),
        );
    }

    // First Debug delegation consumes attempt 1 and is not escalated.
    let first = bridge.dispatch(
        "tool.execute.before",
        &json!({"session_id": "s", "args": {"subagent_type": "ocg-debug", "prompt": "diagnose"}}),
    );
    assert_eq!(first["ok"], json!(true));
    assert_eq!(first["destination"], json!("debug"));
    assert!(!first["context"]
        .as_str()
        .unwrap()
        .contains("user escalation required"));

    // Second Debug delegation exceeds the default max (1) and escalates.
    let second = bridge.dispatch(
        "tool.execute.before",
        &json!({"session_id": "s", "args": {"subagent_type": "ocg-debug", "prompt": "diagnose again"}}),
    );
    assert_eq!(second["ok"], json!(true));
    assert!(
        second["context"]
            .as_str()
            .unwrap()
            .contains("user escalation required"),
        "{}",
        second["context"]
    );
    let state = controller.load_state();
    assert_eq!(state.state.session("s").unwrap().attempts.debug, 2);
}

#[test]
fn secret_user_message_is_not_persisted_in_state_or_checkpoints() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let secret = format!("{}{}", concat!("sk", "-"), "A".repeat(40));
    let message = format!("update parse_1 with {secret} please");
    let git = FakeGitHost::new();
    let clock = FixedClock::new(1_000);
    let controller = Controller::new(
        dir.path(),
        OrchestrationConfig::default(),
        ContextConfig::default(),
        CapabilityConfig::default(),
        verification(),
        &git,
        &clock,
    );
    let context = controller.prepare_lead_context("s", &message).unwrap();
    assert!(!context.dynamic_context.contains(&secret));
    controller
        .prepare_handoff("s", Role::Explore, &message)
        .unwrap();
    controller
        .consume_explore_result("s", "{\"goal\":\"ok\"}")
        .unwrap();

    let state_text =
        fs::read_to_string(opencode_gear::orchestration::state::state_path(dir.path())).unwrap();
    assert!(
        !state_text.contains(&secret),
        "state leaked the secret: {state_text}"
    );
    let checkpoints = opencode_gear::orchestration::checkpoint::checkpoints_dir(dir.path());
    for entry in fs::read_dir(&checkpoints).unwrap() {
        let path = entry.unwrap().path();
        let text = fs::read_to_string(&path).unwrap_or_default();
        assert!(
            !text.contains(&secret),
            "checkpoint {} leaked the secret",
            path.display()
        );
    }
}

// ---- Repeated-context (snapshot deduplication) regressions -----------------

fn lead_controller<'a>(
    root: &'a Path,
    git: &'a FakeGitHost,
    clock: &'a FixedClock,
) -> Controller<'a> {
    Controller::new(
        root,
        OrchestrationConfig::default(),
        ContextConfig::default(),
        CapabilityConfig::default(),
        verification(),
        git,
        clock,
    )
}

fn dispatch_chat(bridge: &BridgeContext<'_>, session: &str, text: &str) -> serde_json::Value {
    bridge.dispatch(
        "chat.message",
        &json!({"session_id": session, "text": text}),
    )
}

/// A. First prompt injects the full snapshot.
/// B/C. Later prompts with an unchanged repository snapshot are deduplicated.
/// E. A distinct session still receives its own initial context.
#[test]
fn lead_context_snapshot_is_injected_once_and_deduplicated_per_session() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let git = FakeGitHost::new();
    let clock = FixedClock::new(1_000);
    let controller = lead_controller(dir.path(), &git, &clock);
    let runner = FakeCaptureRunner::new();
    let bridge = BridgeContext::new(&controller, &runner, TelemetryConfig::disabled());

    // A. The first prompt injects the full repository snapshot plus metadata.
    let first = dispatch_chat(&bridge, "lead-1", TASK);
    assert_eq!(first["ok"], json!(true));
    assert_eq!(first["cached"], json!(false));
    let first_context = first["context"].as_str().unwrap();
    assert!(first_context.contains(TASK));
    let first_id = first["snapshot_id"].as_str().unwrap().to_string();
    assert!(first_id.starts_with("sha256:"), "{first_id}");
    assert!(first["bytes"].as_u64().unwrap() > 0);
    assert_eq!(
        first["estimated_tokens"].as_u64().unwrap() as usize,
        first["bytes"].as_u64().unwrap() as usize / 4
    );
    assert!(first["file_count"].as_u64().unwrap() > 0);
    assert!(first["symbol_count"].as_u64().unwrap() > 0);
    // The baseline generation is persisted on the session.
    let session = controller
        .load_state()
        .state
        .session("lead-1")
        .cloned()
        .unwrap();
    assert_eq!(
        session.repository_generation_id.as_deref(),
        Some(first_id.as_str())
    );

    // B. The second prompt with an unchanged repository returns no context.
    let second = dispatch_chat(&bridge, "lead-1", TASK);
    assert_eq!(second["ok"], json!(true));
    assert_eq!(second["cached"], json!(true));
    assert_eq!(second["context"], json!(""));
    assert_eq!(second["snapshot_id"], json!(first_id));

    // C. The third prompt stays deduplicated.
    let third = dispatch_chat(&bridge, "lead-1", TASK);
    assert_eq!(third["cached"], json!(true));
    assert_eq!(third["context"], json!(""));

    // E. A distinct session gets its own initial snapshot. The identity is a
    // property of the repository snapshot, so it is legitimately the same.
    let other = dispatch_chat(&bridge, "lead-2", TASK);
    assert_eq!(other["cached"], json!(false));
    assert!(!other["context"].as_str().unwrap().is_empty());
    assert_eq!(other["snapshot_id"], json!(first_id));
}

/// D. A material repository change produces a new identity and a fresh snapshot.
#[test]
fn lead_context_snapshot_changes_when_repository_context_changes() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let git = FakeGitHost::new();
    let clock = FixedClock::new(1_000);
    let controller = lead_controller(dir.path(), &git, &clock);
    let runner = FakeCaptureRunner::new();
    let bridge = BridgeContext::new(&controller, &runner, TelemetryConfig::disabled());

    let first = dispatch_chat(&bridge, "lead-1", TASK);
    assert_eq!(first["cached"], json!(false));
    let first_id = first["snapshot_id"].as_str().unwrap().to_string();
    assert!(first["context"].as_str().unwrap().contains("parse_1"));

    // Edit the selected file so the repository snapshot materially changes.
    fs::write(
        dir.path().join("src/module_1.rs"),
        "pub fn parse_1(value: u32) -> u32 {\n    value + 2\n}\n// changed\n",
    )
    .unwrap();

    let second = dispatch_chat(&bridge, "lead-1", TASK);
    assert_eq!(second["ok"], json!(true));
    assert_eq!(second["cached"], json!(false));
    let second_id = second["snapshot_id"].as_str().unwrap().to_string();
    assert_ne!(second_id, first_id, "changed context must change identity");
    assert!(!second["context"].as_str().unwrap().is_empty());

    // The next unchanged prompt is deduplicated against the new baseline.
    let third = dispatch_chat(&bridge, "lead-1", TASK);
    assert_eq!(third["cached"], json!(true));
    assert_eq!(third["context"], json!(""));
    assert_eq!(third["snapshot_id"], json!(second_id));
}

/// F. Deduplication is per session: a Worker hand-off in a distinct session is
/// never suppressed by the Lead session's cached snapshot.
#[test]
fn worker_handoff_is_not_suppressed_by_lead_snapshot_deduplication() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let git = FakeGitHost::new();
    let clock = FixedClock::new(1_000);
    let controller = lead_controller(dir.path(), &git, &clock);
    let runner = FakeCaptureRunner::new();
    let bridge = BridgeContext::new(&controller, &runner, TelemetryConfig::disabled());

    // Cache the Lead session's snapshot.
    let first = dispatch_chat(&bridge, "lead-1", TASK);
    assert_eq!(first["cached"], json!(false));
    let cached = dispatch_chat(&bridge, "lead-1", TASK);
    assert_eq!(cached["cached"], json!(true));
    assert_eq!(cached["context"], json!(""));

    // A Worker subagent delegation runs in its own session and still receives
    // its typed hand-off context even though the Lead snapshot is cached.
    let worker = bridge.dispatch(
        "tool.execute.before",
        &json!({
            "session_id": "worker-1",
            "args": {"subagent_type": "ocg-explore", "prompt": "explore the parser"}
        }),
    );
    assert_eq!(worker["ok"], json!(true));
    assert_eq!(worker["destination"], json!("explore"));
    assert!(!worker["context"].as_str().unwrap().is_empty());

    // A distinct Lead session also still gets its own initial snapshot.
    let other_lead = dispatch_chat(&bridge, "lead-2", TASK);
    assert_eq!(other_lead["cached"], json!(false));
    assert!(!other_lead["context"].as_str().unwrap().is_empty());
}

/// A differently phrased follow-up is a new task id but the same effective
/// repository snapshot, so the per-session identity is preserved across the
/// task reset and the snapshot is not re-appended.
#[test]
fn lead_context_snapshot_dedup_survives_a_new_task_message() {
    let dir = tempfile::tempdir().unwrap();
    // An empty repository keeps the rendered snapshot body stable across
    // messages, isolating the per-session identity from task-aware ranking.
    let git = FakeGitHost::new();
    let clock = FixedClock::new(1_000);
    let controller = lead_controller(dir.path(), &git, &clock);
    let runner = FakeCaptureRunner::new();
    let bridge = BridgeContext::new(&controller, &runner, TelemetryConfig::disabled());

    let first = dispatch_chat(&bridge, "lead-1", "first task");
    assert_eq!(first["cached"], json!(false));
    let first_id = first["snapshot_id"].as_str().unwrap().to_string();

    let second = dispatch_chat(&bridge, "lead-1", "a differently phrased follow-up task");
    assert_ne!(second["task_id"], first["task_id"]);
    assert_eq!(second["cached"], json!(true));
    assert_eq!(second["context"], json!(""));
    assert_eq!(second["snapshot_id"], json!(first_id));
}

/// A materially different task in the same session must not append another full
/// repository baseline. Task-aware ranking may differ, but the session baseline
/// is scoped to the indexed repository generation, not the task wording.
#[test]
fn lead_context_snapshot_suppresses_different_task() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let git = FakeGitHost::new();
    let clock = FixedClock::new(1_000);
    let controller = lead_controller(dir.path(), &git, &clock);
    let runner = FakeCaptureRunner::new();
    let bridge = BridgeContext::new(&controller, &runner, TelemetryConfig::disabled());

    let first = dispatch_chat(&bridge, "lead-1", "update module_1 parser");
    assert_eq!(first["cached"], json!(false));
    assert!(!first["context"].as_str().unwrap().is_empty());
    let first_id = first["snapshot_id"].as_str().unwrap().to_string();

    let second = dispatch_chat(&bridge, "lead-1", "refactor module_2 helper");
    assert_ne!(second["task_id"], first["task_id"]);
    assert_eq!(second["cached"], json!(true));
    assert_eq!(second["context"], json!(""));
    assert_eq!(second["snapshot_id"], json!(first_id));
}

/// Returning to a previously seen task/context still does not append another
/// full repository baseline (A -> B -> A).
#[test]
fn lead_context_snapshot_suppresses_a_b_a() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let git = FakeGitHost::new();
    let clock = FixedClock::new(1_000);
    let controller = lead_controller(dir.path(), &git, &clock);
    let runner = FakeCaptureRunner::new();
    let bridge = BridgeContext::new(&controller, &runner, TelemetryConfig::disabled());

    let a = dispatch_chat(&bridge, "lead-1", "update module_1 parser");
    assert_eq!(a["cached"], json!(false));
    let baseline = a["snapshot_id"].as_str().unwrap().to_string();

    let b = dispatch_chat(&bridge, "lead-1", "refactor module_2 helper");
    assert_eq!(b["cached"], json!(true));
    assert_eq!(b["context"], json!(""));

    let a2 = dispatch_chat(&bridge, "lead-1", "update module_1 parser");
    assert_eq!(a2["cached"], json!(true));
    assert_eq!(a2["context"], json!(""));
    assert_eq!(a2["snapshot_id"], json!(baseline));
}

/// Four different task-aware projections must produce exactly one persistent
/// full repository baseline (A -> B -> C -> D).
#[test]
fn lead_context_snapshot_suppresses_a_b_c_d() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let git = FakeGitHost::new();
    let clock = FixedClock::new(1_000);
    let controller = lead_controller(dir.path(), &git, &clock);
    let runner = FakeCaptureRunner::new();
    let bridge = BridgeContext::new(&controller, &runner, TelemetryConfig::disabled());

    let tasks = [
        "update module_1 parser",
        "refactor module_2 helper",
        "document module_3 logic",
        "test module_4 behavior",
    ];
    let mut baseline: Option<String> = None;
    for (index, task) in tasks.iter().enumerate() {
        let result = dispatch_chat(&bridge, "lead-1", task);
        if index == 0 {
            assert_eq!(result["cached"], json!(false));
            assert!(!result["context"].as_str().unwrap().is_empty());
            baseline = Some(result["snapshot_id"].as_str().unwrap().to_string());
        } else {
            assert_eq!(
                result["cached"],
                json!(true),
                "task {task} should reuse baseline"
            );
            assert_eq!(result["context"], json!(""));
            assert_eq!(result["snapshot_id"], json!(baseline.as_ref().unwrap()));
        }
    }
}

/// A trivial third-turn message must not append another full repository
/// baseline. This is the exact live failure shape that motivated the fix.
#[test]
fn lead_context_snapshot_suppresses_trivial_third_turn() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let git = FakeGitHost::new();
    let clock = FixedClock::new(1_000);
    let controller = lead_controller(dir.path(), &git, &clock);
    let runner = FakeCaptureRunner::new();
    let bridge = BridgeContext::new(&controller, &runner, TelemetryConfig::disabled());

    let first = dispatch_chat(
        &bridge,
        "lead-1",
        "Use the ocg-explore subagent exactly once.",
    );
    assert_eq!(first["cached"], json!(false));
    assert!(!first["context"].as_str().unwrap().is_empty());
    let baseline = first["snapshot_id"].as_str().unwrap().to_string();

    let second = dispatch_chat(
        &bridge,
        "lead-1",
        "After the subagent returns, reply with PROJECT=fixture",
    );
    assert_eq!(second["cached"], json!(true));
    assert_eq!(second["context"], json!(""));

    let third = dispatch_chat(&bridge, "lead-1", "Reply with exactly: TURN3_OK");
    assert_eq!(third["cached"], json!(true));
    assert_eq!(third["context"], json!(""));
    assert_eq!(third["snapshot_id"], json!(baseline));
}

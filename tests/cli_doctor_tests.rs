//! CLI tests for the `ocg doctor` sections added with local telemetry:
//! repository map/index, symbol index, context cache, checkpoints, verification,
//! telemetry and capability planning. Doctor is strictly read-only.

mod common;

use common::TestDir;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn base_command(cwd: &Path, work: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ocg"));
    command
        .current_dir(cwd)
        .env("OPENCODE_GEAR_USER_CONFIG", work.join("no-user.json"))
        .env_remove("OPENCODE_GEAR_PROJECT_CONFIG")
        .env_remove("OPENCODE_GEAR_THROTTLE")
        .env_remove("OPENCODE_GEAR_HOME")
        .env_remove("OPENCODE_GEAR_TRACE")
        .env_remove("OPENCODE_GEAR_OPENCODE")
        .env_remove("OPENCODE_GEAR_OPENCODE_BIN")
        .env_remove("OC_GEAR_OPENCODE_BIN")
        .env_remove("OPENCODE_GEAR_TELEMETRY");
    command
}

fn run(cwd: &Path, work: &Path, args: &[&str]) -> Output {
    base_command(cwd, work)
        .args(args)
        .output()
        .expect("run ocg")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn write_project_file(root: &Path, relative: &str, content: &str) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

#[test]
fn doctor_reports_the_new_sections_and_creates_nothing() {
    let dir = TestDir::new();
    let project = dir.project();
    let project_arg = project.to_string_lossy().into_owned();
    let output = run(
        dir.path(),
        dir.path(),
        &["--project", &project_arg, "doctor"],
    );
    assert!(output.status.success(), "{}", stdout(&output));
    let text = stdout(&output);
    for label in [
        "repository map/index",
        "symbol index",
        "context cache",
        "task checkpoints",
        "verification config",
        "telemetry",
        "tool capability planner",
        "sensitive-file exclusions",
    ] {
        assert!(text.contains(label), "missing '{label}' in\n{text}");
    }
    // Missing optional state is informational, never a failure.
    assert!(text.contains("not built"), "{text}");
    assert!(text.contains("none saved"), "{text}");
    assert!(text.contains("not present"), "{text}");
    assert!(
        !project.join(".opencode-gear").exists(),
        "doctor must not create index, cache, logs or telemetry"
    );
}

#[test]
fn doctor_warns_on_a_corrupt_index_without_failing_or_creating_state() {
    let dir = TestDir::new();
    let project = dir.project();
    write_project_file(&project, "src/lib.rs", "pub fn a() {}\n");
    write_project_file(
        &project,
        ".opencode-gear/index/context-index.json",
        "{not valid json",
    );
    let project_arg = project.to_string_lossy().into_owned();
    let output = run(
        dir.path(),
        dir.path(),
        &["--project", &project_arg, "doctor"],
    );
    assert!(
        output.status.success(),
        "a corrupt index is a warning, not a failure: {}",
        stdout(&output)
    );
    let text = stdout(&output);
    assert!(text.contains("repository map/index"), "{text}");
    assert!(text.contains("unreadable or corrupt"), "{text}");
    assert!(
        !project.join(".opencode-gear/telemetry").exists(),
        "doctor must not create telemetry"
    );
}

#[test]
fn doctor_warns_on_corrupt_telemetry_lines_but_does_not_fail() {
    let dir = TestDir::new();
    let project = dir.project();
    write_project_file(
        &project,
        ".opencode-gear/telemetry/events.jsonl",
        "not json at all\n",
    );
    let project_arg = project.to_string_lossy().into_owned();
    let output = run(
        dir.path(),
        dir.path(),
        &["--project", &project_arg, "doctor"],
    );
    assert!(output.status.success(), "{}", stdout(&output));
    let text = stdout(&output);
    assert!(text.contains("telemetry"), "{text}");
    assert!(text.contains("corrupt line"), "{text}");
    // The corrupt line itself is never echoed.
    assert!(!text.contains("not json at all"), "{text}");
}

#[test]
fn doctor_warns_on_unsupported_telemetry_schema_without_printing_the_event() {
    let dir = TestDir::new();
    let project = dir.project();
    write_project_file(
        &project,
        ".opencode-gear/telemetry/events.jsonl",
        "{\"schema_version\":999,\"timestamp\":1,\"task_id\":\"task-future\",\"outcome\":\"success\"}\n",
    );
    let project_arg = project.to_string_lossy().into_owned();
    let output = run(
        dir.path(),
        dir.path(),
        &["--project", &project_arg, "doctor"],
    );
    assert!(output.status.success(), "{}", stdout(&output));
    let text = stdout(&output);
    assert!(text.contains("unsupported schema line"), "{text}");
    assert!(!text.contains("task-future"), "{text}");
}

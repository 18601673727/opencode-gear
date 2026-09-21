//! CLI-level orchestration tests: plugin injection, the hidden bridge and the
//! disabled escape hatch.

mod common;

use common::{write_yaml, TestDir};
use serde_json::{json, Value};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

fn base_command(cwd: &Path, work: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ocg"));
    command
        .current_dir(cwd)
        .env("OPENCODE_GEAR_USER_CONFIG", work.join("no-user.yaml"))
        .env_remove("OPENCODE_GEAR_PROJECT_CONFIG")
        .env_remove("OPENCODE_GEAR_THROTTLE")
        .env_remove("OPENCODE_GEAR_HOME")
        .env_remove("OPENCODE_GEAR_TRACE")
        .env_remove("OPENCODE_GEAR_ORCHESTRATION")
        .env_remove("OC_GEAR_ORCHESTRATION")
        .env_remove("OC_GEAR_USER_CONFIG")
        .env_remove("OC_GEAR_PROJECT_CONFIG")
        .env_remove("OC_GEAR_THROTTLE")
        .env_remove("OC_GEAR_HOME")
        .env_remove("OC_GEAR_TRACE")
        .env_remove("OC_GEAR_OPENCODE_BIN")
        .env_remove("OPENCODE_GEAR_OPENCODE")
        .env_remove("OPENCODE_GEAR_OPENCODE_BIN")
        .env_remove("OPENCODE_GEAR_API_BASE")
        .env_remove("OPENCODE_GEAR_CACHE_DIR");
    command
}

fn bridge(cwd: &Path, work: &Path, event: &str, payload: &Value) -> Value {
    let mut child = base_command(cwd, work)
        .args(["__bridge", event, "--project", cwd.to_str().unwrap()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn bridge");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("bridge JSON")
}

fn project(dir: &TestDir) -> std::path::PathBuf {
    let project = dir.project();
    std::fs::write(project.join("parser.rs"), "pub fn parse() {}\n").unwrap();
    project
}

/// A fake `opencode` reporting a fixed version, so runtime-facing config
/// output resolves a deterministic runtime family regardless of whatever
/// OpenCode the host happens to have installed.
#[cfg(unix)]
fn fake_runtime(dir: &TestDir, name: &str, version: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let script = dir.join(name);
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo {version}; exit 0; fi\nexit 0\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script
}

#[cfg(unix)]
#[test]
fn enabled_build_injects_plugin_and_preserves_user_plugins() {
    let dir = TestDir::new();
    let project = project(&dir);
    write_yaml(
        &project.join(".opencode-gear.yaml"),
        &json!({"opencode": {"plugin": ["my-user-plugin"]}}),
    );
    // The V1 contract: the generated local adapter is injected as a file://
    // entry in the singular `plugin` array.
    let runtime = fake_runtime(&dir, "fake-opencode-v1", "1.18.31");
    let output = base_command(&project, dir.path())
        .env("OPENCODE_GEAR_OPENCODE", &runtime)
        .args(["build", "--pretty"])
        .output()
        .expect("run ocg");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let config: Value = serde_json::from_slice(&output.stdout).unwrap();
    let plugins = config["plugin"].as_array().expect("plugin array");
    let names: Vec<String> = plugins
        .iter()
        .map(|value| value.as_str().unwrap_or_default().to_string())
        .collect();
    assert!(names.iter().any(|name| name == "my-user-plugin"));
    assert!(names
        .iter()
        .any(|name| name.ends_with("ocg-orchestration.js") && name.starts_with("file://")));
}

#[cfg(unix)]
#[test]
fn disabled_orchestration_emits_no_plugin() {
    let dir = TestDir::new();
    let project = project(&dir);
    let runtime = fake_runtime(&dir, "fake-opencode-v1", "1.18.31");
    let output = base_command(&project, dir.path())
        .env("OPENCODE_GEAR_ORCHESTRATION", "0")
        .env("OPENCODE_GEAR_OPENCODE", &runtime)
        .args(["build"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let config: Value = serde_json::from_slice(&output.stdout).unwrap();
    let has_ocg = config
        .get("plugin")
        .and_then(Value::as_array)
        .map(|plugins| {
            plugins.iter().any(|value| {
                value
                    .as_str()
                    .map(|name| name.contains("ocg-orchestration.js"))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false);
    assert!(
        !has_ocg,
        "disabled orchestration must preserve the explicit no-hook path: {config}"
    );
    assert!(!project
        .join(".opencode-gear")
        .join("orchestration")
        .exists());
}

#[cfg(unix)]
#[test]
fn disabled_orchestration_launch_preserves_the_no_hook_path() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TestDir::new();
    let project = project(&dir);
    let marker = dir.join("launched");
    let script = dir.join("fake-opencode-no-hook.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 1.18.31; exit 0; fi\nif [ \"$1\" = \"models\" ]; then printf '%s\\n' openai/gpt-5.6-sol; exit 0; fi\nprintf launched > \"{}\"\n",
            marker.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let output = base_command(&project, dir.path())
        .env("OPENCODE_GEAR_ORCHESTRATION", "0")
        .env("OPENCODE_GEAR_OPENCODE", &script)
        .args(["run", "hello"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(marker.exists());
    assert!(
        !project.join(".opencode-gear").exists(),
        "the explicit no-hook launch must not materialize local plugin state"
    );
}

#[test]
fn bridge_chat_message_prepares_context_and_state() {
    let dir = TestDir::new();
    let project = project(&dir);
    let value = bridge(
        &project,
        dir.path(),
        "chat.message",
        &json!({"session_id": "cli-session", "text": "fix the parser"}),
    );
    assert_eq!(value["ok"], json!(true));
    assert!(value["context"]
        .as_str()
        .unwrap()
        .contains("fix the parser"));
    assert!(project
        .join(".opencode-gear")
        .join("orchestration")
        .join("state.json")
        .is_file());
}

#[test]
fn bridge_session_context_supplies_the_baseline_on_every_dispatch() {
    let dir = TestDir::new();
    let project = project(&dir);
    let payload = json!({"session_id": "cli-session", "text": "fix the parser"});
    let first = bridge(&project, dir.path(), "session.context", &payload);
    assert_eq!(first["ok"], json!(true));
    assert_eq!(first["event"], json!("session.context"));
    assert_eq!(first["cached"], json!(false));
    let body = first["context"].as_str().unwrap();
    assert!(body.contains("fix the parser"));

    // The V2 model-dispatch path never suppresses the baseline: a repeated
    // dispatch reuses the computation but still returns the full body.
    let second = bridge(&project, dir.path(), "session.context", &payload);
    assert_eq!(second["ok"], json!(true));
    assert_eq!(second["cached"], json!(true));
    assert_eq!(second["context"].as_str().unwrap(), body);
    assert_eq!(second["snapshot_id"], first["snapshot_id"]);
    assert_eq!(second["bytes"], first["bytes"]);
    assert!(second["bytes"].as_u64().unwrap() > 0);
    assert!(project
        .join(".opencode-gear")
        .join("orchestration")
        .join("state.json")
        .is_file());
}

#[test]
fn bridge_is_disabled_and_empty_when_orchestration_is_off() {
    let dir = TestDir::new();
    let project = project(&dir);
    let value = base_command(&project, dir.path())
        .env("OPENCODE_GEAR_ORCHESTRATION", "0")
        .args([
            "__bridge",
            "chat.message",
            "--project",
            project.to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            child
                .stdin
                .as_mut()
                .unwrap()
                .write_all(br#"{"session_id":"s","text":"hello"}"#)
                .unwrap();
            child.wait_with_output()
        })
        .unwrap();
    let value: Value = serde_json::from_slice(&value.stdout).unwrap();
    assert_eq!(value["ok"], json!(false));
    assert_eq!(value["disabled"], json!(true));
    assert!(!project.join(".opencode-gear").exists());
}

#[test]
fn bridge_unknown_event_is_soft() {
    let dir = TestDir::new();
    let project = project(&dir);
    let value = bridge(&project, dir.path(), "no.such.event", &json!({}));
    assert_eq!(value["ok"], json!(false));
}

#[cfg(unix)]
#[test]
fn launch_fails_clearly_when_the_plugin_cannot_be_materialized() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TestDir::new();
    let project = dir.project();
    std::fs::write(project.join("parser.rs"), "pub fn parse() {}\n").unwrap();
    // A file where the state directory must be makes materialization fail.
    std::fs::write(project.join(".opencode-gear"), b"not a directory").unwrap();

    let script = dir.join("fake-opencode.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 1.18.31; exit 0; fi\nif [ \"$1\" = \"models\" ]; then printf '%s\\n' openai/gpt-5.6-sol; exit 0; fi\necho launched\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let project_arg = project.to_string_lossy().into_owned();
    let output = base_command(dir.path(), dir.path())
        .env("OPENCODE_GEAR_OPENCODE", &script)
        .args(["--project", project_arg.as_str(), "run", "hello"])
        .output()
        .expect("run ocg");
    assert!(
        !output.status.success(),
        "a broken file:// integration must fail the launch, not warn"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.to_lowercase().contains("orchestration")
            || stderr.to_lowercase().contains("plugin")
            || stderr.to_lowercase().contains("directory"),
        "stderr should explain the materialization failure: {stderr}"
    );
}

fn bridge_with_stdin(
    cwd: &Path,
    work: &Path,
    event: &str,
    body: &str,
    orchestration: Option<&str>,
) -> Value {
    let mut command = base_command(cwd, work);
    if let Some(value) = orchestration {
        command.env("OPENCODE_GEAR_ORCHESTRATION", value);
    }
    let mut child = command
        .args(["__bridge", event, "--project", cwd.to_str().unwrap()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn bridge");
    let mut stdin = child.stdin.take().unwrap();
    let body = body.to_string();
    // Write in a thread: the bridge may stop reading at its cap and close the
    // pipe, which surfaces as a BrokenPipe on the writer. That is expected.
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(body.as_bytes());
    });
    let output = child.wait_with_output().unwrap();
    let _ = writer.join();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("bridge JSON")
}

#[test]
fn disabled_bridge_drains_large_and_repeated_payloads_without_a_race() {
    let dir = TestDir::new();
    let project = project(&dir);
    // Large (but under the 4 MiB cap) so the disabled path must still drain.
    let large = format!(
        "{{\"session_id\":\"s\",\"text\":\"{}\"}}",
        "x".repeat(1024 * 1024)
    );
    for _ in 0..8 {
        let value = bridge_with_stdin(&project, dir.path(), "chat.message", &large, Some("0"));
        assert_eq!(value["ok"], json!(false));
        assert_eq!(value["disabled"], json!(true));
    }
    assert!(!project.join(".opencode-gear").exists());
}

#[test]
fn oversized_bridge_payload_is_rejected_fail_soft_without_state() {
    let dir = TestDir::new();
    let project = project(&dir);
    let huge = "x".repeat(5 * 1024 * 1024);
    let value = bridge_with_stdin(&project, dir.path(), "chat.message", &huge, None);
    assert_eq!(value["ok"], json!(false));
    assert!(value["error"].as_str().unwrap_or_default().contains("cap"));
    assert!(!project.join(".opencode-gear").exists());
}

#[cfg(unix)]
#[test]
fn models_does_not_require_plugin_materialization_or_a_writable_project() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TestDir::new();
    let project = project(&dir);
    // A file where the state directory must be makes materialization fail.
    std::fs::write(project.join(".opencode-gear"), b"not a directory").unwrap();
    let marker = dir.join("models-marker");
    let script = dir.join("fake-opencode.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 1.18.31; exit 0; fi\nprintf ok > \"{}\"\n",
            marker.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let project_arg = project.to_string_lossy().into_owned();
    let output = base_command(dir.path(), dir.path())
        .env("OPENCODE_GEAR_OPENCODE", &script)
        .args(["--project", project_arg.as_str(), "models"])
        .output()
        .expect("run ocg models");
    assert!(
        output.status.success(),
        "ocg models must not require plugin materialization: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(marker.is_file(), "ocg models must still reach OpenCode");
    // The state path is untouched (still the original file).
    assert!(project.join(".opencode-gear").is_file());
}

#[cfg(unix)]
#[test]
fn launch_propagates_explicit_config_paths_to_the_bridge_environment() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TestDir::new();
    let project = project(&dir);
    let user_config = dir.join("explicit-user.yaml");
    let project_config = dir.join("explicit-project.yaml");
    write_yaml(&user_config, &json!({}));
    write_yaml(
        &project_config,
        &json!({"verification": {"stages": {"normal": {"commands": ["cargo check --locked"]}}}}),
    );
    let marker = dir.join("bridge-env");
    let script = dir.join("fake-opencode.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 1.18.31; exit 0; fi\nif [ \"$1\" = \"models\" ]; then printf '%s\\n' openai/gpt-5.6-sol; exit 0; fi\nprintf '%s\\n%s\\n' \"$OPENCODE_GEAR_USER_CONFIG\" \"$OPENCODE_GEAR_PROJECT_CONFIG\" > \"{}\"\n",
            marker.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let output = base_command(&project, dir.path())
        .env("OPENCODE_GEAR_OPENCODE", &script)
        .args([
            "--user-config",
            user_config.to_str().unwrap(),
            "--project-config",
            project_config.to_str().unwrap(),
            "run",
            "hello",
        ])
        .output()
        .expect("launch ocg");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let values = std::fs::read_to_string(marker).unwrap();
    let lines: Vec<&str> = values.lines().collect();
    assert_eq!(
        lines,
        [
            user_config.to_str().unwrap(),
            project_config.to_str().unwrap()
        ]
    );
}

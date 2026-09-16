//! End-to-end CLI tests for the `ocg` binary.

mod common;

use common::{write_json, TestDir};
use serde_json::{json, Value};
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
        .env_remove("OC_GEAR_USER_CONFIG")
        .env_remove("OC_GEAR_PROJECT_CONFIG")
        .env_remove("OC_GEAR_THROTTLE")
        .env_remove("OC_GEAR_HOME")
        .env_remove("OC_GEAR_TRACE")
        .env_remove("OC_GEAR_OPENCODE_BIN");
    command
}

fn run(cwd: &Path, work: &Path, args: &[&str]) -> Output {
    base_command(cwd, work)
        .args(args)
        .output()
        .expect("run ocg")
}

fn stdout_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stdout_json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout is not JSON: {error}\nstdout={}\nstderr={}",
            stdout_text(output),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

#[test]
fn version_and_help() {
    let dir = TestDir::new();
    let output = run(dir.path(), dir.path(), &["version"]);
    assert!(output.status.success());
    assert!(stdout_text(&output).contains("OpenCode Gear"));

    let output = run(dir.path(), dir.path(), &["help"]);
    assert!(output.status.success());
    assert!(stdout_text(&output).contains("Usage:"));
}

#[test]
fn dry_run_default_is_lead_low() {
    let dir = TestDir::new();
    let output = run(dir.path(), dir.path(), &["--dry-run"]);
    assert!(output.status.success());
    let config = stdout_json(&output);
    assert_eq!(config["default_agent"], json!("lead-low"));
    assert_eq!(config["model"], json!("openai/gpt-5.6-sol"));
}

#[test]
fn throttle_flag_and_positional_level_select_the_lead() {
    let dir = TestDir::new();
    for (args, agent, model) in [
        (
            vec!["--throttle", "high", "--dry-run"],
            "lead-high",
            "openai/gpt-6-astra",
        ),
        (vec!["high", "--dry-run"], "lead-high", "openai/gpt-6-astra"),
        (
            vec!["--throttle=mid", "--dry-run"],
            "lead-mid",
            "openai/gpt-5.6-sol",
        ),
        (vec!["mid", "--dry-run"], "lead-mid", "openai/gpt-5.6-sol"),
    ] {
        let output = run(dir.path(), dir.path(), &args);
        assert!(output.status.success(), "args {args:?}");
        let config = stdout_json(&output);
        assert_eq!(config["default_agent"], json!(agent), "args {args:?}");
        assert_eq!(config["model"], json!(model), "args {args:?}");
    }
}

#[test]
fn throttle_environment_precedence() {
    let dir = TestDir::new();
    let canonical = base_command(dir.path(), dir.path())
        .env("OPENCODE_GEAR_THROTTLE", "high")
        .arg("--dry-run")
        .output()
        .expect("run");
    assert_eq!(stdout_json(&canonical)["default_agent"], json!("lead-high"));

    let legacy = base_command(dir.path(), dir.path())
        .env("OC_GEAR_THROTTLE", "high")
        .arg("--dry-run")
        .output()
        .expect("run");
    assert_eq!(stdout_json(&legacy)["default_agent"], json!("lead-high"));

    let precedence = base_command(dir.path(), dir.path())
        .env("OPENCODE_GEAR_THROTTLE", "high")
        .args(["--throttle", "low", "--dry-run"])
        .output()
        .expect("run");
    assert_eq!(stdout_json(&precedence)["default_agent"], json!("lead-low"));
}

#[test]
fn consumer_routing_is_independent_of_throttle() {
    let dir = TestDir::new();
    let output = run(dir.path(), dir.path(), &["--throttle", "high", "--dry-run"]);
    let config = stdout_json(&output);
    assert_eq!(
        config["agent"]["ocg-explore"]["model"],
        json!("volcengine-coding/kimi-k2.7-code")
    );
    assert_eq!(
        config["agent"]["ocg-build"]["model"],
        json!("opencode-go/deepseek-v4.1-flash")
    );
}

#[test]
fn report_subcommands() {
    let dir = TestDir::new();
    let status = run(dir.path(), dir.path(), &["status"]);
    assert!(status.status.success());
    assert!(stdout_text(&status).contains("Throttle (OpenAI Lead tier)"));

    let routing = run(dir.path(), dir.path(), &["routing"]);
    assert!(routing.status.success());
    assert!(stdout_text(&routing).contains("kimi-k2.7-code"));

    let validate = run(dir.path(), dir.path(), &["validate"]);
    assert!(validate.status.success());
    assert!(stdout_text(&validate).contains("configuration is valid"));

    let layers = run(dir.path(), dir.path(), &["layers"]);
    assert!(layers.status.success());
    assert!(stdout_text(&layers).contains("gear home"));
}

#[test]
fn project_override_changes_routing_without_leaking() {
    let dir = TestDir::new();
    let project = dir.project();
    write_json(
        &project.join(".opencode-gear.json"),
        &json!({
            "throttle": {"default": "mid"},
            "routing": {"roles": {"build": {"model": "glm-5.3", "variant": "high"}}}
        }),
    );
    let project_arg = project.to_string_lossy().into_owned();
    let output = run(
        dir.path(),
        dir.path(),
        &["--project", &project_arg, "--dry-run"],
    );
    let config = stdout_json(&output);
    assert_eq!(config["default_agent"], json!("lead-mid"));
    assert_eq!(
        config["agent"]["ocg-build"]["model"],
        json!("opencode-go/glm-5.3")
    );

    let plain = run(dir.path(), dir.path(), &["--dry-run"]);
    assert_eq!(stdout_json(&plain)["default_agent"], json!("lead-low"));
}

#[test]
fn invalid_override_fails() {
    let dir = TestDir::new();
    let project = dir.project();
    write_json(
        &project.join(".opencode-gear.json"),
        &json!({"routing": {"roles": {"build": {"model": "does-not-exist"}}}}),
    );
    let project_arg = project.to_string_lossy().into_owned();
    let output = run(
        dir.path(),
        dir.path(),
        &["--project", &project_arg, "--dry-run"],
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("does-not-exist"));
}

#[test]
fn usage_failures_exit_non_zero() {
    let dir = TestDir::new();
    for args in [
        vec!["not-a-command"],
        vec!["--nope"],
        vec!["--throttle"],
        vec!["--project", "/definitely/not/a/directory"],
    ] {
        let output = run(dir.path(), dir.path(), &args);
        assert!(
            !output.status.success(),
            "expected failure for args {args:?}"
        );
        assert_eq!(output.status.code(), Some(2), "args {args:?}");
    }
}

#[test]
fn conflicting_positional_and_flag_throttle_fails() {
    let dir = TestDir::new();
    let output = run(
        dir.path(),
        dir.path(),
        &["low", "--throttle", "high", "--dry-run"],
    );
    assert!(!output.status.success());
}

#[test]
fn throttle_persistence_round_trips() {
    let dir = TestDir::new();
    let user = dir.join("user-config.json");
    let user_arg = user.to_string_lossy().into_owned();
    let output = base_command(dir.path(), dir.path())
        .args(["--user-config", &user_arg, "throttle", "high"])
        .output()
        .expect("run");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let config = base_command(dir.path(), dir.path())
        .args(["--user-config", &user_arg, "--dry-run"])
        .output()
        .expect("run");
    assert_eq!(stdout_json(&config)["default_agent"], json!("lead-high"));

    let printed = base_command(dir.path(), dir.path())
        .args(["--user-config", &user_arg, "throttle"])
        .output()
        .expect("run");
    assert_eq!(stdout_text(&printed).trim(), "high");
}

#[test]
fn options_after_the_command_are_parsed() {
    let dir = TestDir::new();
    let pretty = run(dir.path(), dir.path(), &["build", "--pretty"]);
    assert!(pretty.status.success());
    assert!(
        stdout_text(&pretty).contains("\n  \"$schema\""),
        "build --pretty should be pretty-printed"
    );

    let high = run(dir.path(), dir.path(), &["build", "--throttle", "high"]);
    assert_eq!(stdout_json(&high)["default_agent"], json!("lead-high"));
}

#[cfg(unix)]
#[test]
fn run_execs_the_configured_binary_with_config_and_cwd() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TestDir::new();
    let project = dir.project();
    let record = dir.join("record.txt");
    let config_file = dir.join("config.json");
    let script = dir.join("fake-opencode.sh");
    let body = format!(
        "#!/bin/sh\npwd > \"{record}\"\nprintf '%s\\n' \"$@\" >> \"{record}\"\nprintf '%s' \"$OPENCODE_CONFIG_CONTENT\" > \"{config_file}\"\n",
        record = record.display(),
        config_file = config_file.display()
    );
    fs::write(&script, body).expect("write script");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod");

    let project_arg = project.to_string_lossy().into_owned();
    let output = base_command(dir.path(), dir.path())
        .env("OPENCODE_GEAR_OPENCODE_BIN", &script)
        .args(["--project", project_arg.as_str(), "run", "hello world"])
        .output()
        .expect("run");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let recorded = fs::read_to_string(&record).expect("record");
    let lines: Vec<&str> = recorded.lines().collect();
    assert_eq!(lines[0], project.display().to_string());
    assert_eq!(lines[1], "run");
    assert_eq!(lines[2], "hello world");

    let config: Value =
        serde_json::from_str(&fs::read_to_string(&config_file).expect("config")).expect("parse");
    assert_eq!(config["default_agent"], json!("lead-low"));
}

#[cfg(unix)]
#[test]
fn legacy_opencode_bin_variable_is_honored() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TestDir::new();
    let marker = dir.join("marker.txt");
    let script = dir.join("legacy-opencode.sh");
    let body = format!("#!/bin/sh\nprintf done > \"{}\"\n", marker.display());
    fs::write(&script, body).expect("write script");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod");

    let output = base_command(dir.path(), dir.path())
        .env("OC_GEAR_OPENCODE_BIN", &script)
        .arg("run")
        .output()
        .expect("run");
    assert!(output.status.success());
    assert!(marker.is_file());
}

//! End-to-end CLI tests for the `ocg` binary.

mod common;

use common::{write_json, TestDir};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

#[cfg(unix)]
fn assert_same_existing_directory(observed: &Path, expected: &Path) {
    let observed_resolved = fs::canonicalize(observed).unwrap_or_else(|error| {
        panic!(
            "cannot resolve observed directory {}: {error}",
            observed.display()
        )
    });
    let expected_resolved = fs::canonicalize(expected).unwrap_or_else(|error| {
        panic!(
            "cannot resolve expected directory {}: {error}",
            expected.display()
        )
    });
    assert_eq!(
        observed_resolved,
        expected_resolved,
        "child used the wrong working directory (observed {}, expected {})",
        observed.display(),
        expected.display()
    );
}

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
        .env_remove("OC_GEAR_OPENCODE_BIN")
        .env_remove("OPENCODE_GEAR_OPENCODE")
        .env_remove("OPENCODE_GEAR_OPENCODE_BIN")
        .env_remove("OPENCODE_GEAR_API_BASE")
        .env_remove("OPENCODE_GEAR_CACHE_DIR")
        .env_remove("OPENCODE_GEAR_DISABLE_PROXY")
        .env_remove("GH_TOKEN")
        .env_remove("GITHUB_TOKEN")
        .env_remove("HTTP_PROXY")
        .env_remove("http_proxy")
        .env_remove("HTTPS_PROXY")
        .env_remove("https_proxy")
        .env_remove("ALL_PROXY")
        .env_remove("all_proxy")
        .env_remove("NO_PROXY")
        .env_remove("no_proxy");
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
    let preflight_config_file = dir.join("preflight-config.json");
    let script = dir.join("fake-opencode.sh");
    let body = format!(
        "#!/bin/sh\nif [ \"$1\" = \"models\" ]; then printf '%s' \"$OPENCODE_CONFIG_CONTENT\" > \"{preflight_config_file}\"; printf '%s\\n' openai/gpt-5.6-sol openai/gpt-6-astra; exit 0; fi\npwd > \"{record}\"\nprintf '%s\\n' \"$@\" >> \"{record}\"\nprintf '%s' \"$OPENCODE_CONFIG_CONTENT\" > \"{config_file}\"\n",
        record = record.display(),
        config_file = config_file.display(),
        preflight_config_file = preflight_config_file.display()
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
    assert_same_existing_directory(Path::new(lines[0]), &project);
    assert_eq!(lines[1], "run");
    assert_eq!(lines[2], "hello world");

    let config: Value =
        serde_json::from_str(&fs::read_to_string(&config_file).expect("config")).expect("parse");
    assert_eq!(config["default_agent"], json!("lead-low"));
    assert!(
        config["plugin"]
            .as_array()
            .is_some_and(|plugins| !plugins.is_empty()),
        "the coding launch must retain the generated plugin"
    );
    let preflight: Value = serde_json::from_str(
        &fs::read_to_string(&preflight_config_file).expect("preflight config"),
    )
    .expect("parse preflight config");
    assert!(
        preflight.get("plugin").is_none(),
        "model preflight must not load the not-yet-materialized OCG plugin"
    );
}

#[cfg(unix)]
#[test]
fn launch_exports_the_exact_lead_contract_for_each_throttle() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TestDir::new();
    let _project = dir.project();
    let record = dir.join("lead-contract.json");
    let script = dir.join("fake-opencode-contract.sh");
    let body = format!(
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 1.18.31; exit 0; fi\nif [ \"$1\" = \"models\" ]; then printf '%s\\n' openai/gpt-5.6-sol openai/gpt-6-astra; exit 0; fi\nprintf '%s' \"$OPENCODE_GEAR_LEAD_CONTRACT\" > \"{}\"\n",
        record.display()
    );
    fs::write(&script, body).expect("write script");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod");

    for (level, agent, model, variant) in [
        ("low", "lead-low", "gpt-5.6-sol", "low"),
        ("mid", "lead-mid", "gpt-5.6-sol", "medium"),
        ("high", "lead-high", "gpt-6-astra", "low"),
    ] {
        let output = base_command(dir.path(), dir.path())
            .env("OPENCODE_GEAR_OPENCODE_BIN", &script)
            .args(["--throttle", level, "run", "hello"])
            .output()
            .expect("run");
        assert!(
            output.status.success(),
            "{level}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let contract: Value =
            serde_json::from_str(&fs::read_to_string(&record).expect("contract")).expect("json");
        assert_eq!(contract["agent"], json!(agent));
        assert_eq!(contract["provider_id"], json!("openai"));
        assert_eq!(contract["model_id"], json!(model));
        assert_eq!(contract["variant"], json!(variant));
    }
}

/// The `--dry-run` config and the config/contract the bridge actually receives
/// must agree for every throttle level. This is the contract test that closes
/// the "config changes but the runtime does not" gap.
#[cfg(unix)]
#[test]
fn dry_run_effective_config_matches_the_runtime_bridge_contract() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TestDir::new();
    let project = dir.project();
    let project_arg = project.to_string_lossy().into_owned();

    for (level, _agent, _model, _variant) in [
        ("low", "lead-low", "gpt-5.6-sol", "low"),
        ("mid", "lead-mid", "gpt-5.6-sol", "medium"),
        ("high", "lead-high", "gpt-6-astra", "low"),
    ] {
        let record = dir.join(&format!("runtime-{level}"));
        let config_file = record.join("config.json");
        let contract_file = record.join("contract.json");
        fs::create_dir_all(&record).unwrap();
        let script = dir.join(&format!("fake-opencode-{level}.sh"));
        let body = format!(
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 1.18.31; exit 0; fi\nif [ \"$1\" = \"models\" ]; then printf '%s\\n' openai/gpt-5.6-sol openai/gpt-6-astra; exit 0; fi\nprintf '%s' \"$OPENCODE_CONFIG_CONTENT\" > \"{}\"\nprintf '%s' \"$OPENCODE_GEAR_LEAD_CONTRACT\" > \"{}\"\n",
            config_file.display(),
            contract_file.display()
        );
        fs::write(&script, body).unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

        let launch = base_command(dir.path(), dir.path())
            .env("OPENCODE_GEAR_OPENCODE_BIN", &script)
            .args([
                "--project",
                &project_arg,
                "--throttle",
                level,
                "run",
                "hello",
            ])
            .output()
            .expect("run");
        assert!(
            launch.status.success(),
            "{level}: {}",
            String::from_utf8_lossy(&launch.stderr)
        );

        let dry = base_command(dir.path(), dir.path())
            .args(["--project", &project_arg, "--throttle", level, "--dry-run"])
            .output()
            .expect("dry-run");
        assert!(dry.status.success(), "{level}: dry-run failed");
        let dry_config = stdout_json(&dry);

        let runtime_config: Value =
            serde_json::from_str(&fs::read_to_string(&config_file).expect("runtime config"))
                .expect("runtime config json");
        let contract: Value =
            serde_json::from_str(&fs::read_to_string(&contract_file).expect("contract"))
                .expect("contract json");

        // Top-level selection and the per-level Lead agent must agree.
        assert_eq!(runtime_config["default_agent"], dry_config["default_agent"]);
        assert_eq!(runtime_config["model"], dry_config["model"]);
        let agent = contract["agent"].as_str().expect("contract agent");
        assert_eq!(dry_config["default_agent"], json!(agent));
        assert_eq!(
            dry_config["agent"][agent], runtime_config["agent"][agent],
            "{level}: dry-run and runtime lead agent differ"
        );
        let full = format!(
            "{}/{}",
            contract["provider_id"].as_str().unwrap(),
            contract["model_id"].as_str().unwrap()
        );
        assert_eq!(dry_config["agent"][agent]["model"], json!(full));
        assert_eq!(dry_config["agent"][agent]["variant"], contract["variant"]);
    }
}

/// A profile is resolved from scratch on every invocation. `build` must not
/// persist a level, so `ocg high` after `ocg low` cannot leave a sticky
/// profile behind for a new session.
#[test]
fn building_a_profile_leaves_no_session_state() {
    let dir = TestDir::new();
    let project = dir.project();
    let project_arg = project.to_string_lossy().into_owned();

    let low = run(
        dir.path(),
        dir.path(),
        &["--project", &project_arg, "--throttle", "low", "build"],
    );
    let high = run(
        dir.path(),
        dir.path(),
        &["--project", &project_arg, "--throttle", "high", "build"],
    );
    assert_ne!(
        stdout_json(&low)["default_agent"],
        stdout_json(&high)["default_agent"]
    );
    assert_eq!(stdout_json(&low)["default_agent"], json!("lead-low"));
    assert_eq!(stdout_json(&high)["default_agent"], json!("lead-high"));
    assert!(
        !project.join(".opencode-gear").exists(),
        "resolving a profile must not create session/profile state"
    );
}

#[cfg(unix)]
#[test]
fn missing_active_lead_model_blocks_launch_without_fallback() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TestDir::new();
    let marker = dir.join("must-not-launch");
    let script = dir.join("fake-opencode-missing-model.sh");
    let body = format!(
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 1.18.31; exit 0; fi\nif [ \"$1\" = \"models\" ]; then printf '%s\\n' openai/gpt-5.6-sol; exit 0; fi\nprintf launched > \"{}\"\n",
        marker.display()
    );
    fs::write(&script, body).expect("write script");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod");

    let output = base_command(dir.path(), dir.path())
        .env("OPENCODE_GEAR_OPENCODE_BIN", &script)
        .args(["--throttle", "high", "run", "hello"])
        .output()
        .expect("run");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("openai/gpt-6-astra"), "{stderr}");
    assert!(stderr.contains("required Lead model"), "{stderr}");
    assert!(
        !marker.exists(),
        "OpenCode must not launch with a false fallback"
    );
}

#[cfg(unix)]
#[test]
fn unavailable_model_probe_warns_but_does_not_block_launch() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TestDir::new();
    let marker = dir.join("launched");
    let script = dir.join("fake-opencode-probe-failure.sh");
    let body = format!(
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 1.18.31; exit 0; fi\nif [ \"$1\" = \"models\" ]; then echo probe-diagnostic-must-not-be-echoed >&2; exit 9; fi\nprintf launched > \"{}\"\n",
        marker.display()
    );
    fs::write(&script, body).expect("write script");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod");

    let output = base_command(dir.path(), dir.path())
        .env("OPENCODE_GEAR_OPENCODE_BIN", &script)
        .args(["run", "hello"])
        .output()
        .expect("run");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("runtime model check could not be completed"),
        "{stderr}"
    );
    // The probe's own diagnostics can contain arbitrary third-party text and
    // must not be echoed back.
    assert!(
        !stderr.contains("probe-diagnostic-must-not-be-echoed"),
        "{stderr}"
    );
    assert!(marker.exists());
}

#[cfg(unix)]
#[test]
fn filesystem_aliases_resolve_to_the_same_directory() {
    use std::os::unix::fs::symlink;

    let dir = TestDir::new();
    let project = dir.project();
    let alias = dir.join("project-alias");
    symlink(&project, &alias).expect("create directory alias");

    assert_ne!(
        alias, project,
        "the fixture must use distinct lexical paths"
    );
    assert_same_existing_directory(&alias, &project);
}

#[cfg(unix)]
#[test]
fn legacy_opencode_bin_variable_is_honored() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TestDir::new();
    let marker = dir.join("marker.txt");
    let script = dir.join("legacy-opencode.sh");
    let body = format!(
        "#!/bin/sh\nif [ \"$1\" = \"models\" ]; then printf '%s\\n' openai/gpt-5.6-sol; exit 0; fi\nprintf done > \"{}\"\n",
        marker.display()
    );
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

#[test]
fn disable_proxy_flag_is_accepted_before_and_after_the_command() {
    let dir = TestDir::new();
    for args in [
        vec!["--disable-proxy", "--dry-run"],
        vec!["build", "--disable-proxy"],
        vec!["build", "--disable-proxy", "--pretty"],
    ] {
        let output = run(dir.path(), dir.path(), &args);
        assert!(
            output.status.success(),
            "args {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[cfg(unix)]
fn child_env(dir: &TestDir, extra_env: &[(&str, &str)], args: &[&str]) -> Vec<String> {
    use std::os::unix::fs::PermissionsExt;

    let record = dir.join("child-env.txt");
    let script = dir.join("fake-opencode-env.sh");
    let body = format!(
        "#!/bin/sh\nif [ \"$1\" = \"models\" ]; then printf '%s\\n' openai/gpt-5.6-sol; exit 0; fi\nenv > \"{}\"\n",
        record.display()
    );
    fs::write(&script, body).expect("write script");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod");

    let mut command = base_command(dir.path(), dir.path());
    command.env("OPENCODE_GEAR_OPENCODE_BIN", &script);
    for (name, value) in extra_env {
        command.env(name, value);
    }
    let output = command.args(args).output().expect("run ocg");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::read_to_string(&record)
        .expect("read child env")
        .lines()
        .map(str::to_string)
        .collect()
}

#[cfg(unix)]
fn env_value(lines: &[String], name: &str) -> Option<String> {
    let prefix = format!("{name}=");
    lines
        .iter()
        .find_map(|line| line.strip_prefix(&prefix).map(str::to_string))
}

#[cfg(unix)]
#[test]
fn child_process_receives_the_resolved_proxy_only() {
    let dir = TestDir::new();
    let lines = child_env(
        &dir,
        &[
            ("HTTPS_PROXY", "http://proxy-internal:3128"),
            ("https_proxy", "http://stale:1"),
            ("NO_PROXY", "localhost"),
        ],
        &["run", "hello"],
    );
    // The resolved value wins in both spellings; a stale lower-case variable
    // cannot survive.
    for name in ["HTTPS_PROXY", "https_proxy"] {
        assert_eq!(
            env_value(&lines, name).as_deref(),
            Some("http://proxy-internal:3128"),
            "{name}"
        );
    }
    assert_eq!(env_value(&lines, "HTTP_PROXY"), None);
    assert_eq!(env_value(&lines, "http_proxy"), None);
    for name in ["NO_PROXY", "no_proxy"] {
        assert_eq!(
            env_value(&lines, name).as_deref(),
            Some("localhost"),
            "{name}"
        );
    }
}

#[cfg(unix)]
#[test]
fn model_probe_child_receives_the_resolved_proxy_and_no_stale_spelling() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TestDir::new();
    let record = dir.join("models-env.txt");
    let script = dir.join("fake-opencode-models-env.sh");
    let body = format!(
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 1.18.31; exit 0; fi\nif [ \"$1\" = \"models\" ]; then env > \"{record}\"; printf '%s\\n' openai/gpt-5.6-sol openai/gpt-6-astra; exit 0; fi\nexit 0\n",
        record = record.display()
    );
    fs::write(&script, body).expect("write script");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod");

    let output = base_command(dir.path(), dir.path())
        .env("OPENCODE_GEAR_OPENCODE_BIN", &script)
        .env("HTTPS_PROXY", "http://proxy-internal:3128")
        .env("all_proxy", "socks5://stale:1")
        .args(["run", "hello"])
        .output()
        .expect("run ocg");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lines: Vec<String> = fs::read_to_string(&record)
        .expect("read models child env")
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(
        env_value(&lines, "HTTPS_PROXY").as_deref(),
        Some("http://proxy-internal:3128")
    );
    assert_eq!(
        env_value(&lines, "https_proxy").as_deref(),
        Some("http://proxy-internal:3128")
    );
    // An unsupported SOCKS value the user configured is preserved verbatim for
    // the child under its original spelling rather than silently dropped.
    assert_eq!(
        env_value(&lines, "all_proxy").as_deref(),
        Some("socks5://stale:1")
    );
    assert_eq!(
        env_value(&lines, "ALL_PROXY"),
        None,
        "the unconfigured upper-case spelling must stay absent"
    );
}

#[cfg(unix)]
#[test]
fn disable_proxy_flag_strips_every_proxy_variable_for_the_launch() {
    let dir = TestDir::new();
    let lines = child_env(
        &dir,
        &[
            ("HTTPS_PROXY", "http://proxy-internal:3128"),
            ("http_proxy", "http://proxy-internal:3128"),
        ],
        &["--disable-proxy", "run", "hello"],
    );
    for name in [
        "HTTP_PROXY",
        "http_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
        "NO_PROXY",
        "no_proxy",
    ] {
        assert_eq!(env_value(&lines, name), None, "{name} must be stripped");
    }
}

#[cfg(unix)]
#[test]
fn disable_proxy_environment_strips_every_proxy_variable_for_the_launch() {
    let dir = TestDir::new();
    let lines = child_env(
        &dir,
        &[
            ("OPENCODE_GEAR_DISABLE_PROXY", "true"),
            ("HTTPS_PROXY", "http://proxy-internal:3128"),
        ],
        &["run", "hello"],
    );
    assert_eq!(env_value(&lines, "HTTPS_PROXY"), None);
}

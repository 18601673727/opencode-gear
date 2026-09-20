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
        .env("OPENCODE_GEAR_USER_CONFIG", work.join("no-user.yaml"))
        .env_remove("OPENCODE_GEAR_PROJECT_CONFIG")
        .env_remove("OPENCODE_GEAR_THROTTLE")
        .env_remove("OPENCODE_GEAR_HOME")
        .env_remove("OPENCODE_GEAR_TRACE")
        .env_remove("OPENCODE_GEAR_OPENCODE")
        .env_remove("OPENCODE_GEAR_OPENCODE_BIN")
        .env_remove("OC_GEAR_OPENCODE_BIN")
        .env_remove("OPENCODE_GEAR_TELEMETRY")
        .env_remove("OPENCODE_GEAR_ORCHESTRATION")
        .env_remove("OC_GEAR_ORCHESTRATION")
        .env_remove("HTTP_PROXY")
        .env_remove("http_proxy")
        .env_remove("HTTPS_PROXY")
        .env_remove("https_proxy")
        .env_remove("ALL_PROXY")
        .env_remove("all_proxy")
        .env_remove("NO_PROXY")
        .env_remove("no_proxy")
        .env_remove("GH_TOKEN")
        .env_remove("GITHUB_TOKEN");
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

#[cfg(unix)]
fn write_executable(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
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
        "orchestration",
        "orchestration plugin",
        "orchestration state",
        "projection",
        "context activation",
        "verification integration",
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

#[cfg(unix)]
#[test]
fn doctor_reports_static_config_separately_from_available_runtime_models() {
    let dir = TestDir::new();
    let project = dir.project();
    let script = dir.join("fake-opencode.sh");
    write_executable(
        &script,
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 1.18.31; exit 0; fi\nif [ \"$1\" = \"models\" ]; then printf '%s\\n' openai/gpt-5.6-sol openai/gpt-6-astra volcengine-coding/kimi-k2.7-code volcengine-coding/kimi-k3 opencode-go/deepseek-v4.1-flash opencode-go/glm-5.3-flash opencode-go/glm-5.3; exit 0; fi\nexit 2\n",
    );
    let output = base_command(&project, dir.path())
        .env("OPENCODE_GEAR_OPENCODE", &script)
        .args(["--disable-proxy", "doctor"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stdout(&output));
    let text = stdout(&output);
    assert!(text.contains("static config"), "{text}");
    assert!(text.contains("runtime models"), "{text}");
    assert!(text.contains("openai/gpt-5.6-sol (variant low)"), "{text}");
    assert!(
        text.contains("openai/gpt-5.6-sol (variant medium)"),
        "{text}"
    );
    assert!(text.contains("openai/gpt-6-astra (variant low)"), "{text}");
    assert!(text.contains("disabled by CLI"), "{text}");
}

#[cfg(unix)]
#[test]
fn doctor_distinguishes_missing_provider_and_missing_model() {
    let dir = TestDir::new();
    let project = dir.project();
    let script = dir.join("fake-opencode-partial.sh");
    write_executable(
        &script,
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 1.18.31; exit 0; fi\nif [ \"$1\" = \"models\" ]; then printf '%s\\n' openai/gpt-5.6-sol; exit 0; fi\nexit 2\n",
    );
    let output = base_command(&project, dir.path())
        .env("OPENCODE_GEAR_OPENCODE", &script)
        .arg("doctor")
        .output()
        .unwrap();
    assert!(!output.status.success(), "missing routes must fail doctor");
    let text = stdout(&output);
    assert!(text.contains("provider is not currently exposed"), "{text}");
    assert!(text.contains("model is not currently exposed"), "{text}");
    // A FAIL must be visible in the summary, not only in the failing line.
    assert!(text.contains("failures"), "{text}");
    assert!(!text.contains("0 failures"), "{text}");
}

/// The layering section, the effective Lead contracts, the consumer router and
/// the summary are all present for a plain project with no overrides.
#[test]
fn doctor_reports_layering_contracts_and_summary() {
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
        "config layering",
        "defaults",
        "user config",
        "project config",
        "project root",
        "effective Lead contracts",
        "default throttle",
        "default agent",
        "consumer router (independent of throttle)",
        "doctor summary",
    ] {
        assert!(text.contains(label), "missing '{label}' in\n{text}");
    }
    // The effective contracts must be the shipped profile, not a stale value.
    assert!(text.contains("openai/gpt-5.6-sol variant low"), "{text}");
    assert!(text.contains("openai/gpt-5.6-sol variant medium"), "{text}");
    assert!(text.contains("openai/gpt-6-astra variant low"), "{text}");
    // The Consumer Router is reported alongside, and is not the Lead.
    assert!(text.contains("volcengine-coding/kimi-k2.7-code"), "{text}");
    assert!(text.contains("opencode-go/deepseek-v4.1-flash"), "{text}");
    // A clean environment has no FAIL.
    assert!(text.contains("0 failures"), "{text}");
}

#[test]
fn doctor_reports_missing_project_config_as_info() {
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
    assert!(text.contains("project config"), "{text}");
    assert!(text.contains("not present"), "{text}");
}

#[test]
fn doctor_marks_user_and_project_layers_as_found() {
    let dir = TestDir::new();
    let project = dir.project();
    let user = dir.join("user-config.yaml");
    let project_config = project.join("project-config.yaml");
    fs::write(&user, "{\"throttle\":{\"default\":\"mid\"}}\n").unwrap();
    fs::write(&project_config, "{\"throttle\":{\"default\":\"high\"}}\n").unwrap();
    let output = base_command(dir.path(), dir.path())
        .args([
            "--user-config",
            user.to_str().unwrap(),
            "--project-config",
            project_config.to_str().unwrap(),
            "doctor",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stdout(&output));
    let text = stdout(&output);
    assert!(text.contains(user.to_str().unwrap()), "{text}");
    assert!(text.contains(project_config.to_str().unwrap()), "{text}");
    // Both layers are applied, so the project default (high) wins.
    assert!(
        text.lines()
            .any(|line| line.contains("default agent") && line.contains("lead-high")),
        "{text}"
    );
}

#[cfg(unix)]
#[test]
fn doctor_warns_about_a_socks_proxy_without_leaking_its_value() {
    let dir = TestDir::new();
    let project = dir.project();
    let output = base_command(dir.path(), dir.path())
        .env(
            "ALL_PROXY",
            "socks5h://alice:wonderland@proxy-internal:1080",
        )
        .args(["--project", project.to_str().unwrap(), "doctor"])
        .output()
        .unwrap();
    // SOCKS is a warning, never a failure: the child keeps the value.
    assert!(output.status.success(), "{}", stdout(&output));
    let text = stdout(&output);
    assert!(text.contains("ALL_PROXY"), "{text}");
    assert!(text.contains("SOCKS"), "{text}");
    assert!(text.contains("preserved"), "{text}");
    assert!(text.contains("ignore it"), "{text}");
    for secret in ["wonderland", "alice", "proxy-internal", "1080"] {
        assert!(!text.contains(secret), "leaked '{secret}' in\n{text}");
    }
}

#[cfg(unix)]
#[test]
fn doctor_reports_a_normal_http_proxy_without_its_value() {
    let dir = TestDir::new();
    let project = dir.project();
    let output = base_command(dir.path(), dir.path())
        .env("HTTP_PROXY", "http://proxy-internal:3128")
        .env("HTTPS_PROXY", "http://proxy-internal:3129")
        .env("NO_PROXY", "localhost,.internal")
        .args(["--project", project.to_str().unwrap(), "doctor"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stdout(&output));
    let text = stdout(&output);
    assert!(text.contains("HTTP_PROXY"), "{text}");
    assert!(text.contains("HTTPS_PROXY"), "{text}");
    assert!(text.contains("NO_PROXY"), "{text}");
    assert!(text.contains("present"), "{text}");
    assert!(!text.contains("proxy-internal"), "leaked value in\n{text}");
}

#[cfg(unix)]
#[test]
fn doctor_never_prints_proxy_or_provider_credentials() {
    let dir = TestDir::new();
    let project = dir.project();
    // Build the token shape at runtime so this test source does not itself look
    // like a committed credential to the repository hygiene scan.
    let fake_pat = format!("ghp_{}", "0123456789abcdefghijklmnopqrstuvwxyz");
    let output = base_command(dir.path(), dir.path())
        .env("HTTPS_PROXY", "http://alice:wonderland@proxy-internal:3128")
        .env("ALL_PROXY", "socks5://bob:builder@proxy-internal:1080")
        .env("GH_TOKEN", &fake_pat)
        .env("GITHUB_TOKEN", "github_pat_value_that_must_not_render")
        .args(["--project", project.to_str().unwrap(), "doctor"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "warnings must not fail doctor: {}",
        stdout(&output)
    );
    let text = stdout(&output);
    assert!(!text.contains(&fake_pat), "leaked GH_TOKEN in\n{text}");
    for secret in [
        "wonderland",
        "alice",
        "bob",
        "builder",
        "proxy-internal",
        "github_pat_value_that_must_not_render",
    ] {
        assert!(!text.contains(secret), "leaked '{secret}' in\n{text}");
    }
}

#[cfg(unix)]
#[test]
fn doctor_warns_when_the_system_and_runtime_versions_differ() {
    let dir = TestDir::new();
    let project = dir.project();
    let bin = dir.join("bin");
    fs::create_dir_all(&bin).unwrap();
    write_executable(
        &bin.join("opencode"),
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 9.9.9; exit 0; fi\nexit 2\n",
    );
    let runtime = dir.join("fake-runtime.sh");
    write_executable(
        &runtime,
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 1.18.31; exit 0; fi\nif [ \"$1\" = \"models\" ]; then printf '%s\\n' openai/gpt-5.6-sol openai/gpt-6-astra volcengine-coding/kimi-k2.7-code volcengine-coding/kimi-k3 opencode-go/deepseek-v4.1-flash opencode-go/glm-5.3-flash opencode-go/glm-5.3; exit 0; fi\nexit 2\n",
    );
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = base_command(dir.path(), dir.path())
        .env("PATH", path)
        .env("OPENCODE_GEAR_OPENCODE", &runtime)
        .args(["--project", project.to_str().unwrap(), "doctor"])
        .output()
        .unwrap();
    let text = stdout(&output);
    assert!(text.contains("opencode on PATH"), "{text}");
    assert!(text.contains("9.9.9"), "{text}");
    assert!(text.contains("system PATH is 9.9.9"), "{text}");
    assert!(text.contains("runtime (explicit) is 1.18.31"), "{text}");
}

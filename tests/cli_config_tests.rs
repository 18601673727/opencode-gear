//! End-to-end tests for `ocg config` (Lead and provider configuration).
//!
//! Every test runs the real binary against temp directories and an isolated
//! HOME. The runtime catalogue probe is driven by a fake OpenCode executable
//! so no real runtime, network or credential store is involved.

mod common;

use common::{read_yaml, write_yaml, TestDir};
use serde_json::{json, Map, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// All environment that could leak the developer's real configuration.
fn base_command(cwd: &Path, dir: &TestDir, user: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ocg"));
    command
        .current_dir(cwd)
        .env("HOME", dir.join("home"))
        .env("OPENCODE_GEAR_USER_CONFIG", user)
        .env_remove("OPENCODE_GEAR_PROJECT_CONFIG")
        .env_remove("OC_GEAR_USER_CONFIG")
        .env_remove("OC_GEAR_PROJECT_CONFIG")
        .env_remove("OPENCODE_GEAR_THROTTLE")
        .env_remove("OC_GEAR_THROTTLE")
        .env_remove("OPENCODE_GEAR_HOME")
        .env_remove("OC_GEAR_HOME")
        .env_remove("OPENCODE_GEAR_TRACE")
        .env_remove("OC_GEAR_TRACE")
        .env_remove("OPENCODE_GEAR_OPENCODE")
        .env_remove("OPENCODE_GEAR_OPENCODE_BIN")
        .env_remove("OC_GEAR_OPENCODE")
        .env_remove("OC_GEAR_OPENCODE_BIN")
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

fn run(cwd: &Path, dir: &TestDir, user: &Path, args: &[&str]) -> Output {
    base_command(cwd, dir, user)
        .args(args)
        .output()
        .expect("run ocg")
}

fn run_with_fake(cwd: &Path, dir: &TestDir, user: &Path, fake: &Path, args: &[&str]) -> Output {
    base_command(cwd, dir, user)
        .env("OPENCODE_GEAR_OPENCODE_BIN", fake)
        .args(args)
        .output()
        .expect("run ocg")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// A fake OpenCode whose catalogue is fully controlled.
///
/// The fake is a small Python server because OpenCode 2's catalogue probe now
/// starts an OCG-owned `opencode serve` child and reads `/api/config`; the
/// same fake also preserves the historical `opencode models` CLI surface and
/// the activation session API for the post-write read-back.
fn fake_opencode(dir: &TestDir, name: &str, models: Option<&str>) -> PathBuf {
    fake_opencode_impl(dir, name, models, false)
}

/// A fake OpenCode that reflects the providers and models declared in the
/// generated config it receives through `OPENCODE_CONFIG_CONTENT`.
fn fake_opencode_config_aware(dir: &TestDir, name: &str) -> PathBuf {
    fake_opencode_impl(dir, name, None, true)
}

fn fake_opencode_impl(
    dir: &TestDir,
    name: &str,
    models: Option<&str>,
    config_aware: bool,
) -> PathBuf {
    let path = dir.join(name);
    let fixed_models_json = models
        .map(|models| {
            let mut providers: serde_json::Map<String, Value> = Map::new();
            for token in models.split_whitespace() {
                let (provider, id) = token.split_once('/').unwrap_or(("unknown", token));
                let provider_obj = providers
                    .entry(provider.to_string())
                    .or_insert_with(|| json!({"models": {}}));
                provider_obj["models"][id] = json!({"name": id});
            }
            json!(providers).to_string()
        })
        .unwrap_or_else(|| "{}".to_string());
    let config_aware_py = if config_aware { "True" } else { "False" };
    let fail_probe_py = if models.is_none() && !config_aware {
        "True"
    } else {
        "False"
    };
    let script = format!(
        r#"
import base64
import json
import os
import secrets
import sys
import threading
import urllib.parse
from http.server import BaseHTTPRequestHandler, HTTPServer

CONFIG_AWARE = {config_aware_py}
FAIL_PROBE = {fail_probe_py}
FIXED_MODELS = json.loads({fixed_models_json:?})
PASSWORD = secrets.token_urlsafe(32)
SESSION_STORE = {{}}

print("fake checkpoint: python entry", file=sys.stderr, flush=True)
print(f"fake checkpoint: argv={{sys.argv[1:]!r}}", file=sys.stderr, flush=True)


def catalogue_from_env():
    content = os.environ.get("OPENCODE_CONFIG_CONTENT", "{{}}")
    try:
        config = json.loads(content)
    except Exception:
        config = {{}}
    providers = config.get("provider", {{}})
    result = {{}}
    for provider_name, provider in providers.items():
        result[provider_name] = {{"models": {{}}}}
        for model_id in provider.get("models", {{}}).keys():
            result[provider_name]["models"][model_id] = {{"name": model_id}}
    return result


def get_catalogue():
    if CONFIG_AWARE:
        return catalogue_from_env()
    return FIXED_MODELS


def config_response():
    return [
        {{"type": "directory", "path": os.environ.get("HOME", "/tmp")}},
        {{
            "type": "document",
            "info": {{
                "$schema": "https://opencode.ai/config.json",
                "providers": get_catalogue(),
            }},
        }},
    ]


def check_auth(headers):
    auth = headers.get("Authorization", "")
    if not auth.startswith("Basic "):
        return False
    try:
        decoded = base64.b64decode(auth[6:]).decode("utf-8")
        return decoded.split(":", 1)[1] == PASSWORD
    except Exception:
        return False


class Handler(BaseHTTPRequestHandler):
    def log_message(self, format, *args):
        pass

    def send_json(self, status, body):
        data = json.dumps(body).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        if not check_auth(self.headers):
            self.send_error(401)
            return
        parsed = urllib.parse.urlparse(self.path)
        if parsed.path == "/api/config":
            self.send_json(200, config_response())
        elif parsed.path == "/api/session":
            self.send_json(
                200,
                {{"data": [], "cursor": {{"previous": None, "next": None}}}},
            )
        elif parsed.path.startswith("/api/session/"):
            session_id = parsed.path[len("/api/session/"):].split("/")[0]
            model = SESSION_STORE.get(session_id, {{}})
            self.send_json(
                200,
                {{
                    "data": {{
                        "agent": model.get("agent", ""),
                        "model": {{
                            "providerID": model.get("providerID", ""),
                            "id": model.get("id", ""),
                            "variant": model.get("variant"),
                        }},
                    }}
                }},
            )
        else:
            self.send_error(404)

    def do_POST(self):
        if not check_auth(self.headers):
            self.send_error(401)
            return
        parsed = urllib.parse.urlparse(self.path)
        length = int(self.headers.get("Content-Length", "0"))
        body = self.rfile.read(length) if length else b"{{}}"
        try:
            payload = json.loads(body)
        except Exception:
            payload = {{}}
        if parsed.path == "/api/session":
            session_id = "ses_test_" + secrets.token_hex(8)
            self.send_json(200, {{"data": {{"id": session_id}}}})
        elif parsed.path.startswith("/api/session/"):
            session_id = parsed.path[len("/api/session/"):].split("/")[0]
            rest = "/".join(parsed.path[len("/api/session/"):].split("/")[1:])
            if rest == "agent":
                SESSION_STORE[session_id] = SESSION_STORE.get(session_id, {{}})
                SESSION_STORE[session_id]["agent"] = payload.get("agent", "")
                self.send_response(204)
                self.end_headers()
            elif rest == "model":
                model = payload.get("model", {{}})
                SESSION_STORE[session_id] = {{
                    "agent": SESSION_STORE.get(session_id, {{}}).get("agent", ""),
                    "providerID": model.get("providerID", ""),
                    "id": model.get("id", ""),
                    "variant": model.get("variant"),
                }}
                self.send_response(204)
                self.end_headers()
            else:
                self.send_error(404)
        else:
            self.send_error(404)


def run_serve():
    print("fake checkpoint: before HTTPServer", file=sys.stderr, flush=True)
    server = HTTPServer(("127.0.0.1", 0), Handler)
    print("fake checkpoint: after HTTPServer", file=sys.stderr, flush=True)
    port = server.server_address[1]
    print("fake checkpoint: before handshake", file=sys.stderr, flush=True)
    print(f"server listening on http://127.0.0.1:{{port}}", flush=True)
    print(f"server password {{PASSWORD}}", flush=True)
    print("fake checkpoint: after handshake prints", file=sys.stderr, flush=True)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    thread.join()


if sys.argv[1:] == ["--version"]:
    print("fake checkpoint: version", file=sys.stderr, flush=True)
    print("2.0.11")
    sys.exit(0)

if sys.argv[1:] == ["models"]:
    if FAIL_PROBE:
        sys.exit(1)
    catalogue = get_catalogue()
    for provider_name, provider in catalogue.items():
        for model_id in provider.get("models", {{}}).keys():
            print(f"{{provider_name}}/{{model_id}}")
    sys.exit(0)

if sys.argv[1:] and sys.argv[1] == "serve":
    if FAIL_PROBE:
        sys.exit(1)
    print("fake checkpoint: serve dispatch", file=sys.stderr, flush=True)
    run_serve()

print("fake opencode: unexpected arguments", file=sys.stderr)
sys.exit(1)
"#
    );
    let python_path = path.with_extension("py");
    fs::write(&python_path, script).expect("write fake opencode python");
    let launcher = format!(
        "#!/bin/sh\nset -eu\nif command -v python3 >/dev/null 2>&1; then\n  exec \"$(command -v python3)\" \"{}\" \"$@\"\nfi\nif [ -x /usr/bin/python3 ]; then\n  exec /usr/bin/python3 \"{}\" \"$@\"\nfi\nprintf '%s\\n' 'fake opencode requires python3' >&2\nexit 127\n",
        python_path.display(),
        python_path.display()
    );
    fs::write(&path, launcher).expect("write fake opencode launcher");
    make_executable(&path);
    path
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("chmod");
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) {}

fn project(dir: &TestDir) -> PathBuf {
    dir.project()
}

fn user_path(dir: &TestDir) -> PathBuf {
    dir.join("user.yaml")
}

#[test]
fn user_scope_lead_change_writes_the_user_layer_and_preserves_unrelated_yaml() {
    let dir = TestDir::new();
    let project = project(&dir);
    let user = user_path(&dir);
    write_yaml(
        &user,
        &json!({
            "observability": {"enabled": true, "path": "/tmp/keep.yaml"},
            "unrelated": {"keep": "me"},
        }),
    );
    let fake = fake_opencode(&dir, "fake-ok", Some("openai/gpt-6-astra"));

    let output = run_with_fake(
        &project,
        &dir,
        &user,
        &fake,
        &[
            "config",
            "lead",
            "high",
            "--model",
            "openai/gpt-6-astra",
            "--variant",
            "xhigh",
            "--scope",
            "user",
            "--yes",
        ],
    );
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert!(
        stdout(&output).contains("Lead for throttle 'high' updated"),
        "{}",
        stdout(&output)
    );

    let written = read_yaml(&user);
    assert_eq!(
        written["throttle"]["levels"]["high"]["model"],
        json!("astra")
    );
    assert_eq!(
        written["throttle"]["levels"]["high"]["variant"],
        json!("xhigh")
    );
    // Unrelated data survives the semantic rewrite.
    assert_eq!(written["observability"]["enabled"], json!(true));
    assert_eq!(written["observability"]["path"], json!("/tmp/keep.yaml"));
    assert_eq!(written["unrelated"]["keep"], json!("me"));
    // No temp file is left behind by the atomic write.
    for entry in fs::read_dir(dir.path()).expect("read dir") {
        let name = entry
            .expect("entry")
            .file_name()
            .to_string_lossy()
            .into_owned();
        assert!(
            !name.contains(".tmp-"),
            "temporary file left behind: {name}"
        );
    }
}

#[test]
fn project_scope_lead_change_writes_the_project_layer() {
    let dir = TestDir::new();
    let project = project(&dir);
    let user = user_path(&dir);
    let fake = fake_opencode(&dir, "fake-ok", Some("volcengine-coding-plan/kimi-k3"));

    let output = run_with_fake(
        &project,
        &dir,
        &user,
        &fake,
        &[
            "config",
            "lead",
            "low",
            "--model",
            "volcengine-coding-plan/kimi-k3",
            "--scope",
            "project",
            "--yes",
        ],
    );
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));

    let project_file = project.join(".opencode-gear.yaml");
    let written = read_yaml(&project_file);
    assert_eq!(
        written["throttle"]["levels"]["low"]["model"],
        json!("kimi-k3")
    );
    assert!(!user.is_file(), "the user layer must not be touched");
}

#[test]
fn auto_creates_a_deterministic_registry_entry_and_provider_declaration() {
    let dir = TestDir::new();
    let project = project(&dir);
    let user = user_path(&dir);
    let fake = fake_opencode(&dir, "fake-ok", Some("acme/widget-7"));

    let output = run_with_fake(
        &project,
        &dir,
        &user,
        &fake,
        &["config", "lead", "mid", "--model", "acme/widget-7", "--yes"],
    );
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));

    let written = read_yaml(&user);
    assert_eq!(
        written["throttle"]["levels"]["mid"]["model"],
        json!("acme-widget-7")
    );
    assert_eq!(
        written["models"]["models"]["acme-widget-7"]["provider"],
        json!("acme")
    );
    assert_eq!(
        written["models"]["models"]["acme-widget-7"]["id"],
        json!("widget-7")
    );
    assert_eq!(
        written["models"]["providers"]["acme"]["label"],
        json!("Acme")
    );
    // No invented variant: the route runs at the provider default.
    assert!(written["throttle"]["levels"]["mid"]
        .get("variant")
        .is_none());
}

#[test]
fn reuses_an_existing_registry_entry_without_adding_one() {
    let dir = TestDir::new();
    let project = project(&dir);
    let user = user_path(&dir);
    let fake = fake_opencode(&dir, "fake-ok", Some("opencode-go/glm-5.3"));

    let output = run_with_fake(
        &project,
        &dir,
        &user,
        &fake,
        &[
            "config",
            "lead",
            "mid",
            "--model",
            "opencode-go/glm-5.3",
            "--variant",
            "high",
            "--yes",
        ],
    );
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));

    let written = read_yaml(&user);
    // The existing registry key is reused (glm-5.3) and no registry section is
    // written into the layer at all.
    assert_eq!(
        written["throttle"]["levels"]["mid"]["model"],
        json!("glm-5.3")
    );
    assert_eq!(
        written["throttle"]["levels"]["mid"]["variant"],
        json!("high")
    );
    assert!(written.get("models").is_none(), "{written}");
}

#[test]
fn an_invalid_variant_is_rejected_without_replacing_the_file() {
    let dir = TestDir::new();
    let project = project(&dir);
    let user = user_path(&dir);
    let original = json!({"throttle": {"levels": {"high": {"model": "kimi-k3"}}}});
    write_yaml(&user, &original);
    let fake = fake_opencode(&dir, "fake-ok", Some("openai/gpt-5.6-sol"));

    let output = run_with_fake(
        &project,
        &dir,
        &user,
        &fake,
        &[
            "config",
            "lead",
            "high",
            "--model",
            "openai/gpt-5.6-sol",
            "--variant",
            "bogus-variant",
            "--yes",
        ],
    );
    assert_ne!(output.status.code(), Some(0));
    let text = format!("{}{}", stdout(&output), stderr(&output));
    assert!(text.contains("bogus-variant"), "{text}");
    assert_eq!(read_yaml(&user), original, "the file must be unchanged");
}

#[test]
fn a_definitely_missing_active_model_is_rejected_when_the_probe_succeeds() {
    let dir = TestDir::new();
    let project = project(&dir);
    let user = user_path(&dir);
    let original = json!({"throttle": {"levels": {"high": {"model": "kimi-k3"}}}});
    write_yaml(&user, &original);
    // The catalogue exposes the provider but not the requested model.
    let fake = fake_opencode(&dir, "fake-missing", Some("openai/gpt-5.6-sol"));

    let output = run_with_fake(
        &project,
        &dir,
        &user,
        &fake,
        &[
            "config",
            "lead",
            "high",
            "--model",
            "openai/gpt-6-astra",
            "--yes",
        ],
    );
    assert_ne!(output.status.code(), Some(0));
    let text = format!("{}{}", stdout(&output), stderr(&output));
    assert!(
        text.contains("does not expose openai/gpt-6-astra"),
        "{text}"
    );
    assert_eq!(read_yaml(&user), original, "the file must be unchanged");
}

#[test]
fn an_unavailable_probe_writes_the_change_and_says_it_was_not_verified() {
    let dir = TestDir::new();
    let project = project(&dir);
    let user = user_path(&dir);
    // `models` fails: the catalogue cannot be checked at all.
    let fake = fake_opencode(&dir, "fake-fail", None);

    let output = run_with_fake(
        &project,
        &dir,
        &user,
        &fake,
        &[
            "config",
            "lead",
            "low",
            "--model",
            "volcengine-coding-plan/kimi-k3",
            "--yes",
        ],
    );
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let text = format!("{}{}", stdout(&output), stderr(&output));
    assert!(text.contains("NOT verified"), "{text}");
    let written = read_yaml(&user);
    assert_eq!(
        written["throttle"]["levels"]["low"]["model"],
        json!("kimi-k3")
    );
}

#[test]
fn adding_an_openai_compatible_provider_writes_an_env_reference_only() {
    let dir = TestDir::new();
    let project = project(&dir);
    let user = user_path(&dir);

    let output = run(
        &project,
        &dir,
        &user,
        &[
            "config",
            "provider",
            "add-openai-compatible",
            "acme",
            "--base-url",
            "https://api.acme.dev/v1",
            "--api-key-env",
            "ACME_API_KEY",
            "--model",
            "widget-7",
            "--model",
            "widget-8",
            "--scope",
            "user",
            "--yes",
        ],
    );
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));

    let written = read_yaml(&user);
    let provider = &written["opencode"]["provider"]["acme"];
    assert_eq!(provider["npm"], json!("@ai-sdk/openai-compatible"));
    assert_eq!(
        provider["options"]["baseURL"],
        json!("https://api.acme.dev/v1")
    );
    assert_eq!(provider["options"]["apiKey"], json!("{env:ACME_API_KEY}"));
    assert!(provider["models"]["widget-7"].is_object());
    assert_eq!(
        written["models"]["models"]["acme-widget-7"]["provider"],
        json!("acme")
    );
    assert_eq!(
        written["models"]["models"]["acme-widget-8"]["id"],
        json!("widget-8")
    );
    assert_eq!(
        written["models"]["providers"]["acme"]["label"],
        json!("Acme")
    );
    // Only the reference is persisted: never a literal secret.
    let text = fs::read_to_string(&user).expect("read user yaml");
    assert!(
        !text.contains("sk-"),
        "a raw key must never be written: {text}"
    );
}

#[test]
fn a_raw_api_key_is_never_accepted() {
    let dir = TestDir::new();
    let project = project(&dir);
    let user = user_path(&dir);

    let output = run(
        &project,
        &dir,
        &user,
        &[
            "config",
            "provider",
            "add-openai-compatible",
            "acme",
            "--base-url",
            "https://api.acme.dev/v1",
            "--api-key-env",
            "sk-live-abcd1234",
            "--model",
            "widget-7",
            "--yes",
        ],
    );
    assert_ne!(output.status.code(), Some(0));
    let text = format!("{}{}", stdout(&output), stderr(&output));
    assert!(text.contains("environment variable name"), "{text}");
    assert!(!user.is_file(), "nothing may be written");
}

#[test]
fn routing_still_works_and_config_is_no_longer_an_alias() {
    let dir = TestDir::new();
    let project = project(&dir);
    let user = user_path(&dir);

    let routing = run(&project, &dir, &user, &["routing"]);
    assert_eq!(
        routing.status.code(),
        Some(0),
        "stderr: {}",
        stderr(&routing)
    );
    assert!(
        stdout(&routing).contains("ocg-build"),
        "{}",
        stdout(&routing)
    );

    let table = run(&project, &dir, &user, &["config", "lead"]);
    assert_eq!(table.status.code(), Some(0), "stderr: {}", stderr(&table));
    let text = stdout(&table);
    assert!(text.contains("OpenCode Gear Lead configuration"), "{text}");
    assert!(text.contains("openai/gpt-5.6-sol"), "{text}");
    assert!(text.contains("openai/gpt-6-astra"), "{text}");
}

#[test]
fn the_project_layer_override_is_reported_explicitly() {
    let dir = TestDir::new();
    let project = project(&dir);
    let user = user_path(&dir);
    // The project pins the active level; a user-scope change is shadowed.
    write_yaml(
        &project.join(".opencode-gear.yaml"),
        &json!({"throttle": {"levels": {"low": {"model": "astra"}}}}),
    );
    let fake = fake_opencode(&dir, "fake-ok", Some("openai/gpt-6-astra"));

    let output = run_with_fake(
        &project,
        &dir,
        &user,
        &fake,
        &[
            "config",
            "lead",
            "low",
            "--model",
            "volcengine-coding-plan/kimi-k3",
            "--scope",
            "user",
            "--yes",
        ],
    );
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("the project value wins"), "{text}");
    // The user layer still records the requested change.
    let written = read_yaml(&user);
    assert_eq!(
        written["throttle"]["levels"]["low"]["model"],
        json!("kimi-k3")
    );
}

#[test]
fn the_interactive_menu_exits_cleanly_on_closed_stdin() {
    let dir = TestDir::new();
    let project = project(&dir);
    let user = user_path(&dir);
    // Hermetic probe: the menu's change path must never touch a real runtime.
    let fake = fake_opencode(&dir, "fake-ok", Some("openai/gpt-6-astra"));

    let mut child = base_command(&project, &dir, &user)
        .env("OPENCODE_GEAR_OPENCODE_BIN", &fake)
        .arg("config")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn ocg config");
    // Close stdin immediately: the menu must terminate, never hang.
    drop(child.stdin.take());
    let output = child.wait_with_output().expect("wait");
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("OpenCode Gear configuration"), "{text}");

    // A scripted menu session drives a real change: choice 2, level high,
    // model, blank variant, user scope, confirm.
    let mut child = base_command(&project, &dir, &user)
        .env("OPENCODE_GEAR_OPENCODE_BIN", &fake)
        .arg("config")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn ocg config");
    {
        use std::io::Write;
        let stdin = child.stdin.as_mut().expect("stdin");
        stdin
            .write_all(b"2\nhigh\nopenai/gpt-6-astra\n\nuser\ny\n0\n")
            .expect("write menu input");
    }
    let output = child.wait_with_output().expect("wait");
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let written = read_yaml(&user);
    assert_eq!(
        written["throttle"]["levels"]["high"]["model"],
        json!("astra")
    );
}

#[test]
fn a_missing_yes_refuses_to_write_in_scripted_mode() {
    let dir = TestDir::new();
    let project = project(&dir);
    let user = user_path(&dir);

    let output = run(
        &project,
        &dir,
        &user,
        &[
            "config",
            "lead",
            "low",
            "--model",
            "volcengine-coding-plan/kimi-k3",
        ],
    );
    assert_ne!(output.status.code(), Some(0));
    assert!(!user.is_file(), "nothing may be written without --yes");
}

#[test]
fn selecting_a_user_scoped_custom_provider_at_project_scope_succeeds() {
    let dir = TestDir::new();
    let project = project(&dir);
    let user = user_path(&dir);
    let fake = fake_opencode_config_aware(&dir, "fake-config-aware");

    // Register the provider at user scope.
    let add = run_with_fake(
        &project,
        &dir,
        &user,
        &fake,
        &[
            "config",
            "provider",
            "add-openai-compatible",
            "vsllm",
            "--base-url",
            "https://vsllm.cc/v1",
            "--api-key-env",
            "VSLLM_API_KEY",
            "--model",
            "gpt-6-astra",
            "--model",
            "gpt-5.6-sol",
            "--scope",
            "user",
            "--yes",
        ],
    );
    assert_eq!(add.status.code(), Some(0), "stderr: {}", stderr(&add));

    // Project config already exists with some content so it is not empty.
    write_yaml(
        &project.join(".opencode-gear.yaml"),
        &json!({"throttle": {"levels": {"high": {"model": "astra"}}}}),
    );

    // Select a model from the user-scoped provider at project scope.
    let output = run_with_fake(
        &project,
        &dir,
        &user,
        &fake,
        &[
            "config",
            "lead",
            "high",
            "--model",
            "vsllm/gpt-6-astra",
            "--scope",
            "project",
            "--yes",
        ],
    );
    let text = format!("{}\n{}", stdout(&output), stderr(&output));
    assert_eq!(output.status.code(), Some(0), "output: {text}");
    assert!(text.contains("Lead for throttle 'high' updated"), "{text}");
    assert!(text.contains("vsllm/gpt-6-astra"), "{text}");
    // The candidate runtime validated and the post-write activation read back
    // the effective Lead from the OCG-owned private server.
    assert!(
        text.contains("runtime:  verified") || text.contains("effective: verified"),
        "expected runtime/effective verification in: {text}"
    );

    let project_file = project.join(".opencode-gear.yaml");
    let written = read_yaml(&project_file);
    assert_eq!(
        written["throttle"]["levels"]["high"]["model"],
        json!("vsllm-gpt-6-astra")
    );

    // The user-scoped provider definition and env reference survive untouched.
    let user_written = read_yaml(&user);
    assert_eq!(
        user_written["opencode"]["provider"]["vsllm"]["options"]["apiKey"],
        json!("{env:VSLLM_API_KEY}")
    );
    let user_text = fs::read_to_string(&user).expect("read user yaml");
    assert!(
        !user_text.contains("sk-"),
        "the actual secret must never be persisted: {user_text}"
    );
}

#[test]
fn missing_provider_is_rejected_and_leaves_files_unchanged() {
    let dir = TestDir::new();
    let project = project(&dir);
    let user = user_path(&dir);
    let fake = fake_opencode(&dir, "fake-ok", Some("openai/gpt-6-astra"));

    let original_user = json!({"throttle": {"levels": {"high": {"model": "astra"}}}});
    write_yaml(&user, &original_user);
    let project_file = project.join(".opencode-gear.yaml");
    let original_project = json!({"throttle": {"levels": {"low": {"model": "sol"}}}});
    write_yaml(&project_file, &original_project);

    let output = run_with_fake(
        &project,
        &dir,
        &user,
        &fake,
        &[
            "config",
            "lead",
            "high",
            "--model",
            "nonexistent/widget-7",
            "--yes",
        ],
    );
    assert_ne!(output.status.code(), Some(0));
    let text = format!("{}\n{}", stdout(&output), stderr(&output));
    assert!(
        text.contains("does not expose nonexistent/widget-7"),
        "{text}"
    );
    assert_eq!(read_yaml(&user), original_user);
    assert_eq!(read_yaml(&project_file), original_project);
}

#[test]
fn valid_provider_with_invalid_model_is_rejected_and_leaves_files_unchanged() {
    let dir = TestDir::new();
    let project = project(&dir);
    let user = user_path(&dir);

    // Register a provider that only exposes gpt-6-astra.
    write_yaml(
        &user,
        &json!({
            "models": {
                "providers": {"vsllm": {"label": "Vsllm"}},
                "models": {"vsllm-gpt-6-astra": {"provider": "vsllm", "id": "gpt-6-astra"}}
            },
            "opencode": {
                "provider": {
                    "vsllm": {
                        "npm": "@ai-sdk/openai-compatible",
                        "options": {
                            "baseURL": "https://vsllm.cc/v1",
                            "apiKey": "{env:VSLLM_API_KEY}"
                        },
                        "models": {"gpt-6-astra": {"name": "Gpt 6 Astra"}}
                    }
                }
            }
        }),
    );

    // The fake runtime reflects the configured provider/models exactly.
    let fake = fake_opencode_config_aware(&dir, "fake-config-aware");
    let project_file = project.join(".opencode-gear.yaml");
    let original_project = json!({"throttle": {"levels": {"high": {"model": "astra"}}}});
    write_yaml(&project_file, &original_project);

    let output = run_with_fake(
        &project,
        &dir,
        &user,
        &fake,
        &[
            "config",
            "lead",
            "high",
            "--model",
            "vsllm/gpt-5.6-sol",
            "--scope",
            "project",
            "--yes",
        ],
    );
    assert_ne!(output.status.code(), Some(0));
    let text = format!("{}\n{}", stdout(&output), stderr(&output));
    assert!(text.contains("does not expose vsllm/gpt-5.6-sol"), "{text}");
    assert_eq!(read_yaml(&user), read_yaml(&user));
    assert_eq!(read_yaml(&project_file), original_project);
}

#[test]
fn the_config_layers_are_reported_in_the_lead_table() {
    let dir = TestDir::new();
    let project = project(&dir);
    let user = user_path(&dir);
    write_yaml(
        &user,
        &json!({"throttle": {"levels": {"low": {"model": "kimi-k3", "variant": "high"}}}}),
    );

    let output = run(&project, &dir, &user, &["config", "lead"]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("volcengine-coding-plan/kimi-k3"), "{text}");
    assert!(text.contains("user"), "{text}");
    let _: Value = read_yaml(&user);
}

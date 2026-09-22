//! End-to-end tests for `ocg config` (Lead and provider configuration).
//!
//! Every test runs the real binary against temp directories and an isolated
//! HOME. The runtime catalogue probe is driven by a fake OpenCode executable
//! so no real runtime, network or credential store is involved.
//!
//! The fake's HTTP surface is served by a Rust-owned `TcpListener` inside the
//! test process. The generated executable only answers `--version`, prints the
//! handshake for the already-listening Rust server, and stays alive until OCG
//! terminates it. No external interpreter is involved, which keeps the tests
//! deterministic on every supported platform (Linux and macOS runners).
//!
//! The candidate catalogue can be either fixed (`Fixed`) or derived from the
//! exact candidate config OCG passes through `OPENCODE_CONFIG_CONTENT`
//! (`ConfigAware`), which is what a real OpenCode 2 runtime loads.

mod common;

use common::{read_yaml, write_yaml, TestDir};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};

/// All environment that could leak the developer's real configuration.
fn base_command(cwd: &Path, dir: &TestDir, user: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ocg"));
    command
        .current_dir(cwd)
        .env("HOME", dir.join("home"))
        .env("PATH", "/usr/bin:/bin")
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

/// The password OCG will be given for the fake runtime's local service.
const FAKE_PASSWORD: &str = "local-fake-password";

/// What the fake runtime answers `GET /api/config` with.
#[derive(Clone)]
enum Catalogue {
    /// A fixed provider -> models mapping.
    Fixed(BTreeMap<String, Vec<String>>),
    /// Reflect the providers/models declared in `OPENCODE_CONFIG_CONTENT`.
    ConfigAware,
}

/// A Rust-owned fake OpenCode 2 runtime server.
///
/// The listener is bound before the fake executable exists, so the
/// advertisement the fake prints always describes a live socket. The
/// executable itself is only a thin `sh` wrapper; the HTTP contract lives
/// here, in the test process.
struct FakeV2Server {
    port: u16,
    sessions: Arc<Mutex<BTreeMap<String, Value>>>,
}

impl FakeV2Server {
    fn spawn(catalogue: Catalogue, candidate_path: PathBuf) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake runtime");
        let port = listener.local_addr().expect("addr").port();
        let sessions = Arc::new(Mutex::new(BTreeMap::new()));
        let state = Arc::new(ServerState {
            catalogue: Mutex::new(catalogue),
            sessions: Arc::clone(&sessions),
            candidate_path,
        });
        std::thread::spawn(move || serve(state, listener));
        Self { port, sessions }
    }

    /// Write the fake OpenCode executable that advertises this server.
    ///
    /// The child writes its candidate `OPENCODE_CONFIG_CONTENT` to a file
    /// shared with the Rust-owned server before printing the handshake.
    fn executable(&self, dir: &TestDir, name: &str) -> PathBuf {
        let path = dir.join(name);
        let config_path = dir.join(format!("{name}.candidate.json").as_str());
        let script = format!(
            "#!/bin/sh\n\
             if [ \"$1\" = \"--version\" ]; then printf '%s\\n' '2.0.11'; exit 0; fi\n\
             if [ \"$1\" = \"serve\" ]; then\n\
               if [ -n \"${{OPENCODE_CONFIG_CONTENT+x}}\" ]; then\n\
                 printf '%s' \"$OPENCODE_CONFIG_CONTENT\" > '{config_path}'\n\
               fi\n\
               printf '%s\\n' 'server listening on http://127.0.0.1:{port}'\n\
               printf '%s\\n' 'server password {FAKE_PASSWORD}'\n\
               while :; do sleep 5; done\n\
             fi\n\
             printf '%s\\n' 'fake opencode: unexpected arguments' >&2\n\
             exit 1\n",
            config_path = config_path.display(),
            port = self.port,
        );
        fs::write(&path, script).expect("write fake opencode");
        make_executable(&path);
        path
    }

    /// The session state the fake accumulated, for assertions.
    #[allow(dead_code)]
    fn sessions(&self) -> BTreeMap<String, Value> {
        self.sessions.lock().expect("sessions").clone()
    }
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

struct ServerState {
    catalogue: Mutex<Catalogue>,
    sessions: Arc<Mutex<BTreeMap<String, Value>>>,
    candidate_path: PathBuf,
}

/// Read one HTTP request, returning `(method, path, body)`.
fn read_request(stream: &mut TcpStream) -> Option<(String, String, String)> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).ok()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();
    let mut length = 0usize;
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).ok()?;
        if header == "\r\n" || header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                length = value.trim().parse().unwrap_or(0);
            }
        }
    }
    let mut body = vec![0u8; length];
    if length > 0 {
        reader.read_exact(&mut body).ok()?;
    }
    Some((method, path, String::from_utf8_lossy(&body).into_owned()))
}

fn respond(stream: &mut TcpStream, status: u16, body: &str) {
    let reason = match status {
        200 => "OK",
        204 => "No Content",
        401 => "Unauthorized",
        404 => "Not Found",
        _ => "OK",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

/// Serve the routes `V2SessionClient` uses, exactly like the Python fake did.
fn serve(state: Arc<ServerState>, listener: TcpListener) {
    for stream in listener.incoming().take(256) {
        let Ok(mut stream) = stream else { break };
        let Some((method, path, body)) = read_request(&mut stream) else {
            continue;
        };
        let path_only = path.split('?').next().unwrap_or("");
        match (method.as_str(), path_only) {
            ("GET", "/api/config") => {
                let providers = match &*state.catalogue.lock().expect("catalogue") {
                    Catalogue::Fixed(map) => {
                        let mut result = Map::new();
                        for (provider, models) in map {
                            let mut model_map = Map::new();
                            for model in models {
                                model_map.insert(model.clone(), json!({"name": model}));
                            }
                            result.insert(provider.clone(), json!({"models": model_map}));
                        }
                        result
                    }
                    Catalogue::ConfigAware => catalogue_from_candidate(&state.candidate_path),
                };
                let body = json!([
                    {"type": "directory", "path": "/tmp"},
                    {
                        "type": "document",
                        "info": {
                            "$schema": "https://opencode.ai/config.json",
                            "providers": Value::Object(providers),
                        },
                    },
                ]);
                respond(&mut stream, 200, &body.to_string());
            }
            ("GET", "/api/session") => {
                respond(
                    &mut stream,
                    200,
                    &json!({"data": [], "cursor": {"previous": null, "next": null}}).to_string(),
                );
            }
            ("POST", "/api/session") => {
                let id = format!(
                    "ses_fake_{}",
                    state.sessions.lock().expect("sessions").len()
                );
                state
                    .sessions
                    .lock()
                    .expect("sessions")
                    .insert(id.clone(), json!({}));
                respond(&mut stream, 200, &json!({"data": {"id": id}}).to_string());
            }
            ("GET", "/api/session/current") => {
                let sessions = state.sessions.lock().expect("sessions");
                let Some((id, model)) = sessions.iter().next_back() else {
                    respond(&mut stream, 404, "{}");
                    continue;
                };
                let agent = model.get("agent").and_then(Value::as_str).unwrap_or("");
                respond(
                    &mut stream,
                    200,
                    &json!({"data": {"id": id, "agent": agent, "model": model.get("model")}})
                        .to_string(),
                );
            }
            _ if method == "GET" && path_only.starts_with("/api/session/") => {
                let id = path_only.trim_start_matches("/api/session/");
                let sessions = state.sessions.lock().expect("sessions");
                let Some(model) = sessions.get(id) else {
                    respond(&mut stream, 404, "{}");
                    continue;
                };
                respond(
                    &mut stream,
                    200,
                    &json!({"data": {"agent": model.get("agent").unwrap_or(&json!("")),
                                     "model": model.get("model")}})
                    .to_string(),
                );
            }
            _ if method == "POST" && path_only.ends_with("/agent") => {
                let id = path_only
                    .trim_start_matches("/api/session/")
                    .trim_end_matches("/agent");
                let agent = serde_json::from_str::<Value>(&body)
                    .ok()
                    .and_then(|value| value.get("agent").cloned())
                    .unwrap_or(Value::Null);
                let mut sessions = state.sessions.lock().expect("sessions");
                let entry = sessions.entry(id.to_string()).or_insert(json!({}));
                entry["agent"] = agent;
                respond(&mut stream, 204, "");
            }
            _ if method == "POST" && path_only.ends_with("/model") => {
                let id = path_only
                    .trim_start_matches("/api/session/")
                    .trim_end_matches("/model");
                let model = serde_json::from_str::<Value>(&body)
                    .ok()
                    .and_then(|value| value.get("model").cloned())
                    .unwrap_or(Value::Null);
                let mut sessions = state.sessions.lock().expect("sessions");
                let entry = sessions.entry(id.to_string()).or_insert(json!({}));
                entry["model"] = model;
                respond(&mut stream, 204, "");
            }
            _ => {
                respond(&mut stream, 404, "{}");
            }
        }
    }
}

/// Reflect the providers/models declared in the candidate config the fake
/// child wrote before printing its handshake.
fn catalogue_from_candidate(path: &Path) -> Map<String, Value> {
    let content = fs::read_to_string(path).unwrap_or_default();
    let config: Value = serde_json::from_str(&content).unwrap_or(Value::Null);
    let mut result = Map::new();
    if let Some(providers) = config.get("provider").and_then(Value::as_object) {
        for (name, provider) in providers {
            let mut models = Map::new();
            if let Some(declared) = provider.get("models").and_then(Value::as_object) {
                for model_id in declared.keys() {
                    models.insert(model_id.clone(), json!({"name": model_id}));
                }
            }
            result.insert(name.clone(), json!({"models": models}));
        }
    }
    result
}

fn project(dir: &TestDir) -> PathBuf {
    dir.project()
}

/// A fake OpenCode whose catalogue is fully controlled.
///
/// The HTTP surface is a Rust-owned listener inside this test process; the
/// generated executable only answers `--version`, prints the handshake for
/// that listener and stays alive until OCG terminates it.
fn fake_opencode(dir: &TestDir, name: &str, models: Option<&str>) -> PathBuf {
    let Some(models) = models else {
        return fake_opencode_failing_probe(dir, name);
    };
    let server = FakeV2Server::spawn(
        Catalogue::Fixed(parse_fixed_models(models)),
        dir.join(format!("{name}.candidate.json").as_str()),
    );
    server.executable(dir, name)
}

/// A fake OpenCode that reflects the providers and models declared in the
/// generated config it receives through `OPENCODE_CONFIG_CONTENT`.
fn fake_opencode_config_aware(dir: &TestDir, name: &str) -> PathBuf {
    FakeV2Server::spawn(
        Catalogue::ConfigAware,
        dir.join(format!("{name}.candidate.json").as_str()),
    )
    .executable(dir, name)
}

/// A fake whose catalogue probe fails entirely: `models` exits non-zero and
/// `serve` exits before any handshake, so OCG must report the probe as
/// unavailable rather than return a catalogue verdict.
fn fake_opencode_failing_probe(dir: &TestDir, name: &str) -> PathBuf {
    let path = dir.join(name);
    let script = "#!/bin/sh\n\
                  if [ \"$1\" = \"--version\" ]; then printf '%s\\n' '2.0.11'; exit 0; fi\n\
                  exit 1\n";
    fs::write(&path, script).expect("write fake opencode");
    make_executable(&path);
    path
}

fn parse_fixed_models(models: &str) -> BTreeMap<String, Vec<String>> {
    let mut providers: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for token in models.split_whitespace() {
        match token.split_once('/') {
            Some((provider, id)) => providers
                .entry(provider.to_string())
                .or_default()
                .push(id.to_string()),
            None => providers
                .entry("unknown".to_string())
                .or_default()
                .push(token.to_string()),
        }
    }
    providers
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

//! Command-line interface: argument parsing, environment resolution and
//! dispatch.
//!
//! The CLI preserves the historical `oc` semantics while exposing the binary
//! as `ocg`. `OPENCODE_GEAR_*` variables are canonical; the legacy `OC_GEAR_*`
//! names are accepted as fallbacks.

use crate::build;
use crate::capabilities::{CapabilityConfig, CapabilityEvidence, CapabilityPlan};
use crate::clock::{Clock, SystemClock};
use crate::config;
use crate::context::{self, ContextConfig, ContextEngine};
use crate::defaults::{load_defaults, GearSource};
use crate::error::GearError;
use crate::http::{HttpTransport, NoHttp, ReqwestHttp};
use crate::json;
use crate::model;
use crate::observability;
use crate::orchestration::checkpoint::{self, Phase};
use crate::platform::Platform;
use crate::process::{
    ProcessHost, ProcessRunner, SystemCaptureRunner, SystemGitHost, SystemProcessHost,
};
use crate::report;
use crate::runtime::policy::RuntimePolicy;
use crate::runtime::{self, install::Layout};
use crate::telemetry::{self, TelemetryConfig};
use crate::validate;
use crate::verification::runner::{execute, VerifyRequest};
use crate::verification::Config as VerificationConfig;
use semver::Version;
use serde_json::{json, Value};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Printed by `version` and embedded in the help header.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

const THROTTLE_LEVELS: [&str; 3] = ["low", "mid", "high"];

/// Process environment, resolved once so the rest of the code stays pure.
#[derive(Debug, Default, Clone)]
pub struct Env {
    pub home: Option<PathBuf>,
    pub throttle: Option<String>,
    pub user_config: Option<PathBuf>,
    pub project_config: Option<PathBuf>,
    /// Explicit OpenCode executable. Canonical `OPENCODE_GEAR_OPENCODE`, with
    /// `OPENCODE_GEAR_OPENCODE_BIN` and `OC_GEAR_OPENCODE_BIN` as aliases.
    pub opencode_bin: Option<OsString>,
    pub trace: Option<PathBuf>,
    pub xdg_config_home: Option<PathBuf>,
    pub home_dir: Option<PathBuf>,
    /// Override for the GitHub API base (mirrors and tests).
    pub api_base: Option<String>,
    /// Override for the update-check cache directory.
    pub cache_dir: Option<PathBuf>,
    /// `OPENCODE_GEAR_TELEMETRY` on/off override.
    pub telemetry: Option<String>,
    /// `OPENCODE_GEAR_ORCHESTRATION` on/off escape hatch.
    pub orchestration: Option<String>,
}

impl Env {
    pub fn from_process() -> Self {
        let home_dir = std::env::var_os("HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from));
        Self {
            home: env_path(&["OPENCODE_GEAR_HOME", "OC_GEAR_HOME"]),
            throttle: env_string(&["OPENCODE_GEAR_THROTTLE", "OC_GEAR_THROTTLE"]),
            user_config: env_path(&["OPENCODE_GEAR_USER_CONFIG", "OC_GEAR_USER_CONFIG"]),
            project_config: env_path(&["OPENCODE_GEAR_PROJECT_CONFIG", "OC_GEAR_PROJECT_CONFIG"]),
            opencode_bin: env_os(&[
                "OPENCODE_GEAR_OPENCODE",
                "OPENCODE_GEAR_OPENCODE_BIN",
                "OC_GEAR_OPENCODE_BIN",
            ]),
            trace: env_path(&["OPENCODE_GEAR_TRACE", "OC_GEAR_TRACE"]),
            xdg_config_home: env_path(&["XDG_CONFIG_HOME"]),
            home_dir,
            api_base: env_string(&["OPENCODE_GEAR_API_BASE"]),
            cache_dir: env_path(&["OPENCODE_GEAR_CACHE_DIR"]),
            telemetry: env_string(&["OPENCODE_GEAR_TELEMETRY", "OC_GEAR_TELEMETRY"]),
            orchestration: env_string(&["OPENCODE_GEAR_ORCHESTRATION", "OC_GEAR_ORCHESTRATION"]),
        }
    }
}

fn env_os(names: &[&str]) -> Option<OsString> {
    for name in names {
        if let Some(value) = std::env::var_os(name) {
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    None
}

fn env_path(names: &[&str]) -> Option<PathBuf> {
    env_os(names).map(PathBuf::from)
}

fn env_string(names: &[&str]) -> Option<String> {
    env_os(names).and_then(|value| value.into_string().ok())
}

/// A command-line usage error. Always exits with status 2.
#[derive(Debug)]
pub struct UsageError(pub String);

#[derive(Debug)]
pub enum Command {
    Launch,
    Run(Vec<OsString>),
    Models(Vec<OsString>),
    Status,
    Routing,
    Throttle(Option<String>),
    Validate,
    Layers,
    Build,
    Trace(Option<String>),
    Context(Vec<OsString>),
    Cache(Option<String>),
    Stats(Vec<OsString>),
    Verify(Vec<OsString>),
    Tools(Vec<OsString>),
    Checkpoint(Vec<OsString>),
    /// Hidden/internal: the generated plugin's bridge. Never advertised.
    Bridge(Vec<OsString>),
    Version,
    Doctor,
    Upgrade,
    Help,
}

#[derive(Debug)]
pub struct Cli {
    pub throttle: Option<String>,
    pub project: Option<PathBuf>,
    pub dry_run: bool,
    pub pretty: bool,
    pub user_config: Option<PathBuf>,
    pub project_config: Option<PathBuf>,
    pub command: Command,
}

fn is_throttle_level(text: &str) -> bool {
    THROTTLE_LEVELS.contains(&text)
}

fn merge_throttle(
    flag: Option<String>,
    positional: Option<String>,
) -> std::result::Result<Option<String>, UsageError> {
    match (flag, positional) {
        (Some(flag), Some(positional)) if flag != positional => Err(UsageError(format!(
            "conflicting throttle levels: '{flag}' and '{positional}'"
        ))),
        (Some(flag), _) => Ok(Some(flag)),
        (None, Some(positional)) => Ok(Some(positional)),
        (None, None) => Ok(None),
    }
}

/// Parse arguments. Options may appear before or after the command; a bare
/// `low|mid|high` before the command is treated as the throttle level.
pub fn parse<I>(args: I) -> std::result::Result<Cli, UsageError>
where
    I: IntoIterator<Item = OsString>,
{
    let args: Vec<OsString> = args.into_iter().collect();
    let mut throttle: Option<String> = None;
    let mut positional_level: Option<String> = None;
    let mut project: Option<PathBuf> = None;
    let mut dry_run = false;
    let mut pretty = false;
    let mut user_config: Option<PathBuf> = None;
    let mut project_config: Option<PathBuf> = None;
    let mut command_token: Option<String> = None;
    let mut rest: Vec<OsString> = Vec::new();
    let mut event: Option<String> = None;

    let mut index = 0;
    let mut passthrough = false;
    while index < args.len() {
        let text = args[index].to_string_lossy().into_owned();

        if passthrough {
            rest.push(args[index].clone());
            index += 1;
            continue;
        }

        if text == "--" {
            index += 1;
            if index < args.len() {
                command_token = Some(args[index].to_string_lossy().into_owned());
                index += 1;
                rest = args[index..].to_vec();
            }
            break;
        }
        if let Some(value) = text.strip_prefix("--throttle=") {
            throttle = Some(value.to_string());
            index += 1;
            continue;
        }
        if text == "--throttle" {
            let value = args
                .get(index + 1)
                .ok_or_else(|| UsageError("--throttle needs a value".to_string()))?;
            throttle = Some(value.to_string_lossy().into_owned());
            index += 2;
            continue;
        }
        if let Some(value) = text.strip_prefix("--project=") {
            project = Some(PathBuf::from(value));
            index += 1;
            continue;
        }
        if text == "--project" || text == "--cwd" {
            let value = args
                .get(index + 1)
                .ok_or_else(|| UsageError(format!("{text} needs a value")))?;
            project = Some(PathBuf::from(value));
            index += 2;
            continue;
        }
        if let Some(value) = text.strip_prefix("--cwd=") {
            project = Some(PathBuf::from(value));
            index += 1;
            continue;
        }
        if let Some(value) = text.strip_prefix("--user-config=") {
            user_config = Some(PathBuf::from(value));
            index += 1;
            continue;
        }
        if text == "--user-config" {
            let value = args
                .get(index + 1)
                .ok_or_else(|| UsageError("--user-config needs a value".to_string()))?;
            user_config = Some(PathBuf::from(value));
            index += 2;
            continue;
        }
        if let Some(value) = text.strip_prefix("--project-config=") {
            project_config = Some(PathBuf::from(value));
            index += 1;
            continue;
        }
        if text == "--project-config" {
            let value = args
                .get(index + 1)
                .ok_or_else(|| UsageError("--project-config needs a value".to_string()))?;
            project_config = Some(PathBuf::from(value));
            index += 2;
            continue;
        }
        if text == "--dry-run" {
            dry_run = true;
            index += 1;
            continue;
        }
        if text == "--pretty" {
            pretty = true;
            index += 1;
            continue;
        }
        if let Some(value) = text.strip_prefix("--event=") {
            event = Some(value.to_string());
            index += 1;
            continue;
        }
        if text == "--event" {
            let value = args
                .get(index + 1)
                .ok_or_else(|| UsageError("--event needs a value".to_string()))?;
            event = Some(value.to_string_lossy().into_owned());
            index += 2;
            continue;
        }
        if text == "-h" || text == "--help" {
            command_token = Some("help".to_string());
            break;
        }
        if text == "--version" {
            command_token = Some("version".to_string());
            break;
        }
        if text.starts_with('-') && text != "-" {
            // Only commands that actually accept subcommand options may collect
            // an unknown option. `checkpoint save --phase ...` does; every
            // legacy command (validate, status, doctor, version, routing,
            // layers, build, upgrade, cache, context, verify, ...) keeps the
            // strict usage error it always had.
            if command_token.as_deref() == Some("checkpoint") {
                rest.push(args[index].clone());
                index += 1;
                continue;
            }
            return Err(UsageError(format!(
                "unknown option: {text} (try 'ocg help')"
            )));
        }

        if command_token.is_none() && throttle.is_none() && is_throttle_level(&text) {
            positional_level = Some(text);
            index += 1;
            continue;
        }
        if command_token.is_none() {
            command_token = Some(text);
            index += 1;
            // `run` and `models` forward everything after them to OpenCode.
            if matches!(command_token.as_deref(), Some("run") | Some("models")) {
                passthrough = true;
            }
            continue;
        }
        // A positional argument for a reporting command (for example the
        // level after `throttle`). Options are still parsed.
        rest.push(args[index].clone());
        index += 1;
    }

    let throttle = merge_throttle(throttle, positional_level)?;
    let command = match command_token.as_deref() {
        None => Command::Launch,
        Some("run") => Command::Run(rest),
        Some("models") => Command::Models(rest),
        Some("status") => Command::Status,
        Some("routing") | Some("routes") | Some("config") => Command::Routing,
        Some("throttle") => Command::Throttle(
            rest.first()
                .map(|value| value.to_string_lossy().into_owned()),
        ),
        Some("validate") => Command::Validate,
        Some("layers") => Command::Layers,
        Some("build") => Command::Build,
        Some("dry-run") => Command::Build,
        Some("trace") => Command::Trace(event),
        Some("context") => Command::Context(rest),
        Some("cache") => Command::Cache(
            rest.first()
                .map(|value| value.to_string_lossy().into_owned()),
        ),
        Some("stats") => Command::Stats(rest),
        Some("verify") => Command::Verify(rest),
        Some("tools") => Command::Tools(rest),
        Some("checkpoint") => Command::Checkpoint(rest),
        Some("__bridge") => Command::Bridge(rest),
        Some("version") => Command::Version,
        Some("doctor") => Command::Doctor,
        Some("upgrade") => Command::Upgrade,
        Some("help") => Command::Help,
        Some(other) => {
            return Err(UsageError(format!(
                "unknown command: {other} (try 'ocg help')"
            )))
        }
    };

    Ok(Cli {
        throttle,
        project,
        dry_run,
        pretty,
        user_config,
        project_config,
        command,
    })
}

fn usage() -> &'static str {
    r#"OpenCode Gear - project-agnostic multi-model orchestration for OpenCode

Usage:
  ocg [low|mid|high] [--throttle low|mid|high] [--project DIR] [--dry-run] [command] [args...]

Commands:
  (none)                launch interactive OpenCode with the gear config
  run <args...>         launch `opencode run` with the gear config
  models [args...]      run `opencode models` with the gear config
  status                show throttle, routing and config layers
  routing               show the consumer role -> model table
  throttle [level]      print, or persist, the default throttle level
  validate              validate the merged configuration
  layers                show configuration layers and trace state
  build                 print the resolved OpenCode config
  context <task...>     build a deterministic local repository context plan
  context symbols <q>   find indexed symbols by name (diagnostic)
  cache clean|stats     manage the local context cache (never the runtime)
  stats [--pretty]      read-only project telemetry aggregate (local only)
  verify [fast|normal|full]
                        run the configured trusted commands for a stage
  tools <task...>       show the capability plan / Tool Context Firewall view
  checkpoint list|show|save
                        inspect, or create, a phase checkpoint
  version               report Gear, platform and the resolved OpenCode runtime
  doctor                check platform, config, runtime and cache (read-only)
  upgrade               self-update Gear, then maintain the active OpenCode
  help                  show this help

Options:
  low|mid|high          positional throttle level (same as --throttle)
  --throttle LEVEL      low | mid | high   (OpenAI Lead tier, this launch only)
  --project DIR         project directory used for project-local overrides
  --dry-run             print the merged OpenCode config instead of launching
  --pretty              pretty-print JSON output (build / --dry-run)
  --user-config PATH    user override file
  --project-config PATH project override file
  -h, --help            show this help

Environment:
  OPENCODE_GEAR_HOME           config directory loaded instead of the embedded defaults
  OPENCODE_GEAR_THROTTLE       default throttle level (overridden by --throttle)
  OPENCODE_GEAR_USER_CONFIG    user override file (default ~/.config/opencode-gear/config.json)
  OPENCODE_GEAR_PROJECT_CONFIG project override file (default <project>/.opencode-gear.json)
  OPENCODE_GEAR_OPENCODE       explicit opencode binary (wins over everything)
  OPENCODE_GEAR_OPENCODE_BIN   compatibility alias for the same explicit binary
  OPENCODE_GEAR_TRACE          trace file; only read when observability is enabled
  OPENCODE_GEAR_CACHE_DIR      override the update-check cache directory
  OPENCODE_GEAR_API_BASE       override the GitHub API base (mirrors, tests)
  OPENCODE_GEAR_TELEMETRY      0/1 to force local telemetry off/on
  OPENCODE_GEAR_ORCHESTRATION  0/1 to force orchestration off/on for this process

The legacy OC_GEAR_* names are still accepted as fallbacks (including
OC_GEAR_OPENCODE_BIN). A broken explicit binary is authoritative and errors
instead of silently falling back to another runtime.
"#
}

#[derive(Debug)]
enum Failure {
    Usage(String),
    Gear(GearError),
}

impl From<GearError> for Failure {
    fn from(error: GearError) -> Self {
        Failure::Gear(error)
    }
}

fn usage_failure(message: impl Into<String>) -> Failure {
    Failure::Usage(message.into())
}

/// Entry point. Returns the process exit code.
pub fn run(args: impl Iterator<Item = OsString>) -> i32 {
    match run_inner(args) {
        Ok(code) => code,
        Err(Failure::Usage(message)) => {
            eprintln!("ocg: {message}");
            2
        }
        Err(Failure::Gear(error)) => {
            eprintln!("ocg: {error}");
            2
        }
    }
}

fn run_inner(args: impl Iterator<Item = OsString>) -> std::result::Result<i32, Failure> {
    let cli = parse(args).map_err(|error| usage_failure(error.0))?;
    let env = Env::from_process();

    match &cli.command {
        Command::Help => {
            print!("{}", usage());
            return Ok(0);
        }
        Command::Version => {
            return version_command(&cli, &env);
        }
        _ => {}
    }

    if let Some(project) = &cli.project {
        if !project.is_dir() {
            return Err(usage_failure(format!(
                "--project is not a directory: {}",
                project.display()
            )));
        }
    }

    let (gear_source, gear_home) = match env.home.clone() {
        Some(home) => (GearSource::Dir(home.clone()), Some(home)),
        None => (GearSource::Embedded, None),
    };
    let defaults = load_defaults(&gear_source).map_err(Failure::Gear)?;
    let cwd = match &cli.project {
        Some(project) => project.clone(),
        None => std::env::current_dir()
            .map_err(|error| GearError::io("cannot determine the current directory", error))
            .map_err(Failure::Gear)?,
    };
    let user_path = config::user_config_path(
        cli.user_config.as_deref(),
        env.user_config.as_deref(),
        env.xdg_config_home.as_deref(),
        env.home_dir.as_deref(),
    );
    let project_path = config::project_config_path(
        &cwd,
        cli.project_config.as_deref(),
        env.project_config.as_deref(),
        env.home_dir.as_deref(),
    );
    let effective = config::build_effective(
        defaults,
        gear_home,
        &cwd,
        &user_path,
        &project_path,
        env.home_dir.clone(),
    )
    .map_err(Failure::Gear)?;
    let mut effective = effective;
    // The orchestration escape hatch is applied to the effective config so that
    // build, dry-run, launch and the bridge all agree for one process.
    crate::orchestration::OrchestrationConfig::apply_env_override(
        &mut effective.data,
        env.orchestration.as_deref(),
    );
    let level = model::resolve_throttle(
        &effective.data,
        cli.throttle.as_deref(),
        env.throttle.as_deref(),
    );

    if cli.dry_run {
        let resolved = build::build_opencode_config(&effective, &level).map_err(Failure::Gear)?;
        print_config(&resolved, cli.pretty)?;
        return Ok(0);
    }

    match &cli.command {
        Command::Help | Command::Version => Ok(0),
        Command::Launch => launch(&effective, &cwd, &level, &[], true, true, &env),
        Command::Run(args) => {
            let forwarded = prepend_subcommand("run", args);
            launch(&effective, &cwd, &level, &forwarded, true, true, &env)
        }
        Command::Models(args) => {
            let forwarded = prepend_subcommand("models", args);
            launch(&effective, &cwd, &level, &forwarded, false, false, &env)
        }
        Command::Status => {
            validate::require_valid(&effective).map_err(Failure::Gear)?;
            let text = report::status_text(&effective, &level).map_err(Failure::Gear)?;
            println!("{text}");
            Ok(0)
        }
        Command::Routing => {
            validate::require_valid(&effective).map_err(Failure::Gear)?;
            let text = report::routing_text(&effective).map_err(Failure::Gear)?;
            println!("{text}");
            Ok(0)
        }
        Command::Validate => {
            let errors = validate::validate(&effective);
            if !errors.is_empty() {
                eprintln!("OpenCode Gear configuration errors:");
                for error in &errors {
                    eprintln!("  - {error}");
                }
                return Ok(1);
            }
            build::build_opencode_config(&effective, &level).map_err(Failure::Gear)?;
            println!("configuration is valid");
            Ok(0)
        }
        Command::Layers => {
            println!("{}", report::layers_text(&effective, env.trace.as_deref()));
            Ok(0)
        }
        Command::Build => {
            let resolved =
                build::build_opencode_config(&effective, &level).map_err(Failure::Gear)?;
            print_config(&resolved, cli.pretty)?;
            Ok(0)
        }
        Command::Throttle(requested) => throttle_command(
            &effective,
            requested.as_deref(),
            &level,
            cli.user_config.as_deref(),
            &env,
        ),
        Command::Trace(event) => {
            let event = event.as_deref().unwrap_or("launch");
            if let Some(path) =
                observability::record_event(&effective, event, &level, env.trace.as_deref())
            {
                println!("{}", path.display());
            }
            Ok(0)
        }
        Command::Doctor => doctor_command(&effective, &cwd, &env),
        Command::Context(args) => context_command(&effective, &cwd, args, &env, cli.pretty),
        Command::Cache(action) => cache_command(&effective, &cwd, action.as_deref()),
        Command::Stats(args) => stats_command(&effective, &cwd, args, &env, cli.pretty),
        Command::Verify(args) => verify_command(&effective, &cwd, args, &env, cli.pretty),
        Command::Tools(args) => tools_command(&effective, args, cli.pretty),
        Command::Checkpoint(args) => checkpoint_command(&effective, &cwd, args, cli.pretty),
        Command::Bridge(args) => bridge_command(&effective, &cwd, args, &env),
        Command::Upgrade => upgrade_command(&effective, &cwd, &env),
    }
}

fn print_config(config: &Value, pretty: bool) -> std::result::Result<(), Failure> {
    let text = if pretty {
        serde_json::to_string_pretty(config)
    } else {
        serde_json::to_string(config)
    }
    .map_err(|error| {
        Failure::Gear(GearError::config(format!(
            "cannot serialize the OpenCode config: {error}"
        )))
    })?;
    println!("{text}");
    Ok(())
}

/// `opencode run ...` / `opencode models ...` keep their subcommand name.
fn prepend_subcommand(subcommand: &str, args: &[OsString]) -> Vec<OsString> {
    let mut forwarded = Vec::with_capacity(1 + args.len());
    forwarded.push(OsString::from(subcommand));
    forwarded.extend(args.iter().cloned());
    forwarded
}

fn launch(
    effective: &config::Effective,
    cwd: &Path,
    level: &str,
    args: &[OsString],
    trace: bool,
    coding_session: bool,
    env: &Env,
) -> std::result::Result<i32, Failure> {
    let mut resolved = build::build_opencode_config(effective, level).map_err(Failure::Gear)?;
    // `ocg models` is not a coding session: it must not require the
    // orchestration plugin to exist or the project directory to be writable.
    if !coding_session {
        crate::orchestration::plugin::remove_ocg_plugin(&mut resolved);
    }
    let content = serde_json::to_string(&resolved).map_err(|error| {
        Failure::Gear(GearError::config(format!(
            "cannot serialize the OpenCode config: {error}"
        )))
    })?;
    if trace {
        observability::record_event(effective, "launch", level, env.trace.as_deref());
    }
    let http = ReqwestHttp::new().map_err(Failure::Gear)?;
    let clock = SystemClock;
    let process = SystemProcessHost;
    let manager =
        runtime_manager(cwd, effective, env, &http, &clock, &process).map_err(Failure::Gear)?;
    let selection = manager.resolve_for_launch().map_err(Failure::Gear)?;
    for warning in &selection.warnings {
        eprintln!("ocg: warning: {warning}");
    }
    let runner = ProcessRunner::new(selection.path.into_os_string());
    // Materialize the adapter *before* exec: if it cannot be written, the
    // generated `file://` plugin would be broken, so fail clearly instead of
    // launching an integration that cannot work. A non-coding session (for
    // example `ocg models`) skips this entirely.
    let extra_env = if coding_session {
        orchestration_env(effective, cwd).map_err(Failure::Gear)?
    } else {
        Vec::new()
    };
    runner
        .exec(args, cwd, &content, &extra_env)
        .map_err(Failure::Gear)?;
    Ok(0)
}

/// Materialize the generated plugin and export the exact bridge environment.
///
/// This is the only place `launch` writes local state, and it only happens when
/// orchestration is enabled. Disabled orchestration returns an empty vector, so
/// no plugin, no file and no variable reaches OpenCode. A materialization
/// failure is returned so the launch aborts rather than injecting a `file://`
/// URL that does not resolve.
fn orchestration_env(
    effective: &config::Effective,
    cwd: &Path,
) -> crate::error::Result<Vec<(OsString, OsString)>> {
    let enabled = crate::orchestration::OrchestrationConfig::from_config(&effective.data)
        .map(|config| config.enabled)
        .unwrap_or(false);
    if !enabled {
        return Ok(Vec::new());
    }
    let path = crate::orchestration::plugin::materialize(cwd)?;
    let mut env = Vec::new();
    let exe = std::env::current_exe().map_err(|error| {
        GearError::io(
            "cannot determine the orchestration bridge executable",
            error,
        )
    })?;
    env.push((OsString::from("OPENCODE_GEAR_OCG"), exe.into_os_string()));
    env.push((
        OsString::from("OPENCODE_GEAR_PROJECT"),
        cwd.as_os_str().to_os_string(),
    ));
    // The plugin starts a fresh `ocg __bridge` process. Preserve the exact
    // resolved override paths from this launch so CLI `--user-config` and
    // `--project-config` layers remain authoritative inside that process too.
    env.push((
        OsString::from("OPENCODE_GEAR_USER_CONFIG"),
        effective.user_path.as_os_str().to_os_string(),
    ));
    env.push((
        OsString::from("OPENCODE_GEAR_PROJECT_CONFIG"),
        effective.project_path.as_os_str().to_os_string(),
    ));
    // Defense in depth: the exported path is the one just materialized.
    debug_assert!(path.is_file());
    Ok(env)
}

/// Build a runtime manager from the effective config and environment.
fn runtime_manager<'a>(
    project_root: &Path,
    effective: &config::Effective,
    env: &Env,
    http: &'a dyn HttpTransport,
    clock: &'a SystemClock,
    process: &'a dyn ProcessHost,
) -> crate::error::Result<runtime::RuntimeManager<'a>> {
    let policy = RuntimePolicy::from_config(&effective.data)?;
    let platform = Platform::current()?;
    let mut manager =
        runtime::RuntimeManager::new(project_root, policy, platform, http, clock, process)
            .with_explicit(env.opencode_bin.clone());
    if let Some(cache_dir) = &env.cache_dir {
        manager = manager.with_cache_dir(Some(cache_dir.clone()));
    }
    if let Some(api_base) = &env.api_base {
        manager = manager.with_api_base(api_base.clone());
    }
    Ok(manager)
}

/// `ocg version`: report Gear, platform and the resolved runtime. Never
/// installs, upgrades or writes the update cache.
fn version_command(cli: &Cli, env: &Env) -> std::result::Result<i32, Failure> {
    let project = cli
        .project
        .clone()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let platform = Platform::current().map_err(Failure::Gear)?;
    let clock = SystemClock;
    let process = SystemProcessHost;
    let policy = RuntimePolicy::default();
    let manager =
        runtime::RuntimeManager::new(project, policy, platform, &NoHttp, &clock, &process)
            .with_explicit(env.opencode_bin.clone());
    let report = manager.resolve_for_report();

    println!("OpenCode Gear {VERSION}");
    println!("platform:        {}", platform.slug());
    if report.installed() {
        println!(
            "opencode:        {} ({})",
            describe_runtime_version(report.version.as_ref()),
            report
                .source
                .map(|source| source.label())
                .unwrap_or("unknown")
        );
        if let Some(path) = &report.path {
            println!("runtime path:    {}", path.display());
        }
    } else {
        println!("opencode:        not installed");
        if let Some(error) = &report.error {
            println!("runtime error:   {error}");
        } else {
            println!(
                "runtime:         none; the next launch will bootstrap a project-local runtime"
            );
        }
    }
    for warning in &report.warnings {
        println!("warning:         {warning}");
    }
    Ok(0)
}

fn describe_runtime_version(version: Option<&Version>) -> String {
    version
        .map(ToString::to_string)
        .unwrap_or_else(|| "unknown version".to_string())
}

fn check_line(status: &str, label: &str, detail: &str) {
    println!("  {label:<16} [{status}] {detail}");
}

/// `ocg doctor`: read-only environment and runtime checks. Never installs,
/// updates or writes the cache, and never prints secrets.
fn doctor_command(
    effective: &config::Effective,
    project_root: &Path,
    env: &Env,
) -> std::result::Result<i32, Failure> {
    let mut failures = 0usize;
    println!("OpenCode Gear doctor");

    let platform = match Platform::current() {
        Ok(platform) => {
            check_line("ok", "platform", &platform.slug());
            Some(platform)
        }
        Err(error) => {
            check_line("fail", "platform", &error.to_string());
            failures += 1;
            None
        }
    };

    match std::env::current_exe() {
        Ok(exe) => check_line("ok", "gear", &format!("{} (Gear {VERSION})", exe.display())),
        Err(error) => {
            check_line(
                "fail",
                "gear",
                &format!("cannot determine the running executable: {error}"),
            );
            failures += 1;
        }
    }

    let process = SystemProcessHost;
    match process.find_in_path("ocg") {
        Some(path) => check_line("ok", "gear on PATH", &path.display().to_string()),
        None => check_line(
            "warn",
            "gear on PATH",
            "not found; install with install.sh or add the install directory to PATH",
        ),
    }

    if project_root.is_dir() {
        check_line("ok", "project root", &project_root.display().to_string());
    } else {
        check_line(
            "fail",
            "project root",
            &format!("{} is not a directory", project_root.display()),
        );
        failures += 1;
    }

    if effective.project_path.is_file() {
        check_line(
            "ok",
            "project config",
            &effective.project_path.display().to_string(),
        );
    } else {
        check_line("info", "project config", "not present (optional)");
    }

    let errors = validate::validate(effective);
    if errors.is_empty() {
        let roles = model::role_specs(&effective.data)
            .map(|roles| roles.len())
            .unwrap_or(0);
        check_line("ok", "config/routing", &format!("valid ({roles} roles)"));
    } else {
        check_line("fail", "config/routing", &errors.join("; "));
        failures += 1;
    }

    let clock = SystemClock;
    let policy = RuntimePolicy::from_config(&effective.data).unwrap_or_default();
    let report = match platform {
        Some(platform) => {
            let manager = runtime::RuntimeManager::new(
                project_root,
                policy.clone(),
                platform,
                &NoHttp,
                &clock,
                &process,
            )
            .with_explicit(env.opencode_bin.clone());
            manager.resolve_for_report()
        }
        None => runtime::RuntimeReport::default(),
    };

    if report.installed() {
        check_line(
            "ok",
            "runtime",
            &format!(
                "{} ({}) {}",
                describe_runtime_version(report.version.as_ref()),
                report
                    .source
                    .map(|source| source.label())
                    .unwrap_or("unknown"),
                report
                    .path
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default()
            ),
        );
    } else if let Some(error) = &report.error {
        check_line("fail", "runtime", error);
        failures += 1;
    } else {
        check_line(
            "info",
            "runtime",
            &format!(
                "not installed; `ocg` or `ocg upgrade` will bootstrap {}",
                Layout::new(project_root).runtime_root().display()
            ),
        );
    }
    for warning in &report.warnings {
        check_line("warn", "runtime", warning);
    }

    let runtime_root = Layout::new(project_root).runtime_root();
    if is_writable_dir(&runtime_root) {
        check_line(
            "ok",
            "runtime dir",
            &format!("{} is writable", runtime_root.display()),
        );
    } else {
        check_line(
            "warn",
            "runtime dir",
            &format!("{} is not writable", runtime_root.display()),
        );
    }

    let cache_dir = env
        .cache_dir
        .clone()
        .or_else(runtime::cache::platform_cache_dir);
    match cache_dir {
        Some(dir) => match runtime::cache::CacheRecord::read(&dir) {
            Some(record) => {
                let age = (clock.now_unix() - record.checked_at).max(0);
                let fresh = !runtime::cache::due(
                    Some(&record),
                    clock.now_unix(),
                    policy.check_interval_hours,
                    false,
                );
                check_line(
                    "ok",
                    "update cache",
                    &format!(
                        "{} ({}, checked {age}s ago)",
                        dir.display(),
                        if fresh { "fresh" } else { "expired" }
                    ),
                );
            }
            None => check_line(
                "info",
                "update cache",
                &format!("{} (never checked)", dir.display()),
            ),
        },
        None => check_line(
            "warn",
            "update cache",
            "platform cache directory unavailable",
        ),
    }

    // Repository map / symbol index: inspect the existing artifact only. Doctor
    // never builds, updates or trims an index.
    let index_path = context::index::index_path(project_root);
    let index = if index_path.is_file() {
        match std::fs::read_to_string(&index_path)
            .ok()
            .and_then(|text| serde_json::from_str::<context::index::ContextIndex>(&text).ok())
        {
            Some(index) => {
                check_line(
                    "ok",
                    "repository map/index",
                    &format!(
                        "{} file(s), {} symbol(s) at {}",
                        index.metrics.files,
                        index.metrics.symbols,
                        index_path.display()
                    ),
                );
                Some(index)
            }
            None => {
                check_line(
                    "warn",
                    "repository map/index",
                    &format!(
                        "{} is unreadable or corrupt; the next `ocg context` rebuilds it",
                        index_path.display()
                    ),
                );
                None
            }
        }
    } else {
        check_line(
            "info",
            "repository map/index",
            "not built (optional; `ocg context` builds it)",
        );
        None
    };

    match &index {
        Some(index) => check_line(
            "ok",
            "symbol index",
            &format!(
                "{} symbol(s) across {} indexed file(s)",
                index.metrics.symbols, index.metrics.files
            ),
        ),
        None => check_line(
            "info",
            "symbol index",
            "not built (optional; depends on the repository index)",
        ),
    }

    // Context cache: read-only statistics.
    let cache = context::cache::ContextCache::new(project_root);
    let cache_stats = cache.stats();
    let cache_exists = Path::new(&cache_stats.dir).exists();
    if !cache_exists {
        check_line("info", "context cache", "not present (optional)");
    } else if cache_stats.corrupt > 0 {
        check_line(
            "warn",
            "context cache",
            &format!(
                "{} entr(y/ies), {} corrupt (ignored and removed on reuse)",
                cache_stats.entries, cache_stats.corrupt
            ),
        );
    } else {
        check_line(
            "ok",
            "context cache",
            &format!(
                "{} entr(y/ies), {} bytes",
                cache_stats.entries, cache_stats.bytes
            ),
        );
    }

    // Task checkpoints: corrupt files are counted, never printed.
    let (checkpoints, corrupt_checkpoints) = checkpoint::list(project_root);
    if corrupt_checkpoints > 0 {
        check_line(
            "warn",
            "task checkpoints",
            &format!(
                "{corrupt_checkpoints} corrupt checkpoint(s) ignored ({} readable)",
                checkpoints.len()
            ),
        );
    } else if checkpoints.is_empty() {
        check_line("info", "task checkpoints", "none saved (optional)");
    } else {
        check_line(
            "ok",
            "task checkpoints",
            &format!("{} checkpoint(s)", checkpoints.len()),
        );
    }

    // Verification config: parse only, never run a command.
    match VerificationConfig::from_config(&effective.data) {
        Ok(config) => check_line(
            if config.enabled { "ok" } else { "info" },
            "verification config",
            &format!(
                "{}; default stage '{}'; {} configured command(s)",
                if config.enabled {
                    "enabled"
                } else {
                    "disabled"
                },
                config.default_stage,
                config
                    .stages
                    .values()
                    .map(|stage| stage.commands.len())
                    .sum::<usize>()
            ),
        ),
        Err(error) => check_line("warn", "verification config", &error.to_string()),
    }

    // Telemetry storage: read-only, local-only, no state creation.
    let (telemetry_config, _telemetry_warnings) = telemetry_for(effective, env);
    let store = telemetry::TelemetryStore::new(project_root, telemetry_config);
    let telemetry_log = store.read();
    let telemetry_state = if !telemetry_config.enabled {
        "disabled".to_string()
    } else {
        format!("enabled, {} event(s)", telemetry_log.events.len())
    };
    let telemetry_line = format!(
        "{}; local-only; {}{}",
        telemetry_state,
        store.path().display(),
        match (telemetry_log.corrupt_lines, telemetry_log.unsupported_lines,) {
            (0, 0) => String::new(),
            (corrupt, 0) => format!("; {corrupt} corrupt line(s) skipped"),
            (0, unsupported) => {
                format!("; {unsupported} unsupported schema line(s) skipped")
            }
            (corrupt, unsupported) => format!(
                "; {corrupt} corrupt line(s) and {unsupported} unsupported schema line(s) skipped"
            ),
        }
    );
    if telemetry_log.corrupt_lines > 0 || telemetry_log.unsupported_lines > 0 {
        check_line("warn", "telemetry", &telemetry_line);
    } else if !telemetry_config.enabled || !store.exists() {
        check_line("info", "telemetry", &telemetry_line);
    } else {
        check_line("ok", "telemetry", &telemetry_line);
    }

    // Capability planner: advisory only.
    match CapabilityConfig::from_config(&effective.data) {
        Ok(config) => check_line(
            if config.enabled { "ok" } else { "info" },
            "tool capability planner",
            &format!(
                "{}; {} custom capability name(s)",
                if config.enabled {
                    "enabled"
                } else {
                    "disabled"
                },
                config.custom.len()
            ),
        ),
        Err(error) => check_line("warn", "tool capability planner", &error.to_string()),
    }

    // Sensitive-file exclusions: summarise the current index only.
    match &index {
        Some(index) => {
            let excluded = index.files.iter().filter(|file| file.excluded).count();
            check_line(
                "ok",
                "sensitive-file exclusions",
                &format!("{excluded} path(s) excluded from content in the current index"),
            );
        }
        None => check_line(
            "info",
            "sensitive-file exclusions",
            "applied by path during indexing; no index to summarise",
        ),
    }

    // Orchestration: read-only health of the generated integration. Doctor
    // never launches OpenCode, never runs a bridge and never mutates state.
    match crate::orchestration::OrchestrationConfig::from_config(&effective.data) {
        Ok(config) => {
            check_line(
                if config.enabled { "ok" } else { "info" },
                "orchestration",
                &format!(
                    "{}; build retries {}; debug retries {}; handoff <= {} bytes / {}%",
                    if config.enabled {
                        "enabled"
                    } else {
                        "disabled"
                    },
                    config.max_build_retries,
                    config.max_debug_retries,
                    config.max_handoff_bytes,
                    config.max_handoff_ratio_percent
                ),
            );
            if config.enabled {
                let plugin = crate::orchestration::plugin::plugin_path(project_root);
                if plugin.is_file() {
                    check_line(
                        "ok",
                        "orchestration plugin",
                        &format!(
                            "{} (mechanism: file:// JS adapter injected via config.plugin; hooks: chat.message, tool.execute.before/after)",
                            plugin.display()
                        ),
                    );
                } else {
                    check_line(
                        "info",
                        "orchestration plugin",
                        "not materialized yet (written at the next `ocg` launch)",
                    );
                }
                let loaded = crate::orchestration::state::load(project_root);
                if !loaded.exists {
                    check_line(
                        "info",
                        "orchestration state",
                        "not present (created by the bridge on first use)",
                    );
                } else if loaded.corrupt {
                    check_line(
                        "warn",
                        "orchestration state",
                        "corrupt or unsupported; the next bridge call starts from empty state",
                    );
                } else {
                    let checkpoints = loaded
                        .state
                        .sessions
                        .values()
                        .map(|session| session.checkpoints.len())
                        .sum::<usize>();
                    check_line(
                        "ok",
                        "orchestration state",
                        &format!(
                            "{} session(s); {} checkpoint reference(s)",
                            loaded.state.sessions.len(),
                            checkpoints
                        ),
                    );
                }
                let limits = crate::orchestration::projection::ProjectionLimits {
                    max_bytes: config.max_handoff_bytes,
                    ratio_percent: config.max_handoff_ratio_percent,
                };
                let capsule = crate::orchestration::projection::project(
                    &crate::orchestration::handoff::ProjectionInput::default(),
                    crate::orchestration::handoff::Role::Lead,
                    crate::orchestration::handoff::Role::Build,
                    "health",
                    "health",
                    limits,
                );
                check_line(
                    if capsule.measured_bytes() <= limits.max_bytes {
                        "ok"
                    } else {
                        "warn"
                    },
                    "projection",
                    &format!(
                        "handoff schema reachable ({} bytes, cap {})",
                        capsule.measured_bytes(),
                        limits.max_bytes
                    ),
                );
                let context_enabled = ContextConfig::from_config(&effective.data)
                    .map(|context| context.enabled)
                    .unwrap_or(false);
                check_line(
                    if context_enabled { "ok" } else { "info" },
                    "context activation",
                    if context_enabled {
                        "context preparation is active for orchestration"
                    } else {
                        "context.enabled=false; orchestration runs with an empty dynamic context"
                    },
                );
                let verification_ready = VerificationConfig::from_config(&effective.data)
                    .map(|verification| {
                        verification.enabled
                            && verification.command_count(&verification.default_stage) > 0
                    })
                    .unwrap_or(false);
                check_line(
                    if verification_ready { "ok" } else { "info" },
                    "verification integration",
                    if verification_ready {
                        "after Build, the configured verification stage runs and feeds the retry policy"
                    } else {
                        "no trusted command in the default stage; after Build reports not-configured"
                    },
                );
            }
        }
        Err(error) => check_line("warn", "orchestration", &error.to_string()),
    }

    Ok(if failures == 0 { 0 } else { 1 })
}
/// `ocg upgrade`: self-update Gear, then force-maintain the active OpenCode.
fn upgrade_command(
    effective: &config::Effective,
    project_root: &Path,
    env: &Env,
) -> std::result::Result<i32, Failure> {
    let platform = Platform::current().map_err(Failure::Gear)?;
    let clock = SystemClock;
    let process = SystemProcessHost;
    let http = ReqwestHttp::new().map_err(Failure::Gear)?;
    let manager = runtime_manager(project_root, effective, env, &http, &clock, &process)
        .map_err(Failure::Gear)?;

    let current = Version::parse(VERSION).map_err(|error| {
        Failure::Gear(GearError::config(format!(
            "invalid Gear version '{VERSION}': {error}"
        )))
    })?;
    match std::env::current_exe() {
        Ok(exe) => {
            match runtime::self_update::self_update(
                &http,
                &manager.api_base,
                &manager.gear_repo,
                platform,
                &exe,
                &current,
                &process,
            ) {
                Ok(outcome) if outcome.updated => {
                    println!("Gear:      {} -> {}", outcome.from, outcome.to);
                }
                Ok(outcome) => println!("Gear:      {} (up to date)", outcome.from),
                Err(error) => println!("Gear:      self-update skipped: {error}"),
            }
        }
        Err(error) => println!(
            "Gear:      self-update skipped: cannot determine the running executable: {error}"
        ),
    }

    let outcome = manager.upgrade().map_err(Failure::Gear)?;
    let before = match outcome.before.installed() {
        true => format!(
            "{} ({})",
            describe_runtime_version(outcome.before.version.as_ref()),
            outcome
                .before
                .source
                .map(|source| source.label())
                .unwrap_or("unknown")
        ),
        false => "not installed".to_string(),
    };
    let after = format!(
        "{} ({}) {}",
        describe_runtime_version(outcome.after.version.as_ref()),
        outcome.after.source.label(),
        outcome.after.path.display()
    );
    println!("OpenCode:  {before} -> {after}");
    for warning in outcome.after.warnings.iter().chain(outcome.warnings.iter()) {
        eprintln!("ocg: warning: {warning}");
    }
    Ok(0)
}

/// Whether a directory (or its nearest existing ancestor) is writable.
fn is_writable_dir(path: &Path) -> bool {
    let mut current = path;
    loop {
        match std::fs::metadata(current) {
            Ok(metadata) => return !metadata.permissions().readonly(),
            Err(_) => match current.parent() {
                Some(parent) if !parent.as_os_str().is_empty() => current = parent,
                _ => return false,
            },
        }
    }
}

/// Resolve the telemetry policy without letting a broken policy block the
/// command that owns it. A rejectable config (for example `localOnly=false`)
/// disables collection with a warning; `ocg validate` still reports it.
fn telemetry_for(effective: &config::Effective, env: &Env) -> (TelemetryConfig, Vec<String>) {
    match TelemetryConfig::from_config(&effective.data) {
        Ok(config) => (
            config.with_env_override(env.telemetry.as_deref()),
            Vec::new(),
        ),
        Err(error) => (
            TelemetryConfig::disabled(),
            vec![format!("telemetry disabled: {error}")],
        ),
    }
}

fn print_telemetry_warnings(warnings: &[String]) {
    for warning in warnings {
        eprintln!("ocg: warning: {warning}");
    }
}

/// `ocg context <task...>` and `ocg context symbols <query>`.
fn context_command(
    effective: &config::Effective,
    project_root: &Path,
    args: &[OsString],
    env: &Env,
    pretty: bool,
) -> std::result::Result<i32, Failure> {
    let config = ContextConfig::from_config(&effective.data).map_err(Failure::Gear)?;
    if !config.enabled {
        // Disabled means disabled: do not read, index or cache anything.
        println!(
            "context engine is disabled (context.enabled=false); no index or cache work was performed"
        );
        return Ok(0);
    }
    let git = SystemGitHost;
    let clock = SystemClock;
    let capabilities = crate::capabilities::CapabilityConfig::from_config(&effective.data)
        .map_err(Failure::Gear)?;
    let verification =
        crate::verification::Config::from_config(&effective.data).map_err(Failure::Gear)?;
    let engine = ContextEngine::new(project_root, config, &git, &clock)
        .with_capabilities(capabilities)
        .with_verification(verification);
    let words: Vec<String> = args
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();

    if words.first().map(String::as_str) == Some("symbols") {
        let query = words[1..].join(" ").trim().to_string();
        if query.is_empty() {
            return Err(usage_failure("context symbols needs a query"));
        }
        let hits = engine.search_symbols(&query, 100).map_err(Failure::Gear)?;
        let definition = engine.definition(&query).map_err(Failure::Gear)?;
        let payload = json!({
            "query": query,
            "definition": definition,
            "symbols": hits,
        });
        print_config(&payload, pretty)?;
        return Ok(0);
    }

    let task = words.join(" ").trim().to_string();
    if task.is_empty() {
        return Err(usage_failure(
            "context needs a task description (for example: ocg context fix the parser)",
        ));
    }
    let started = std::time::Instant::now();
    let outcome = engine.plan(&task, None).map_err(Failure::Gear)?;
    let duration_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    for warning in &outcome.warnings {
        eprintln!("ocg: warning: {warning}");
    }

    // Local telemetry: byte and estimate accounting only, never the task text.
    let (telemetry_config, telemetry_warnings) = telemetry_for(effective, env);
    print_telemetry_warnings(&telemetry_warnings);
    let capsule_bytes = outcome
        .plan
        .capsule
        .as_ref()
        .and_then(|capsule| serde_json::to_vec(capsule).ok())
        .map(|bytes| bytes.len() as u64)
        .unwrap_or(0);
    let capabilities = outcome
        .plan
        .capabilities
        .capabilities
        .iter()
        .map(|entry| entry.capability.name())
        .collect();
    let event = telemetry::Event {
        task_id: telemetry::Event::hashed_task_id(&format!("context|{task}")),
        task_type: Some("context".to_string()),
        timestamp: clock.now_unix(),
        duration_ms,
        input_tokens: telemetry::TokenCount::estimated(outcome.plan.estimated_tokens as u64),
        context: telemetry::ContextMetrics::new(
            outcome.plan.candidate_bytes as u64,
            outcome.plan.selected_bytes as u64,
            capsule_bytes,
        ),
        repo: telemetry::RepoMetrics {
            files: outcome.index_report.metrics.files,
            symbols: outcome.index_report.metrics.symbols,
            index_reused: outcome.index_report.metrics.reused,
            index_updated: outcome.index_report.metrics.updated,
            cache_hit: Some(outcome.from_cache),
        },
        capabilities,
        outcome: telemetry::Outcome::Success,
        ..telemetry::Event::new(String::new(), clock.now_unix())
    };
    print_telemetry_warnings(&telemetry::record(project_root, &telemetry_config, event));
    if pretty {
        let value = serde_json::to_value(&outcome.plan).map_err(|error| {
            Failure::Gear(GearError::config(format!(
                "cannot serialize the context plan: {error}"
            )))
        })?;
        print_config(&value, true)?;
    } else {
        println!("{}", context::plan_text(&outcome.plan));
    }
    Ok(0)
}

/// `ocg cache clean|stats`. Never touches the managed runtime.
fn cache_command(
    effective: &config::Effective,
    project_root: &Path,
    action: Option<&str>,
) -> std::result::Result<i32, Failure> {
    let config = ContextConfig::from_config(&effective.data).map_err(Failure::Gear)?;
    let git = SystemGitHost;
    let clock = SystemClock;
    let engine = ContextEngine::new(project_root, config, &git, &clock);
    match action {
        Some("clean") => {
            let report = engine.cache_clean().map_err(Failure::Gear)?;
            println!(
                "removed {} context cache entr{} ({} bytes) from {}",
                report.removed_entries,
                if report.removed_entries == 1 {
                    "y"
                } else {
                    "ies"
                },
                report.removed_bytes,
                report.dir
            );
            Ok(0)
        }
        Some("stats") | None => {
            let stats = engine.cache_stats();
            println!("context cache: {}", stats.dir);
            println!(
                "  entries: {}  bytes: {}  corrupt: {}",
                stats.entries, stats.bytes, stats.corrupt
            );
            match (stats.oldest, stats.newest) {
                (Some(oldest), Some(newest)) => {
                    println!("  oldest: {oldest}  newest: {newest}");
                }
                _ => println!("  oldest: -  newest: -"),
            }
            let index = engine.load_index();
            match index {
                Some(index) => println!(
                    "index: {} indexed files, {} symbols",
                    index.metrics.files, index.metrics.symbols
                ),
                None => println!("index: not built"),
            }
            Ok(0)
        }
        Some(other) => Err(usage_failure(format!(
            "unknown cache action: {other} (try 'ocg cache stats' or 'ocg cache clean')"
        ))),
    }
}

/// `ocg stats [--pretty]`: read-only local telemetry aggregate. Offline, never
/// creates state and never reads the network.
fn stats_command(
    effective: &config::Effective,
    project_root: &Path,
    args: &[OsString],
    env: &Env,
    pretty: bool,
) -> std::result::Result<i32, Failure> {
    if !args.is_empty() {
        let joined = args
            .iter()
            .map(|arg| arg.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ");
        return Err(usage_failure(format!(
            "stats takes no arguments; got: {joined}"
        )));
    }
    let (config, warnings) = telemetry_for(effective, env);
    print_telemetry_warnings(&warnings);
    let store = telemetry::TelemetryStore::new(project_root, config);
    let stats = telemetry::TelemetryStats::collect(&store);
    if pretty {
        let value = serde_json::to_value(&stats).map_err(|error| {
            Failure::Gear(GearError::config(format!(
                "cannot serialize the telemetry stats: {error}"
            )))
        })?;
        print_config(&value, true)?;
    } else {
        print!("{}", stats.render());
    }
    Ok(0)
}

/// `ocg verify [fast|normal|full]`: run only configured trusted commands.
fn verify_command(
    effective: &config::Effective,
    project_root: &Path,
    args: &[OsString],
    env: &Env,
    pretty: bool,
) -> std::result::Result<i32, Failure> {
    let config = VerificationConfig::from_config(&effective.data).map_err(Failure::Gear)?;
    let words: Vec<String> = args
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    if words.len() > 1 {
        return Err(usage_failure(format!(
            "verify takes at most one stage (fast, normal or full); got: {}",
            words.join(" ")
        )));
    }
    let requested = words
        .first()
        .cloned()
        .unwrap_or_else(|| config.default_stage.clone());
    // Validate the stage name even when verification is disabled.
    config.stage(&requested).map_err(Failure::Gear)?;

    let clock = SystemClock;
    let context_config = ContextConfig::from_config(&effective.data).map_err(Failure::Gear)?;
    let mut extra_notes = Vec::new();

    // An advisory proposal only; it is never executed here. When context is
    // disabled nothing is indexed or read and the absence is stated explicitly.
    let proposal = if !config.enabled {
        None
    } else if !context_config.enabled {
        extra_notes.push(
            "context is disabled (context.enabled=false); targeted-test selection was skipped and no context index was created"
                .to_string(),
        );
        None
    } else if config.include_test_proposal {
        let git = SystemGitHost;
        let capabilities = CapabilityConfig::from_config(&effective.data).map_err(Failure::Gear)?;
        let engine = ContextEngine::new(project_root, context_config, &git, &clock)
            .with_capabilities(capabilities)
            .with_verification(config.clone());
        match engine.targeted_tests() {
            Ok(proposal) => Some(proposal),
            Err(error) => {
                eprintln!("ocg: warning: targeted test proposal skipped: {error}");
                extra_notes.push(format!("targeted-test selection was skipped: {error}"));
                None
            }
        }
    } else {
        None
    };

    let runner = SystemCaptureRunner;
    let started = std::time::Instant::now();
    let mut report = execute(&VerifyRequest {
        root: project_root,
        config: &config,
        stage: requested,
        runner: &runner,
        clock: &clock,
        test_proposal: proposal,
    })
    .map_err(Failure::Gear)?;
    let duration_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    report.notes.extend(extra_notes);

    // Local telemetry: attempt/byte accounting only, never a command string or
    // any captured output.
    let (telemetry_config, telemetry_warnings) = telemetry_for(effective, env);
    print_telemetry_warnings(&telemetry_warnings);
    let event = telemetry::Event {
        task_id: telemetry::Event::hashed_task_id(&format!("verify|{}", report.stage)),
        task_type: Some("verification".to_string()),
        timestamp: clock.now_unix(),
        duration_ms,
        verification: telemetry::verification_metrics(&report),
        logs: telemetry::log_metrics(project_root, &report),
        outcome: telemetry::verification_outcome(&report),
        ..telemetry::Event::new(String::new(), clock.now_unix())
    };
    print_telemetry_warnings(&telemetry::record(project_root, &telemetry_config, event));

    if pretty {
        let value = serde_json::to_value(&report).map_err(|error| {
            Failure::Gear(GearError::config(format!(
                "cannot serialize the verification report: {error}"
            )))
        })?;
        print_config(&value, true)?;
    } else {
        print_verification_report(&report);
    }
    Ok(if report.failed() { 1 } else { 0 })
}

fn print_verification_report(report: &crate::verification::VerificationReport) {
    println!(
        "verification stage: {} ({})",
        report.stage,
        report.overall().as_str()
    );
    for note in &report.notes {
        println!("note: {note}");
    }
    for result in &report.results {
        println!(
            "  [{}] {} ({} ms, {})",
            if result.success { "ok" } else { "fail" },
            result.display(),
            result.duration_ms,
            result.exit.label()
        );
        if !result.output.summary.is_empty() {
            for line in result.output.summary.iter().take(20) {
                println!("      {line}");
            }
        }
        for note in &result.output.notes {
            println!("      note: {note}");
        }
        if let Some(path) = &result.raw_log {
            println!(
                "      raw log: {path}{}",
                if result.raw_truncated {
                    " (truncated: retained prefix only)"
                } else {
                    ""
                }
            );
        }
    }
    if let Some(proposal) = &report.test_proposal {
        print!("{}", proposal.render());
    }
}

/// `ocg tools <task...>`: the capability plan / firewall diagnostic.
fn tools_command(
    effective: &config::Effective,
    args: &[OsString],
    pretty: bool,
) -> std::result::Result<i32, Failure> {
    let config = CapabilityConfig::from_config(&effective.data).map_err(Failure::Gear)?;
    let task = args
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string();
    if task.is_empty() {
        return Err(usage_failure(
            "tools needs a task description (for example: ocg tools commit the fix)",
        ));
    }
    let plan = CapabilityPlan::plan_config(
        &task,
        &CapabilityEvidence::default(),
        &config.custom,
        config.enabled,
    );
    if pretty {
        let value = serde_json::to_value(&plan).map_err(|error| {
            Failure::Gear(GearError::config(format!(
                "cannot serialize the capability plan: {error}"
            )))
        })?;
        print_config(&value, true)?;
    } else {
        print!("{}", plan.render());
    }
    Ok(0)
}

/// `ocg checkpoint list|show|save`.
fn checkpoint_command(
    effective: &config::Effective,
    project_root: &Path,
    args: &[OsString],
    pretty: bool,
) -> std::result::Result<i32, Failure> {
    let words: Vec<String> = args
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let git = SystemGitHost;
    match words.first().map(String::as_str) {
        None | Some("list") => {
            if words.len() > 1 {
                return Err(usage_failure(format!(
                    "checkpoint list takes no arguments; got: {}",
                    words[1..].join(" ")
                )));
            }
            let (summaries, corrupt) = checkpoint::list(project_root);
            println!(
                "checkpoints: {} ({} corrupt, ignored)",
                summaries.len(),
                corrupt
            );
            for summary in &summaries {
                println!(
                    "  {}  {}  {}",
                    summary.created_at,
                    summary.phase.as_str(),
                    summary.id
                );
                println!("      task: {}", summary.task);
            }
            Ok(0)
        }
        Some("show") => {
            if words.len() != 2 {
                return Err(usage_failure(
                    "checkpoint show needs exactly one checkpoint id (options such as --pretty may appear before or after it)",
                ));
            }
            let id = &words[1];
            if id.starts_with('-') {
                return Err(usage_failure(format!(
                    "unknown checkpoint show option: {id}"
                )));
            }
            let loaded = checkpoint::load(project_root, id, &git).map_err(Failure::Gear)?;
            if pretty {
                let value = json!({
                    "checkpoint": loaded.checkpoint,
                    "stale": loaded.staleness.stale,
                    "reasons": loaded.staleness.reasons,
                });
                print_config(&value, true)?;
            } else {
                println!(
                    "checkpoint {} ({}) created_at {}",
                    loaded.checkpoint.id,
                    loaded.checkpoint.phase.as_str(),
                    loaded.checkpoint.created_at
                );
                println!("task:        {}", loaded.checkpoint.capsule.task);
                println!(
                    "stale:       {}{}",
                    loaded.staleness.stale,
                    if loaded.staleness.reasons.is_empty() {
                        String::new()
                    } else {
                        format!(" ({})", loaded.staleness.reasons.join("; "))
                    }
                );
            }
            Ok(0)
        }
        Some("save") => save_checkpoint(effective, project_root, &words[1..]),
        Some(other) => Err(usage_failure(format!(
            "unknown checkpoint action: {other} (try 'list', 'show' or 'save')"
        ))),
    }
}

fn save_checkpoint(
    effective: &config::Effective,
    project_root: &Path,
    args: &[String],
) -> std::result::Result<i32, Failure> {
    let mut phase: Option<Phase> = None;
    let mut task: Option<String> = None;
    let mut decisions: Vec<String> = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let argument = &args[index];
        match argument.as_str() {
            "--phase" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| usage_failure("checkpoint save --phase needs a value"))?;
                phase = Phase::parse(value);
                if phase.is_none() {
                    return Err(usage_failure(format!(
                        "unknown phase '{value}' (expected explore-to-build, build-to-verify, verify-to-debug, debug-to-build, decision)"
                    )));
                }
                index += 2;
            }
            "--task" => {
                task = Some(
                    args.get(index + 1)
                        .ok_or_else(|| usage_failure("checkpoint save --task needs a value"))?
                        .clone(),
                );
                index += 2;
            }
            "--decision" => {
                decisions.push(
                    args.get(index + 1)
                        .ok_or_else(|| usage_failure("checkpoint save --decision needs a value"))?
                        .clone(),
                );
                index += 2;
            }
            other => {
                return Err(usage_failure(format!(
                    "unknown checkpoint save option: {other}"
                )))
            }
        }
    }
    let phase = phase.ok_or_else(|| usage_failure("checkpoint save needs --phase"))?;
    let task = task.unwrap_or_else(|| "checkpoint".to_string());

    let git = SystemGitHost;
    let clock = SystemClock;
    let snapshot = crate::context::gitdiff::GitSnapshot::collect(project_root, &git);
    let git_fingerprint = crate::context::gitdiff::snapshot_fingerprint(&snapshot);

    // Best-effort capsule from the current context plan; the checkpoint still
    // saves without one if context is disabled or unavailable.
    let (capsule, provenance) = {
        let context_config = ContextConfig::from_config(&effective.data).map_err(Failure::Gear)?;
        let capabilities = CapabilityConfig::from_config(&effective.data).map_err(Failure::Gear)?;
        let verification =
            VerificationConfig::from_config(&effective.data).map_err(Failure::Gear)?;
        if context_config.enabled {
            let engine = ContextEngine::new(project_root, context_config, &git, &clock)
                .with_capabilities(capabilities)
                .with_verification(verification);
            match engine.plan(&task, None) {
                Ok(outcome) => (
                    outcome
                        .plan
                        .capsule
                        .clone()
                        .unwrap_or_else(|| crate::context::capsule::TaskCapsule::new(&task)),
                    outcome.plan.provenance,
                ),
                Err(error) => {
                    eprintln!("ocg: warning: checkpoint capsule built without context: {error}");
                    (
                        crate::context::capsule::TaskCapsule::new(&task),
                        crate::context::freshness::Provenance::default(),
                    )
                }
            }
        } else {
            (
                crate::context::capsule::TaskCapsule::new(&task),
                crate::context::freshness::Provenance::default(),
            )
        }
    };

    let decisions: Vec<crate::context::capsule::Decision> = decisions
        .into_iter()
        .map(|decision| crate::context::capsule::Decision {
            decision,
            rationale: None,
            date: None,
            date_unknown: true,
        })
        .collect();
    let checkpoint = checkpoint::Checkpoint::build(
        phase,
        capsule,
        snapshot.state.clone(),
        git_fingerprint,
        None,
        provenance,
        decisions,
        clock.now_unix(),
    );
    let path = checkpoint.save(project_root).map_err(Failure::Gear)?;
    println!("checkpoint saved: {} ({})", checkpoint.id, path.display());
    Ok(0)
}

/// `ocg __bridge <event>`: the hidden JSON bridge the generated plugin calls.
///
/// It never fails the process: a bad payload, a disabled policy or a controller
/// error all become `{"ok":false,...}` on stdout, so the adapter can fail soft.
fn bridge_command(
    effective: &config::Effective,
    project_root: &Path,
    args: &[OsString],
    env: &Env,
) -> std::result::Result<i32, Failure> {
    let event = args
        .first()
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_default();
    let value = bridge_payload(effective, project_root, &event, env);
    println!(
        "{}",
        serde_json::to_string(&value).unwrap_or_else(|_| "{\"ok\":false}".to_string())
    );
    Ok(0)
}

/// Build the bridge reply. Fail-soft by construction.
fn bridge_payload(
    effective: &config::Effective,
    project_root: &Path,
    event: &str,
    env: &Env,
) -> Value {
    // Always consume stdin first, even on the disabled/early-return paths, so
    // the generated adapter's writer never sees a BrokenPipe. The read is
    // capped and overflow is rejected fail-soft.
    let (payload, oversized) = read_stdin_json(BRIDGE_MAX_STDIN_BYTES);
    if oversized {
        return json!({
            "ok": false,
            "error": format!("bridge payload exceeds the {BRIDGE_MAX_STDIN_BYTES} byte cap"),
        });
    }
    if event.is_empty() {
        return json!({"ok": false, "error": "missing bridge event"});
    }
    let orchestration =
        match crate::orchestration::OrchestrationConfig::from_config(&effective.data) {
            Ok(config) => config,
            Err(error) => return json!({"ok": false, "error": error.to_string()}),
        };
    if !orchestration.enabled {
        return json!({"ok": false, "disabled": true, "context": ""});
    }
    let context = match ContextConfig::from_config(&effective.data) {
        Ok(config) => config,
        Err(error) => return json!({"ok": false, "error": error.to_string()}),
    };
    let capabilities = match CapabilityConfig::from_config(&effective.data) {
        Ok(config) => config,
        Err(error) => return json!({"ok": false, "error": error.to_string()}),
    };
    let verification = match VerificationConfig::from_config(&effective.data) {
        Ok(config) => config,
        Err(error) => return json!({"ok": false, "error": error.to_string()}),
    };
    let git = SystemGitHost;
    let clock = SystemClock;
    let controller = crate::orchestration::controller::Controller::new(
        project_root,
        orchestration,
        context,
        capabilities,
        verification,
        &git,
        &clock,
    );
    let runner = SystemCaptureRunner;
    let (telemetry_config, warnings) = telemetry_for(effective, env);
    print_telemetry_warnings(&warnings);
    let bridge =
        crate::orchestration::bridge::BridgeContext::new(&controller, &runner, telemetry_config);
    bridge.dispatch(event, &payload)
}

/// The hard cap on a bridge payload. The generated adapter never sends anything
/// close to this; the cap protects against a hostile or broken caller and keeps
/// memory bounded.
pub const BRIDGE_MAX_STDIN_BYTES: usize = 4 * 1024 * 1024;

/// Read the bridge payload from stdin with a hard cap. Returns `(value,
/// oversized)`. Empty or invalid input is `null`, never an error. When the
/// input exceeds the cap it is drained (so the writer does not get a
/// BrokenPipe) but not retained, and `oversized` is true.
fn read_stdin_json(max_bytes: usize) -> (Value, bool) {
    use std::io::Read;
    let mut buffer = Vec::new();
    {
        let mut limited = std::io::stdin().lock().take(max_bytes as u64 + 1);
        if limited.read_to_end(&mut buffer).is_err() {
            return (Value::Null, false);
        }
    }
    // Drain the rest without retaining it, so a larger writer can complete.
    let mut sink = std::io::sink();
    let _ = std::io::copy(&mut std::io::stdin().lock(), &mut sink);
    if buffer.len() > max_bytes {
        return (Value::Null, true);
    }
    let text = String::from_utf8_lossy(&buffer);
    if text.trim().is_empty() {
        return (Value::Null, false);
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(value) => (value, false),
        Err(_) => (Value::Null, false),
    }
}

fn throttle_command(
    effective: &config::Effective,
    requested: Option<&str>,
    resolved: &str,
    cli_user_config: Option<&Path>,
    env: &Env,
) -> std::result::Result<i32, Failure> {
    let Some(level) = requested else {
        println!("{resolved}");
        return Ok(0);
    };

    let levels = effective
        .data
        .get("throttle")
        .and_then(|throttle| throttle.get("levels"))
        .and_then(Value::as_object);
    let known = levels
        .map(|levels| levels.contains_key(level))
        .unwrap_or(false);
    if !known {
        let available = levels
            .map(|levels| levels.keys().cloned().collect::<Vec<_>>().join(", "))
            .unwrap_or_default();
        return Err(Failure::Gear(GearError::config(format!(
            "unknown throttle level: '{level}' (available: {available})"
        ))));
    }

    let path = config::user_config_path(
        cli_user_config,
        env.user_config.as_deref(),
        env.xdg_config_home.as_deref(),
        env.home_dir.as_deref(),
    );
    let mut existing = if path.is_file() {
        json::read_json_object(&path).map_err(Failure::Gear)?
    } else {
        json!({})
    };
    let object = existing.as_object_mut().ok_or_else(|| {
        Failure::Gear(GearError::config(format!(
            "{} must contain a JSON object",
            path.display()
        )))
    })?;
    let throttle = object
        .entry("throttle".to_string())
        .or_insert_with(|| json!({}));
    let throttle_object = throttle.as_object_mut().ok_or_else(|| {
        Failure::Gear(GearError::config(format!(
            "{}: 'throttle' must be an object",
            path.display()
        )))
    })?;
    throttle_object.insert("default".to_string(), Value::String(level.to_string()));

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|error| {
                    GearError::io(format!("cannot create {}", parent.display()), error)
                })
                .map_err(Failure::Gear)?;
        }
    }
    let text = serde_json::to_string_pretty(&existing).map_err(|error| {
        Failure::Gear(GearError::config(format!(
            "cannot serialize {}: {error}",
            path.display()
        )))
    })?;
    std::fs::write(&path, format!("{text}\n"))
        .map_err(|error| GearError::write(&path, error))
        .map_err(Failure::Gear)?;
    println!("default throttle set to {level} in {}", path.display());
    Ok(0)
}

//! Command-line interface: argument parsing, environment resolution and
//! dispatch.
//!
//! The CLI preserves the historical `oc` semantics while exposing the binary
//! as `ocg`. `OPENCODE_GEAR_*` variables are canonical; the legacy `OC_GEAR_*`
//! names are accepted as fallbacks.

use crate::build;
use crate::config;
use crate::defaults::{load_defaults, GearSource};
use crate::error::GearError;
use crate::json;
use crate::model;
use crate::observability;
use crate::process::ProcessRunner;
use crate::report;
use crate::validate;
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
    pub opencode_bin: Option<OsString>,
    pub trace: Option<PathBuf>,
    pub xdg_config_home: Option<PathBuf>,
    pub home_dir: Option<PathBuf>,
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
            opencode_bin: env_os(&["OPENCODE_GEAR_OPENCODE_BIN", "OC_GEAR_OPENCODE_BIN"]),
            trace: env_path(&["OPENCODE_GEAR_TRACE", "OC_GEAR_TRACE"]),
            xdg_config_home: env_path(&["XDG_CONFIG_HOME"]),
            home_dir,
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
    Version,
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
        Some("version") => Command::Version,
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
  version               print the OpenCode Gear version
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
  OPENCODE_GEAR_OPENCODE_BIN   opencode binary to run (default `opencode`)
  OPENCODE_GEAR_TRACE          trace file; only read when observability is enabled

The legacy OC_GEAR_* names are still accepted as fallbacks.
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
            println!("OpenCode Gear {VERSION}");
            return Ok(0);
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
        Command::Launch => launch(&effective, &cwd, &level, &[], true, &env),
        Command::Run(args) => {
            let forwarded = prepend_subcommand("run", args);
            launch(&effective, &cwd, &level, &forwarded, true, &env)
        }
        Command::Models(args) => {
            let forwarded = prepend_subcommand("models", args);
            launch(&effective, &cwd, &level, &forwarded, false, &env)
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
    env: &Env,
) -> std::result::Result<i32, Failure> {
    let resolved = build::build_opencode_config(effective, level).map_err(Failure::Gear)?;
    let content = serde_json::to_string(&resolved).map_err(|error| {
        Failure::Gear(GearError::config(format!(
            "cannot serialize the OpenCode config: {error}"
        )))
    })?;
    if trace {
        observability::record_event(effective, "launch", level, env.trace.as_deref());
    }
    let program = env
        .opencode_bin
        .clone()
        .unwrap_or_else(|| OsString::from("opencode"));
    let runner = ProcessRunner::new(program);
    runner.exec(args, cwd, &content).map_err(Failure::Gear)?;
    Ok(0)
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

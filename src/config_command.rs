//! `ocg config`: guided, safe configuration for the Lead and providers.
//!
//! Two user-facing jobs, deliberately small:
//!
//! - switch which provider/model (and optional reasoning variant) a throttle
//!   level uses, creating a deterministic registry entry when the requested
//!   `provider/model` has none;
//! - register a custom OpenAI-compatible provider whose API key is referenced
//!   as `{env:VAR}` and never stored or printed.
//!
//! Safety rules, in order:
//!
//! 1. the target layer is read, mutated in memory and re-serialized; unrelated
//!    keys are preserved (semantic preservation, not comment preservation);
//! 2. the *candidate* configuration (defaults + both layers, with the edited
//!    layer replaced by the candidate) must pass OCG's own static validation;
//! 3. when the runtime model catalogue can be probed and proves the selected
//!    Lead model is absent, the change is rejected; when probing is
//!    unavailable, the change is written but the report says so plainly;
//! 4. only then is the file replaced, atomically, via a temp file + rename in
//!    the same directory.
//!
//! Nothing here re-implements Gear's rules: the candidate goes through
//! [`crate::validate`] and the runtime check through [`crate::preflight`].

use crate::cli::Env;
use crate::config::{self, Effective};
use crate::defaults::EXECUTION_TIERS;
use crate::error::{GearError, Result};
use crate::model;
use crate::preflight::{Availability, ModelPreflight};
use crate::validate;
use crate::yaml;
use serde_json::{json, Map, Value};
use std::ffi::OsString;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

/// Write a line to the command output, converting the IO error into a
/// [`GearError`] (there is deliberately no blanket `From<io::Error>` in Gear).
macro_rules! emit {
    ($output:expr, $($arg:tt)*) => {
        writeln!($output, $($arg)*).map_err(|error| GearError::io("cannot write output", error))
    };
}

/// Write without a trailing newline (prompts).
macro_rules! emit_raw {
    ($output:expr, $($arg:tt)*) => {
        write!($output, $($arg)*).map_err(|error| GearError::io("cannot write output", error))
    };
}

/// The npm package OpenCode loads for a custom OpenAI-compatible provider.
pub const OPENAI_COMPATIBLE_NPM: &str = "@ai-sdk/openai-compatible";

/// Which configuration layer a write targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    User,
    Project,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::User => "user",
            Scope::Project => "project",
        }
    }

    fn parse(raw: &str) -> Option<Scope> {
        match raw {
            "user" => Some(Scope::User),
            "project" => Some(Scope::Project),
            _ => None,
        }
    }
}

/// One parsed `ocg config` invocation.
#[derive(Debug)]
pub enum Request {
    /// No subcommand: the numbered menu.
    Interactive,
    /// `ocg config lead` without a level: report the effective Lead table.
    ShowLead,
    /// `ocg config lead <low|mid|high> ...`
    Lead {
        level: String,
        scope: Scope,
        model: Option<String>,
        variant: Option<String>,
        yes: bool,
    },
    /// `ocg config provider add-openai-compatible <name> ...`
    ProviderAdd {
        name: String,
        scope: Scope,
        base_url: String,
        api_key_env: String,
        models: Vec<String>,
        yes: bool,
    },
}

/// Everything the command needs from the CLI (already resolved once there).
pub struct Context<'a> {
    pub defaults: Value,
    pub gear_home: Option<PathBuf>,
    pub project_root: PathBuf,
    pub invocation_dir: PathBuf,
    pub user_path: PathBuf,
    pub project_path: PathBuf,
    pub env: &'a Env,
    pub level: String,
    /// The effective configuration as it exists on disk right now.
    pub current: Effective,
}

/// The runtime availability probe, injected by the CLI.
///
/// It builds the candidate OpenCode config for `level`, runs the supported
/// `opencode models` catalogue probe and returns the reduced result. `None`
/// means no probe could be attempted at all (for example no runtime that can
/// be resolved); the caller then reports the change as runtime-unverified.
pub type ProbeFn<'a> = dyn Fn(&Effective, &str) -> Option<ModelPreflight> + 'a;

/// The runtime verdict for the report.
enum RuntimeVerdict {
    Verified,
    Unavailable(String),
    Rejected,
}

/// Parse the arguments after `config`.
pub fn parse_request(args: &[OsString]) -> Result<Request> {
    let words: Vec<String> = args
        .iter()
        .map(|value| value.to_string_lossy().into_owned())
        .collect();
    let Some(first) = words.first() else {
        return Ok(Request::Interactive);
    };
    match first.as_str() {
        "lead" => parse_lead(&words[1..]),
        "provider" => parse_provider(&words[1..]),
        other => Err(usage(format!(
            "unknown config subcommand: '{other}' (use 'lead' or 'provider add-openai-compatible')"
        ))),
    }
}

/// Execute one parsed request.
pub fn execute(
    request: &Request,
    ctx: &Context,
    probe: &ProbeFn,
    input: &mut dyn BufRead,
    output: &mut dyn Write,
) -> Result<i32> {
    match request {
        Request::Interactive => interactive(ctx, probe, input, output),
        Request::ShowLead => {
            print_lead_table(ctx, output)?;
            Ok(0)
        }
        Request::Lead {
            level,
            scope,
            model,
            variant,
            yes,
        } => {
            if !*yes {
                return Err(usage(
                    "refusing to write without --yes (non-interactive): pass --yes to confirm",
                ));
            }
            apply_lead(
                ctx,
                probe,
                level,
                *scope,
                model.as_deref(),
                variant.as_deref(),
                output,
            )
        }
        Request::ProviderAdd {
            name,
            scope,
            base_url,
            api_key_env,
            models,
            yes,
        } => {
            if !*yes {
                return Err(usage(
                    "refusing to write without --yes (non-interactive): pass --yes to confirm",
                ));
            }
            apply_provider(ctx, name, *scope, base_url, api_key_env, models, output)
        }
    }
}

// ---------------------------------------------------------------------------
// Argument parsing
// ---------------------------------------------------------------------------

fn parse_lead(words: &[String]) -> Result<Request> {
    let mut level: Option<String> = None;
    let mut scope = Scope::User;
    let mut model: Option<String> = None;
    let mut variant: Option<String> = None;
    let mut yes = false;
    let mut index = 0;
    while index < words.len() {
        let word = words[index].as_str();
        match word {
            "--yes" => {
                yes = true;
                index += 1;
            }
            "--scope" => {
                let value = words
                    .get(index + 1)
                    .ok_or_else(|| usage("--scope needs a value"))?;
                scope = Scope::parse(value)
                    .ok_or_else(|| usage("--scope must be 'user' or 'project'"))?;
                index += 2;
            }
            "--model" => {
                let value = words
                    .get(index + 1)
                    .ok_or_else(|| usage("--model needs a value"))?;
                model = Some(value.clone());
                index += 2;
            }
            "--variant" => {
                let value = words
                    .get(index + 1)
                    .ok_or_else(|| usage("--variant needs a value"))?;
                variant = Some(value.clone());
                index += 2;
            }
            _ => {
                if let Some(value) = word.strip_prefix("--scope=") {
                    scope = Scope::parse(value)
                        .ok_or_else(|| usage("--scope must be 'user' or 'project'"))?;
                    index += 1;
                } else if let Some(value) = word.strip_prefix("--model=") {
                    model = Some(value.to_string());
                    index += 1;
                } else if let Some(value) = word.strip_prefix("--variant=") {
                    variant = Some(value.to_string());
                    index += 1;
                } else if word.starts_with('-') {
                    return Err(usage(format!("unknown config lead option: {word}")));
                } else if level.is_some() {
                    return Err(usage(format!("unexpected argument: {word}")));
                } else {
                    level = Some(word.to_string());
                    index += 1;
                }
            }
        }
    }
    let Some(level) = level else {
        return Ok(Request::ShowLead);
    };
    if model.is_none() && variant.is_none() {
        return Err(usage(
            "a Lead change needs --model (provider/model or an existing registry key) and/or --variant",
        ));
    }
    Ok(Request::Lead {
        level,
        scope,
        model,
        variant,
        yes,
    })
}

fn parse_provider(words: &[String]) -> Result<Request> {
    let Some(action) = words.first() else {
        return Err(usage(
            "usage: ocg config provider add-openai-compatible <name> --base-url URL --api-key-env VAR --model ID [--model ID...]",
        ));
    };
    if action != "add-openai-compatible" {
        return Err(usage(format!(
            "unknown provider action: '{action}' (only 'add-openai-compatible' exists)"
        )));
    }
    let mut name: Option<String> = None;
    let mut scope = Scope::User;
    let mut base_url: Option<String> = None;
    let mut api_key_env: Option<String> = None;
    let mut models: Vec<String> = Vec::new();
    let mut yes = false;
    let mut index = 1;
    while index < words.len() {
        let word = words[index].as_str();
        match word {
            "--yes" => {
                yes = true;
                index += 1;
            }
            "--scope" => {
                let value = words
                    .get(index + 1)
                    .ok_or_else(|| usage("--scope needs a value"))?;
                scope = Scope::parse(value)
                    .ok_or_else(|| usage("--scope must be 'user' or 'project'"))?;
                index += 2;
            }
            "--base-url" => {
                base_url = Some(
                    words
                        .get(index + 1)
                        .ok_or_else(|| usage("--base-url needs a value"))?
                        .clone(),
                );
                index += 2;
            }
            "--api-key-env" => {
                api_key_env = Some(
                    words
                        .get(index + 1)
                        .ok_or_else(|| usage("--api-key-env needs a value"))?
                        .clone(),
                );
                index += 2;
            }
            "--model" => {
                models.push(
                    words
                        .get(index + 1)
                        .ok_or_else(|| usage("--model needs a value"))?
                        .clone(),
                );
                index += 2;
            }
            _ => {
                if let Some(value) = word.strip_prefix("--scope=") {
                    scope = Scope::parse(value)
                        .ok_or_else(|| usage("--scope must be 'user' or 'project'"))?;
                    index += 1;
                } else if let Some(value) = word.strip_prefix("--base-url=") {
                    base_url = Some(value.to_string());
                    index += 1;
                } else if let Some(value) = word.strip_prefix("--api-key-env=") {
                    api_key_env = Some(value.to_string());
                    index += 1;
                } else if let Some(value) = word.strip_prefix("--model=") {
                    models.push(value.to_string());
                    index += 1;
                } else if word.starts_with('-') {
                    return Err(usage(format!("unknown config provider option: {word}")));
                } else if name.is_some() {
                    return Err(usage(format!("unexpected argument: {word}")));
                } else {
                    name = Some(word.to_string());
                    index += 1;
                }
            }
        }
    }
    let name = name.ok_or_else(|| usage("add-openai-compatible needs a provider name"))?;
    let base_url = base_url.ok_or_else(|| usage("add-openai-compatible needs --base-url"))?;
    let api_key_env =
        api_key_env.ok_or_else(|| usage("add-openai-compatible needs --api-key-env"))?;
    if models.is_empty() {
        return Err(usage("add-openai-compatible needs at least one --model"));
    }
    Ok(Request::ProviderAdd {
        name,
        scope,
        base_url,
        api_key_env,
        models,
        yes,
    })
}

// ---------------------------------------------------------------------------
// Lead changes
// ---------------------------------------------------------------------------

/// One resolved `provider/model` plus the registry work it implies.
struct ModelRef {
    key: String,
    provider: String,
    id: String,
    created_model: bool,
    created_provider: bool,
}

fn apply_lead(
    ctx: &Context,
    probe: &ProbeFn,
    level: &str,
    scope: Scope,
    model_arg: Option<&str>,
    variant: Option<&str>,
    output: &mut dyn Write,
) -> Result<i32> {
    if !ctx
        .current
        .data
        .get("throttle")
        .and_then(|throttle| throttle.get("levels"))
        .and_then(|levels| levels.get(level))
        .is_some()
    {
        let available = ctx
            .current
            .data
            .get("throttle")
            .and_then(|throttle| throttle.get("levels"))
            .and_then(Value::as_object)
            .map(|levels| levels.keys().cloned().collect::<Vec<_>>().join(", "))
            .unwrap_or_default();
        return Err(usage(format!(
            "unknown throttle level: '{level}' (available: {available})"
        )));
    }
    let path = layer_path(ctx, scope);
    guard_writable(&path, scope)?;
    let mut layer = read_layer(&path)?;

    let created = match model_arg {
        Some(raw) => {
            let resolved = resolve_model_ref(&ctx.current.data, raw)?;
            if resolved.created_provider {
                declare_provider(&mut layer, &resolved.provider, None, None)?;
            }
            if resolved.created_model {
                insert_model_entry(&mut layer, &resolved.key, &resolved.provider, &resolved.id)?;
            }
            set_model(&mut layer, level, &resolved.key)?;
            Some(resolved)
        }
        None => None,
    };
    if let Some(variant) = variant {
        set_variant(&mut layer, level, Some(variant))?;
    }

    let candidate = candidate_effective(ctx, scope, &layer)?;
    let errors = validate::validate(&candidate);
    if !errors.is_empty() {
        emit!(
            output,
            "ocg config: the change was rejected; {} is unchanged",
            path.display()
        )?;
        for error in &errors {
            emit!(output, "  - {error}")?;
        }
        return Ok(1);
    }

    let runtime = probe_runtime(&candidate, probe, level, output)?;
    if matches!(runtime, RuntimeVerdict::Rejected) {
        return Ok(1);
    }

    write_layer(&path, &layer)?;
    report_lead(
        ctx,
        scope,
        level,
        &path,
        &candidate,
        created.as_ref(),
        runtime,
        output,
    )?;
    Ok(0)
}

fn resolve_model_ref(current: &Value, raw: &str) -> Result<ModelRef> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(usage("--model must not be empty"));
    }
    let registry = model::model_registry(current);
    if !raw.contains('/') {
        let entry = registry
            .and_then(|registry| registry.get(raw))
            .and_then(Value::as_object)
            .ok_or_else(|| {
                usage(format!(
                    "unknown model key '{raw}': use an existing models key or 'provider/model'"
                ))
            })?;
        return Ok(ModelRef {
            key: raw.to_string(),
            provider: string_field(entry, "provider", raw)?,
            id: string_field(entry, "id", raw)?,
            created_model: false,
            created_provider: false,
        });
    }
    let (provider, id) = raw.split_once('/').expect("contains '/'");
    if provider.is_empty() || id.is_empty() || id.contains('/') {
        return Err(usage(format!(
            "--model '{raw}' must be one 'provider/model' with both parts non-empty"
        )));
    }
    validate_model_id(id)?;
    let existing = registry.and_then(|registry| {
        registry
            .iter()
            .find(|(_, entry)| {
                entry.get("provider").and_then(Value::as_str) == Some(provider)
                    && entry.get("id").and_then(Value::as_str) == Some(id)
            })
            .map(|(key, _)| key.clone())
    });
    if let Some(key) = existing {
        return Ok(ModelRef {
            key,
            provider: provider.to_string(),
            id: id.to_string(),
            created_model: false,
            created_provider: false,
        });
    }
    let key = format!("{}-{}", slug(provider), slug(id));
    if registry
        .map(|registry| registry.contains_key(&key))
        .unwrap_or(false)
    {
        return Err(usage(format!(
            "the model key '{key}' already exists for a different provider/model; choose another name"
        )));
    }
    let declared = model::providers(current)
        .map(|providers| providers.contains_key(provider))
        .unwrap_or(false);
    Ok(ModelRef {
        key,
        provider: provider.to_string(),
        id: id.to_string(),
        created_model: true,
        created_provider: !declared,
    })
}

fn set_model(layer: &mut Value, level: &str, key: &str) -> Result<()> {
    let spec = throttle_spec(layer, level)?;
    spec.insert("model".to_string(), json!(key));
    // A variant belongs to a model. Removing it here lets the layer normalizer
    // drop an inherited variant exactly when the model actually changes; the
    // route then runs at the provider default instead of a stale variant.
    spec.remove("variant");
    Ok(())
}

fn set_variant(layer: &mut Value, level: &str, variant: Option<&str>) -> Result<()> {
    let spec = throttle_spec(layer, level)?;
    match variant {
        Some(value) => {
            spec.insert("variant".to_string(), json!(value));
        }
        None => {
            spec.remove("variant");
        }
    }
    Ok(())
}

fn throttle_spec<'a>(layer: &'a mut Value, level: &str) -> Result<&'a mut Map<String, Value>> {
    let throttle = object_at_mut(layer, "throttle")?;
    let levels = object_at_mut_map(throttle, "levels")?;
    if !levels.contains_key(level) {
        levels.insert(level.to_string(), Value::Object(Map::new()));
    }
    levels
        .get_mut(level)
        .and_then(Value::as_object_mut)
        .ok_or_else(|| GearError::config(format!("throttle level '{level}' must be an object")))
}

fn insert_model_entry(layer: &mut Value, key: &str, provider: &str, id: &str) -> Result<()> {
    let models = object_at_mut(layer, "models")?;
    let registry = object_at_mut_map(models, "models")?;
    registry.insert(
        key.to_string(),
        json!({"provider": provider, "id": id, "label": title(id)}),
    );
    Ok(())
}

/// Declare a provider in `models.providers`, and optionally the OpenCode
/// provider block (used by the OpenAI-compatible registration).
fn declare_provider(
    layer: &mut Value,
    name: &str,
    opencode_block: Option<Value>,
    note: Option<&str>,
) -> Result<()> {
    let models = object_at_mut(layer, "models")?;
    let providers = object_at_mut_map(models, "providers")?;
    providers.entry(name.to_string()).or_insert_with(|| {
        json!({
            "label": title(name),
            "note": note.unwrap_or("registered by `ocg config`"),
        })
    });
    if let Some(block) = opencode_block {
        let opencode = object_at_mut(layer, "opencode")?;
        let map = object_at_mut_map(opencode, "provider")?;
        map.insert(name.to_string(), block);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Provider registration
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn apply_provider(
    ctx: &Context,
    name: &str,
    scope: Scope,
    base_url: &str,
    api_key_env: &str,
    models: &[String],
    output: &mut dyn Write,
) -> Result<i32> {
    validate_provider_name(name)?;
    validate_base_url(base_url)?;
    validate_env_reference(api_key_env)?;
    let mut ids: Vec<String> = Vec::new();
    for model in models {
        validate_model_id(model)?;
        if !ids.iter().any(|existing| existing == model) {
            ids.push(model.clone());
        }
    }
    if models_already_declared(&ctx.current.data, name) {
        return Err(GearError::config(format!(
            "provider '{name}' is already declared; edit its existing definition instead of adding it again"
        )));
    }

    let path = layer_path(ctx, scope);
    guard_writable(&path, scope)?;
    let mut layer = read_layer(&path)?;

    let mut model_map = Map::new();
    for id in &ids {
        model_map.insert(id.clone(), json!({"name": title(id)}));
    }
    let block = json!({
        "npm": OPENAI_COMPATIBLE_NPM,
        "name": title(name),
        "options": {
            "baseURL": base_url,
            "apiKey": format!("{{env:{api_key_env}}}"),
        },
        "models": Value::Object(model_map),
    });
    declare_provider(
        &mut layer,
        name,
        Some(block),
        Some("OpenAI-compatible endpoint registered by `ocg config`"),
    )?;

    // Deterministic registry entries, reusing an entry that already names this
    // exact provider/model.
    for id in &ids {
        let key = format!("{}-{}", slug(name), slug(id));
        let existing = model::model_registry(&ctx.current.data)
            .and_then(|registry| registry.get(&key))
            .and_then(Value::as_object);
        if existing.is_some_and(|entry| {
            entry.get("provider").and_then(Value::as_str) == Some(name)
                && entry.get("id").and_then(Value::as_str) == Some(id.as_str())
        }) {
            continue;
        }
        if existing.is_some() {
            return Err(usage(format!(
                "the model key '{key}' already exists for a different provider/model; choose another name"
            )));
        }
        insert_model_entry(&mut layer, &key, name, id)?;
    }

    let candidate = candidate_effective(ctx, scope, &layer)?;
    let errors = validate::validate(&candidate);
    if !errors.is_empty() {
        emit!(
            output,
            "ocg config: the provider was rejected; {} is unchanged",
            path.display()
        )?;
        for error in &errors {
            emit!(output, "  - {error}")?;
        }
        return Ok(1);
    }

    write_layer(&path, &layer)?;
    emit!(output, "ocg: OpenAI-compatible provider '{name}' added")?;
    emit!(output, "  scope:   {}", scope.as_str())?;
    emit!(output, "  config:  {}", path.display())?;
    emit!(output, "  package: {OPENAI_COMPATIBLE_NPM}")?;
    emit!(output, "  baseURL: {base_url}")?;
    emit!(
        output,
        "  api key: {{env:{api_key_env}}} (read from the environment at launch; never stored)"
    )?;
    emit!(output, "  models:  {}", ids.join(", "))?;
    emit!(
        output,
        "  note:    activate it with `ocg config lead <level> --model {name}/{} --yes`",
        ids.first().map(String::as_str).unwrap_or("MODEL")
    )?;
    Ok(0)
}

fn models_already_declared(data: &Value, name: &str) -> bool {
    model::providers(data)
        .map(|providers| providers.contains_key(name))
        .unwrap_or(false)
        || data
            .get("opencode")
            .and_then(|opencode| opencode.get("provider"))
            .and_then(|providers| providers.get(name))
            .is_some()
}

fn validate_provider_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 64 {
        return Err(usage("the provider name must be 1..=64 characters"));
    }
    if name.starts_with('-') || name.contains('/') || name.contains(char::is_whitespace) {
        return Err(usage(
            "the provider name cannot start with '-' or contain '/' or whitespace",
        ));
    }
    if !name
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_'))
    {
        return Err(usage(
            "the provider name may only contain ASCII letters, digits, '.', '-' and '_'",
        ));
    }
    Ok(())
}

fn validate_base_url(url: &str) -> Result<()> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"));
    let Some(rest) = rest else {
        return Err(usage(
            "--base-url must be an absolute http:// or https:// URL",
        ));
    };
    let host = rest.split('/').next().unwrap_or("");
    if host.is_empty() || host.contains(char::is_whitespace) {
        return Err(usage("--base-url must include a host"));
    }
    Ok(())
}

/// The API key is only ever referenced, never accepted or stored.
fn validate_env_reference(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 128 {
        return Err(usage(
            "--api-key-env must be an environment variable name (for example ACME_API_KEY); the key itself is never accepted or stored",
        ));
    }
    let mut characters = name.chars();
    let first = characters.next().expect("non-empty");
    let valid = (first.is_ascii_alphabetic() || first == '_')
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_');
    if !valid {
        return Err(usage(format!(
            "--api-key-env '{name}' is not an environment variable name; pass the variable name (for example ACME_API_KEY), never the key itself"
        )));
    }
    Ok(())
}

fn validate_model_id(id: &str) -> Result<()> {
    if id.is_empty() || id.len() > 200 {
        return Err(usage("a model id must be 1..=200 characters"));
    }
    if id.contains('/') || id.contains(char::is_whitespace) {
        return Err(usage(format!(
            "model id '{id}' cannot contain '/' or whitespace"
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Candidate building, atomic writes and reporting
// ---------------------------------------------------------------------------

fn layer_path(ctx: &Context, scope: Scope) -> PathBuf {
    match scope {
        Scope::User => ctx.user_path.clone(),
        Scope::Project => ctx.project_path.clone(),
    }
}

fn guard_writable(path: &Path, scope: Scope) -> Result<()> {
    config::reject_stale_json(path, scope.as_str())?;
    if path
        .extension()
        .map(|extension| extension.eq_ignore_ascii_case("json"))
        .unwrap_or(false)
    {
        return Err(GearError::config(format!(
            "refusing to write the {} config as JSON: {}\nOpenCode Gear reads and writes YAML only; pass a .yaml path.",
            scope.as_str(),
            path.display()
        )));
    }
    Ok(())
}

fn read_layer(path: &Path) -> Result<Value> {
    if path.is_file() {
        yaml::read_yaml_object(path)
    } else {
        Ok(json!({}))
    }
}

fn read_optional_layer(path: &Path) -> Result<Option<Value>> {
    if path.is_file() {
        Ok(Some(read_layer(path)?))
    } else {
        Ok(None)
    }
}

/// Replace the target layer with the candidate, atomically.
fn write_layer(path: &Path, layer: &Value) -> Result<()> {
    let text = yaml::to_yaml_string(layer).map_err(|error| {
        GearError::config(format!("cannot serialize {}: {error}", path.display()))
    })?;
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };
    std::fs::create_dir_all(&parent)
        .map_err(|error| GearError::io(format!("cannot create {}", parent.display()), error))?;
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "config.yaml".to_string());
    let temp = parent.join(format!(".{name}.tmp-{}", std::process::id()));
    std::fs::write(&temp, text).map_err(|error| GearError::write(&temp, error))?;
    std::fs::rename(&temp, path).map_err(|error| {
        let _ = std::fs::remove_file(&temp);
        GearError::write(path, error)
    })
}

/// Build the configuration that the write *would* produce.
fn candidate_effective(ctx: &Context, scope: Scope, layer: &Value) -> Result<Effective> {
    let (user_overlay, project_overlay) = match scope {
        Scope::User => (Some(layer.clone()), read_optional_layer(&ctx.project_path)?),
        Scope::Project => (read_optional_layer(&ctx.user_path)?, Some(layer.clone())),
    };
    config::build_effective_with_overlays(
        ctx.defaults.clone(),
        ctx.gear_home.clone(),
        &ctx.project_root,
        user_overlay.as_ref(),
        &ctx.user_path,
        project_overlay.as_ref(),
        &ctx.project_path,
        ctx.env.home_dir.clone(),
    )
}

fn probe_runtime(
    candidate: &Effective,
    probe: &ProbeFn,
    level: &str,
    output: &mut dyn Write,
) -> Result<RuntimeVerdict> {
    let Some(result) = probe(candidate, level) else {
        return Ok(RuntimeVerdict::Unavailable(
            "no OpenCode runtime could be resolved for a catalogue probe".to_string(),
        ));
    };
    match result {
        ModelPreflight::Unavailable { reason } => Ok(RuntimeVerdict::Unavailable(reason)),
        ModelPreflight::Complete { checks } => {
            let contract = model::lead_contract(&candidate.data, level)?;
            let full = contract.full_model_id();
            let check = checks.iter().find(|check| {
                check.requirement.agent == contract.agent && check.requirement.full_model_id == full
            });
            match check.map(|check| check.availability) {
                Some(Availability::Available) | None => Ok(RuntimeVerdict::Verified),
                Some(Availability::MissingProvider | Availability::MissingModel) => {
                    emit!(
                        output,
                        "ocg config: rejected — the runtime catalogue does not expose {full}; the file is unchanged"
                    )?;
                    emit!(
                        output,
                        "  authenticate/configure the provider first (for example `opencode auth login`), or choose another model"
                    )?;
                    Ok(RuntimeVerdict::Rejected)
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn report_lead(
    ctx: &Context,
    scope: Scope,
    level: &str,
    path: &Path,
    candidate: &Effective,
    created: Option<&ModelRef>,
    runtime: RuntimeVerdict,
    output: &mut dyn Write,
) -> Result<()> {
    let contract = model::lead_contract(&candidate.data, level)?;
    emit!(output, "ocg: Lead for throttle '{level}' updated")?;
    emit!(output, "  scope:    {}", scope.as_str())?;
    emit!(output, "  model:    {}", contract.full_model_id())?;
    emit!(
        output,
        "  variant:  {}",
        contract.variant.as_deref().unwrap_or("provider default")
    )?;
    emit!(output, "  config:   {}", path.display())?;
    if let Some(created) = created {
        if created.created_model {
            emit!(
                output,
                "  registry: added model '{}' ({}/{})",
                created.key,
                created.provider,
                created.id
            )?;
        }
        if created.created_provider {
            emit!(
                output,
                "  registry: declared provider '{}' in models.providers",
                created.provider
            )?;
        }
    }
    match runtime {
        RuntimeVerdict::Verified => emit!(
            output,
            "  runtime:  verified — the OpenCode catalogue currently exposes {}",
            contract.full_model_id()
        )?,
        RuntimeVerdict::Unavailable(reason) => emit!(
            output,
            "  runtime:  NOT verified — {reason}; the change is written but was not checked against the runtime"
        )?,
        RuntimeVerdict::Rejected => {}
    }
    // A higher-precedence layer wins silently otherwise: say so explicitly.
    if scope == Scope::User && layer_sets_lead(&ctx.project_path, level) {
        emit!(
            output,
            "  note:     the project config {} also sets throttle '{level}'; the project value wins (effective: {})",
            ctx.project_path.display(),
            contract.full_model_id()
        )?;
    }
    Ok(())
}

fn layer_sets_lead(path: &Path, level: &str) -> bool {
    if !path.is_file() {
        return false;
    }
    yaml::read_yaml_object(path)
        .ok()
        .and_then(|value| {
            value
                .get("throttle")
                .and_then(|throttle| throttle.get("levels"))
                .and_then(|levels| levels.get(level))
                .and_then(|spec| spec.get("model"))
                .map(|model| !model.is_null())
        })
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Reporting and the interactive menu
// ---------------------------------------------------------------------------

fn print_lead_table(ctx: &Context, output: &mut dyn Write) -> Result<()> {
    emit!(output, "OpenCode Gear Lead configuration")?;
    emit!(output, "  project: {}", ctx.project_root.display())?;
    emit!(output, "  user:    {}", ctx.user_path.display())?;
    emit!(output, "  project config: {}", ctx.project_path.display())?;
    emit!(output, "")?;
    emit!(
        output,
        "  {:<6} {:<32} {:<18} source",
        "level",
        "provider/model",
        "variant"
    )?;
    let levels = ctx
        .current
        .data
        .get("throttle")
        .and_then(|throttle| throttle.get("levels"))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut names: Vec<String> = EXECUTION_TIERS
        .iter()
        .map(|level| level.to_string())
        .filter(|level| levels.contains_key(level))
        .collect();
    for name in levels.keys() {
        if !names.contains(name) {
            names.push(name.clone());
        }
    }
    for level in names {
        let contract = model::lead_contract(&ctx.current.data, &level)?;
        let source = if layer_sets_lead(&ctx.project_path, &level) {
            "project"
        } else if layer_sets_lead(&ctx.user_path, &level) {
            "user"
        } else {
            "defaults"
        };
        let active = if level == ctx.level { " (active)" } else { "" };
        emit!(
            output,
            "  {:<6} {:<32} {:<18} {}{}",
            level,
            contract.full_model_id(),
            contract.variant.as_deref().unwrap_or("provider default"),
            source,
            active
        )?;
    }
    emit!(output, "")?;
    emit!(
        output,
        "  change:  ocg config lead <level> --model provider/model [--variant V] [--scope user|project] --yes"
    )?;
    emit!(output, "  provider: ocg config provider add-openai-compatible <name> --base-url URL --api-key-env VAR --model ID --yes")?;
    Ok(())
}

fn interactive(
    ctx: &Context,
    probe: &ProbeFn,
    input: &mut dyn BufRead,
    output: &mut dyn Write,
) -> Result<i32> {
    loop {
        emit!(output, "OpenCode Gear configuration")?;
        emit!(output, "")?;
        emit!(output, "  1) show the effective Lead configuration")?;
        emit!(output, "  2) change the Lead model for a throttle level")?;
        emit!(output, "  3) add an OpenAI-compatible provider")?;
        emit!(output, "  0) exit")?;
        let Some(choice) = ask(input, output, "choice: ")? else {
            return Ok(0);
        };
        emit!(output, "")?;
        match choice.as_str() {
            "0" | "q" | "quit" | "exit" => return Ok(0),
            "1" => print_lead_table(ctx, output)?,
            "2" => {
                if let Some(code) = interactive_lead(ctx, probe, input, output)? {
                    if code != 0 {
                        return Ok(code);
                    }
                }
            }
            "3" => {
                if let Some(code) = interactive_provider(ctx, input, output)? {
                    if code != 0 {
                        return Ok(code);
                    }
                }
            }
            other => emit!(output, "unknown choice: {other}")?,
        }
        emit!(output, "")?;
    }
}

fn interactive_lead(
    ctx: &Context,
    probe: &ProbeFn,
    input: &mut dyn BufRead,
    output: &mut dyn Write,
) -> Result<Option<i32>> {
    let Some(level) = ask(input, output, "throttle level [low|mid|high]: ")? else {
        return Ok(None);
    };
    if !EXECUTION_TIERS.contains(&level.as_str()) {
        emit!(output, "unknown throttle level: {level}")?;
        return Ok(None);
    }
    let current = model::lead_contract(&ctx.current.data, &level)?;
    let model = ask(
        input,
        output,
        &format!(
            "model (registry key or provider/model; blank keeps {}): ",
            current.full_model_id()
        ),
    )?;
    let model = model.filter(|value| !value.is_empty());
    let variant_prompt = if model.is_some() {
        "variant (blank = provider default): "
    } else {
        "variant (blank keeps the current value): "
    };
    let variant = ask(input, output, variant_prompt)?;
    let variant = variant.filter(|value| !value.is_empty());
    if model.is_none() && variant.is_none() {
        emit!(output, "nothing to change")?;
        return Ok(None);
    }
    let scope = ask(input, output, "scope [user|project] (default user): ")?;
    let scope = match scope.as_deref().unwrap_or("user") {
        "" | "user" => Scope::User,
        "project" => Scope::Project,
        other => {
            emit!(output, "unknown scope: {other}")?;
            return Ok(None);
        }
    };
    let summary = format!(
        "level={level} model={} variant={} scope={}",
        model.as_deref().unwrap_or(&current.full_model_id()),
        variant.as_deref().unwrap_or("provider default"),
        scope.as_str()
    );
    if !confirm(input, output, &summary)? {
        emit!(output, "cancelled")?;
        return Ok(None);
    }
    let code = apply_lead(
        ctx,
        probe,
        &level,
        scope,
        model.as_deref(),
        variant.as_deref(),
        output,
    )?;
    Ok(Some(code))
}

fn interactive_provider(
    ctx: &Context,
    input: &mut dyn BufRead,
    output: &mut dyn Write,
) -> Result<Option<i32>> {
    let Some(name) = ask(input, output, "provider name: ")? else {
        return Ok(None);
    };
    let Some(base_url) = ask(input, output, "base URL (https://.../v1): ")? else {
        return Ok(None);
    };
    let Some(api_key_env) = ask(input, output, "API key environment variable name: ")? else {
        return Ok(None);
    };
    let Some(models) = ask(input, output, "model ids (comma separated): ")? else {
        return Ok(None);
    };
    let models: Vec<String> = models
        .split(',')
        .map(|model| model.trim().to_string())
        .filter(|model| !model.is_empty())
        .collect();
    let scope = ask(input, output, "scope [user|project] (default user): ")?;
    let scope = match scope.as_deref().unwrap_or("user") {
        "" | "user" => Scope::User,
        "project" => Scope::Project,
        other => {
            emit!(output, "unknown scope: {other}")?;
            return Ok(None);
        }
    };
    let summary = format!(
        "provider={name} baseURL={base_url} apiKey={{env:{api_key_env}}} models={} scope={}",
        models.join(", "),
        scope.as_str()
    );
    if !confirm(input, output, &summary)? {
        emit!(output, "cancelled")?;
        return Ok(None);
    }
    let code = apply_provider(ctx, &name, scope, &base_url, &api_key_env, &models, output)?;
    Ok(Some(code))
}

fn ask(input: &mut dyn BufRead, output: &mut dyn Write, prompt: &str) -> Result<Option<String>> {
    emit_raw!(output, "{prompt}")?;
    output
        .flush()
        .map_err(|error| GearError::io("cannot write output", error))?;
    let mut line = String::new();
    if input
        .read_line(&mut line)
        .map_err(|error| GearError::io("cannot read the interactive answer", error))?
        == 0
    {
        return Ok(None);
    }
    Ok(Some(line.trim().to_string()))
}

fn confirm(input: &mut dyn BufRead, output: &mut dyn Write, summary: &str) -> Result<bool> {
    emit!(output, "{summary}")?;
    let answer = ask(input, output, "write this change? [y/N]: ")?;
    Ok(matches!(
        answer.as_deref(),
        Some("y") | Some("Y") | Some("yes")
    ))
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

fn object_at_mut<'a>(value: &'a mut Value, key: &str) -> Result<&'a mut Map<String, Value>> {
    if !value.is_object() {
        *value = Value::Object(Map::new());
    }
    object_at_mut_map(value.as_object_mut().expect("object"), key)
}

fn object_at_mut_map<'a>(
    object: &'a mut Map<String, Value>,
    key: &str,
) -> Result<&'a mut Map<String, Value>> {
    if !object.contains_key(key) {
        object.insert(key.to_string(), Value::Object(Map::new()));
    }
    object
        .get_mut(key)
        .and_then(Value::as_object_mut)
        .ok_or_else(|| GearError::config(format!("'{key}' must be an object")))
}

fn string_field(object: &Map<String, Value>, key: &str, label: &str) -> Result<String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| GearError::config(format!("model '{label}' has no '{key}'")))
}

/// A deterministic, readable key fragment: ASCII alphanumerics, '.', '_' kept;
/// every other run of characters becomes a single '-'.
fn slug(raw: &str) -> String {
    let mut out = String::new();
    let mut pending_dash = false;
    for character in raw.trim().chars() {
        if character.is_ascii_alphanumeric() || character == '.' || character == '_' {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            pending_dash = false;
            out.push(character);
        } else {
            pending_dash = true;
        }
    }
    if out.is_empty() {
        "model".to_string()
    } else {
        out
    }
}

/// A display label: each `-`/`_`/`.`/space separated segment is capitalized.
fn title(raw: &str) -> String {
    let mut segments: Vec<String> = Vec::new();
    for segment in raw.split(['-', '_', '.', ' ']) {
        if segment.is_empty() {
            continue;
        }
        let mut characters = segment.chars();
        let first = characters.next().expect("non-empty segment");
        let mut piece = String::new();
        piece.push(first.to_ascii_uppercase());
        piece.push_str(characters.as_str());
        segments.push(piece);
    }
    if segments.is_empty() {
        raw.to_string()
    } else {
        segments.join(" ")
    }
}

fn usage(message: impl Into<String>) -> GearError {
    GearError::config(message.into())
}

#!/usr/bin/env python3
"""OpenCode Gear configuration builder.

OpenCode Gear separates two independent ideas:

* **Throttle** - which OpenAI Lead tier you run (`low` / `mid` / `high`).
  Throttle only ever changes the Lead. It never decides which consumer model
  handles delegated work.
* **Consumer Router** - which specialist model handles each delegated role
  (EXPLORE / BUILD / VERIFY / DEBUG / DOCS).

This module owns the whole resolution pipeline:

    gear defaults  ->  user config  ->  project config  ->  CLI/env throttle

Everything is plain JSON plus one small stdlib-only Python module, so the
routing can be read, tested and overridden without a framework.

Registry files (in ``config/``):

* ``throttle.json``   - throttle level -> Lead model key + reasoning variant
* ``models.json``     - model key -> provider + real provider model id
* ``routing.json``    - role -> model key + optional reasoning variant
* ``permissions.json``- agent isolation profiles
* ``base.json``       - shared OpenCode config merged into the result
* ``prompts/``        - one prompt per role

Only the Python standard library is used.
"""
from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path
from typing import Any, NoReturn

# --------------------------------------------------------------------------
# Constants
# --------------------------------------------------------------------------

LEAD_LEVELS: tuple[str, ...] = ("low", "mid", "high")
LEAD_ROLE = "lead"
CONSUMER_ROLES: tuple[str, ...] = (
    "explore",
    "explore-deep",
    "build",
    "verify",
    "debug",
    "docs",
)
CONSUMER_AGENT_PREFIX = "ocg-"
DEFAULT_PROMPT_NAMES: tuple[str, ...] = (LEAD_ROLE,) + CONSUMER_ROLES
TRACE_ENV = "OC_GEAR_TRACE"

# Files kept in the gear's config/ directory.
REGISTRY_FILES = ("throttle.json", "models.json", "routing.json", "permissions.json", "base.json")


class ConfigError(Exception):
    """A configuration problem the user must fix."""


# --------------------------------------------------------------------------
# Small helpers
# --------------------------------------------------------------------------


def default_gear_home() -> Path:
    return Path(__file__).resolve().parents[1]


def gear_home() -> Path:
    env = os.environ.get("OC_GEAR_HOME")
    return Path(env).expanduser().resolve() if env else default_gear_home()


def deep_merge(base: Any, override: Any) -> Any:
    """Recursively merge ``override`` onto ``base``.

    Dictionaries merge key by key; every other value (including lists) is
    replaced by the override.
    """
    if isinstance(base, dict) and isinstance(override, dict):
        result = dict(base)
        for key, value in override.items():
            result[key] = deep_merge(result.get(key), value)
        return result
    if override is None and isinstance(base, dict):
        return dict(base)
    return override


def load_json(path: Path) -> dict[str, Any]:
    if not path.is_file():
        raise ConfigError(f"missing configuration file: {path}")
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        raise ConfigError(f"{path} is not valid JSON: {exc}") from exc
    if not isinstance(value, dict):
        raise ConfigError(f"{path} must contain a JSON object")
    return value


def read_json_if_exists(path: Path | None) -> dict[str, Any]:
    if path is None or not path.is_file():
        return {}
    return load_json(path)


# --------------------------------------------------------------------------
# Layered configuration
# --------------------------------------------------------------------------


def xdg_config_home() -> Path:
    env = os.environ.get("XDG_CONFIG_HOME")
    if env:
        return Path(env).expanduser()
    return Path.home() / ".config"


def user_config_path(explicit: str | None = None) -> Path | None:
    env = os.environ.get("OC_GEAR_USER_CONFIG")
    raw = explicit or env
    if raw:
        return Path(raw).expanduser()
    return xdg_config_home() / "opencode-gear" / "config.json"


def project_config_path(cwd: Path, explicit: str | None = None) -> Path | None:
    env = os.environ.get("OC_GEAR_PROJECT_CONFIG")
    raw = explicit or env
    if raw:
        return Path(raw).expanduser()
    return cwd / ".opencode-gear.json"


def load_defaults(home: Path) -> dict[str, Any]:
    config = home / "config"
    models = load_json(config / "models.json")
    throttle = load_json(config / "throttle.json")
    routing = load_json(config / "routing.json")
    permissions = load_json(config / "permissions.json")
    base = load_json(config / "base.json")

    for key in ("providers", "models"):
        if key not in models:
            raise ConfigError(f"config/models.json is missing '{key}'")
    for key in ("default", "levels"):
        if key not in throttle:
            raise ConfigError(f"config/throttle.json is missing '{key}'")
    if "roles" not in routing:
        raise ConfigError("config/routing.json is missing 'roles'")

    prompts: dict[str, Any] = {}
    prompt_dir = config / "prompts"
    for role in DEFAULT_PROMPT_NAMES:
        prompts[role] = str(prompt_dir / f"{role}.md")

    return {
        "throttle": throttle,
        "models": models,
        "routing": routing,
        "permissions": permissions,
        "base": base,
        "prompts": prompts,
        "observability": {"enabled": False, "path": None},
    }


def config_layers(
    home: Path,
    cwd: Path,
    user_config: str | None = None,
    project_config: str | None = None,
) -> list[tuple[str, Path]]:
    layers: list[tuple[str, Path]] = []
    user = user_config_path(user_config)
    project = project_config_path(cwd, project_config)
    if user is not None:
        layers.append(("user", user))
    if project is not None:
        layers.append(("project", project))
    return layers


def build_effective(
    home: Path,
    cwd: Path,
    user_config: str | None = None,
    project_config: str | None = None,
) -> tuple[dict[str, Any], list[tuple[str, Path]]]:
    effective = load_defaults(home)
    layers = config_layers(home, cwd, user_config, project_config)
    applied: list[tuple[str, Path]] = []
    for name, path in layers:
        if path.is_file():
            effective = deep_merge(effective, load_json(path))
            applied.append((name, path))
    return effective, applied


# --------------------------------------------------------------------------
# Model / role resolution
# --------------------------------------------------------------------------


def providers(effective: dict[str, Any]) -> dict[str, Any]:
    return effective["models"].get("providers", {})


def model_registry(effective: dict[str, Any]) -> dict[str, Any]:
    return effective["models"].get("models", {})


def provider_label(effective: dict[str, Any], provider: str) -> str:
    entry = providers(effective).get(provider)
    if isinstance(entry, dict):
        return str(entry.get("label") or provider)
    return provider


def model_entry(effective: dict[str, Any], key: str) -> dict[str, Any]:
    entry = model_registry(effective).get(key)
    if not isinstance(entry, dict):
        raise ConfigError(f"unknown model key: {key!r}")
    return entry


def model_full_id(effective: dict[str, Any], key: str) -> tuple[str, str]:
    entry = model_entry(effective, key)
    provider = entry.get("provider")
    model_id = entry.get("id")
    if not isinstance(provider, str) or not provider:
        raise ConfigError(f"model {key!r} has no provider")
    if not isinstance(model_id, str) or not model_id:
        raise ConfigError(f"model {key!r} has no id")
    return provider, f"{provider}/{model_id}"


def model_label(effective: dict[str, Any], key: str) -> str:
    entry = model_entry(effective, key)
    return str(entry.get("label") or entry.get("id") or key)


def lead_agent_id(level: str) -> str:
    return f"lead-{level}"


def consumer_agent_id(role: str) -> str:
    return f"{CONSUMER_AGENT_PREFIX}{role}"


def role_specs(effective: dict[str, Any]) -> dict[str, Any]:
    return effective["routing"].get("roles", {})


def resolve_throttle(effective: dict[str, Any], cli: str | None = None) -> str:
    env = os.environ.get("OC_GEAR_THROTTLE")
    for candidate in (cli, env):
        if candidate:
            return candidate
    return str(effective["throttle"].get("default", ""))


def prompt_source(effective: dict[str, Any], role: str) -> Any:
    prompts = effective.get("prompts", {})
    if role not in prompts:
        raise ConfigError(f"no prompt configured for role {role!r}")
    return prompts[role]


def resolve_prompt_path(raw: str, effective: dict[str, Any], cwd: Path) -> Path:
    candidate = Path(raw).expanduser()
    if candidate.is_absolute():
        return candidate
    for base in (cwd, Path(effective.get("_home", ".")) / "config"):
        resolved = (base / candidate).resolve()
        if resolved.is_file():
            return resolved
    return (cwd / candidate).resolve()


def read_prompt(effective: dict[str, Any], role: str, cwd: Path) -> tuple[dict[str, str], str]:
    source = prompt_source(effective, role)
    if isinstance(source, dict):
        if "text" in source:
            text = str(source["text"])
        elif "path" in source:
            text = resolve_prompt_path(str(source["path"]), effective, cwd).read_text(encoding="utf-8")
        else:
            raise ConfigError(f"prompt for role {role!r} must be a path or have 'text'/'path'")
    elif isinstance(source, str):
        path = resolve_prompt_path(source, effective, cwd)
        if not path.is_file():
            raise ConfigError(f"prompt file for role {role!r} not found: {path}")
        text = path.read_text(encoding="utf-8")
    else:
        raise ConfigError(f"prompt for role {role!r} has an unsupported value")

    meta, body = split_frontmatter(text)
    if not body.strip():
        raise ConfigError(f"prompt for role {role!r} is empty")
    return meta, body.strip()


def split_frontmatter(text: str) -> tuple[dict[str, str], str]:
    if text.startswith("---"):
        parts = text.split("---", 2)
        if len(parts) == 3:
            meta: dict[str, str] = {}
            for line in parts[1].strip().splitlines():
                if ":" in line:
                    key, value = line.split(":", 1)
                    meta[key.strip()] = value.strip()
            return meta, parts[2].strip()
    return {}, text.strip()


# --------------------------------------------------------------------------
# Validation
# --------------------------------------------------------------------------


def validate_variant(effective: dict[str, Any], spec: dict[str, Any], where: str, errors: list[str]) -> None:
    variant = spec.get("variant")
    if variant is None:
        return
    if not isinstance(variant, str):
        errors.append(f"{where}: variant must be a string")
        return
    entry = model_registry(effective).get(spec.get("model"))
    if not isinstance(entry, dict):
        return  # unknown model already reported
    variants = entry.get("variants")
    if isinstance(variants, list) and variant not in variants:
        errors.append(
            f"{where}: variant {variant!r} is not exposed by model "
            f"{spec.get('model')!r} (available: {', '.join(map(str, variants))})"
        )


def validate(effective: dict[str, Any], cwd: Path) -> list[str]:
    errors: list[str] = []
    levels = effective["throttle"].get("levels", {})
    default = effective["throttle"].get("default")
    if default not in levels:
        errors.append(f"throttle.default {default!r} is not one of the defined levels: {', '.join(levels)}")
    for level in LEAD_LEVELS:
        if level not in levels:
            errors.append(f"throttle level {level!r} is not defined")
    for level, spec in levels.items():
        if not isinstance(spec, dict) or "model" not in spec:
            errors.append(f"throttle level {level!r} is missing 'model'")
            continue
        if spec["model"] not in model_registry(effective):
            errors.append(f"throttle level {level!r} references unknown model {spec['model']!r}")
        validate_variant(effective, spec, f"throttle level {level!r}", errors)

    roles = role_specs(effective)
    if not roles:
        errors.append("routing.roles is empty")
    for role, spec in roles.items():
        if not isinstance(spec, dict) or "model" not in spec:
            errors.append(f"routing role {role!r} is missing 'model'")
            continue
        if spec["model"] not in model_registry(effective):
            errors.append(f"routing role {role!r} references unknown model {spec['model']!r}")
        validate_variant(effective, spec, f"routing role {role!r}", errors)
        for fallback in spec.get("fallback", []) or []:
            if not isinstance(fallback, dict) or "model" not in fallback:
                errors.append(f"routing role {role!r} has a malformed fallback entry")
                continue
            if fallback["model"] not in model_registry(effective):
                errors.append(
                    f"routing role {role!r} fallback references unknown model {fallback['model']!r}"
                )
            validate_variant(effective, fallback, f"routing role {role!r} fallback", errors)

    small = effective["routing"].get("small_model")
    if small and small not in model_registry(effective):
        errors.append(f"routing.small_model references unknown model {small!r}")

    for key, entry in model_registry(effective).items():
        if not isinstance(entry, dict):
            errors.append(f"model {key!r} must be an object")
            continue
        provider = entry.get("provider")
        if provider not in providers(effective):
            errors.append(f"model {key!r} uses provider {provider!r}, which is not declared in models.providers")
        if not entry.get("id"):
            errors.append(f"model {key!r} is missing 'id'")

    role_profiles = effective["permissions"].get("role_profiles", {})
    profiles = effective["permissions"].get("profiles", {})
    for role in roles:
        profile = role_profiles.get(role)
        if profile is not None and profile not in profiles:
            errors.append(f"permissions.role_profiles[{role!r}] points at unknown profile {profile!r}")

    for role in (LEAD_ROLE,) + tuple(roles):
        try:
            read_prompt(effective, role, cwd)
        except ConfigError as exc:
            errors.append(str(exc))

    return errors


def require_valid(effective: dict[str, Any], cwd: Path) -> None:
    errors = validate(effective, cwd)
    if errors:
        raise ConfigError("configuration is invalid:\n  - " + "\n  - ".join(errors))


# --------------------------------------------------------------------------
# Build the OpenCode config
# --------------------------------------------------------------------------


def role_provider_models(effective: dict[str, Any]) -> list[str]:
    """Every model key referenced by any lead level, role, fallback or small_model."""
    keys: list[str] = []
    for spec in effective["throttle"].get("levels", {}).values():
        if isinstance(spec, dict) and spec.get("model"):
            keys.append(spec["model"])
    for spec in role_specs(effective).values():
        if not isinstance(spec, dict):
            continue
        if spec.get("model"):
            keys.append(spec["model"])
        for fallback in spec.get("fallback", []) or []:
            if isinstance(fallback, dict) and fallback.get("model"):
                keys.append(fallback["model"])
    small = effective["routing"].get("small_model")
    if small:
        keys.append(small)
    return keys


def enabled_provider_order(effective: dict[str, Any]) -> list[str]:
    used = []
    for key in role_provider_models(effective):
        provider, _ = model_full_id(effective, key)
        if provider not in used:
            used.append(provider)
    ordered = [p for p in providers(effective) if p in used]
    ordered += [p for p in used if p not in ordered]
    return ordered


def routing_block(effective: dict[str, Any], level: str) -> str:
    lines = [
        "## Configured consumer routing",
        "",
        "| Role | Agent | Provider / model |",
        "| --- | --- | --- |",
    ]
    for role, spec in role_specs(effective).items():
        provider, full = model_full_id(effective, spec["model"])
        variant = spec.get("variant")
        rendered = model_label(effective, spec["model"])
        if variant:
            rendered += f" ({variant})"
        lines.append(f"| {role.upper()} | `{consumer_agent_id(role)}` | {provider_label(effective, provider)} / {rendered} |")
    fallbacks = []
    for role, spec in role_specs(effective).items():
        for fallback in spec.get("fallback", []) or []:
            provider, _ = model_full_id(effective, fallback["model"])
            fallbacks.append(
                f"- {role.upper()}: {provider_label(effective, provider)} / {model_label(effective, fallback['model'])}"
            )
    if fallbacks:
        lines += [
            "",
            "Configured fallbacks (use only when the primary model fails or its",
            "quota is exhausted; never treat a fallback as the new default):",
            *fallbacks,
        ]
    lines += [
        "",
        "OpenAI is the Lead only. Never route EXPLORE / BUILD / VERIFY / DEBUG",
        "to an OpenAI model unless the user explicitly configures it.",
    ]
    return "\n".join(lines)


def render_lead_prompt(effective: dict[str, Any], level: str, template: str) -> str:
    text = template
    for role in role_specs(effective):
        text = text.replace("{{" + role.replace("-", "_") + "}}", consumer_agent_id(role))
    text = text.replace("{{throttle}}", level)
    text = text.replace("{{routing}}", routing_block(effective, level))
    return text


def build_opencode_config(effective: dict[str, Any], level: str, cwd: Path) -> dict[str, Any]:
    require_valid(effective, cwd)
    levels = effective["throttle"]["levels"]
    if level not in levels:
        raise ConfigError(f"unknown throttle level: {level!r} (available: {', '.join(levels)})")

    roles = role_specs(effective)
    permissions = effective["permissions"]
    subagent = permissions.get("subagent", {})
    subagent_permission = subagent.get("permission", {})
    profiles = permissions.get("profiles", {})
    role_profiles = permissions.get("role_profiles", {})
    lead_permission = permissions.get("lead", {})

    consumer_agents = {role: consumer_agent_id(role) for role in roles}
    agent_cfg: dict[str, Any] = {}

    # Lead agents - one per throttle level, so the TUI can cycle them live.
    lead_template = read_prompt(effective, LEAD_ROLE, cwd)[1]
    for lead_level, spec in levels.items():
        provider, full = model_full_id(effective, spec["model"])
        lead_spec: dict[str, Any] = {
            "mode": "primary",
            "model": full,
            "temperature": lead_permission.get("temperature", 0.1),
            "prompt": render_lead_prompt(effective, lead_level, lead_template),
            "permission": {
                "task": {
                    "*": lead_permission.get("task_default", "deny"),
                    **{name: "allow" for name in consumer_agents.values()},
                }
            },
        }
        if spec.get("variant"):
            lead_spec["variant"] = spec["variant"]
        agent_cfg[lead_agent_id(lead_level)] = lead_spec

    # Consumer agents - one each, independent of throttle.
    for role, spec in roles.items():
        provider, full = model_full_id(effective, spec["model"])
        meta, body = read_prompt(effective, role, cwd)
        profile_name = role_profiles.get(role)
        profile = profiles.get(profile_name, {}) if profile_name else {}
        permission = deep_merge(subagent_permission, profile)
        consumer_spec: dict[str, Any] = {
            "mode": "subagent",
            "model": full,
            "description": spec.get("description") or meta.get("description") or role,
            "prompt": body,
            "hidden": bool(subagent.get("hidden", True)),
            "permission": permission,
        }
        if "temperature" in meta:
            try:
                consumer_spec["temperature"] = float(meta["temperature"])
            except ValueError as exc:
                raise ConfigError(f"{role} prompt temperature is not a number") from exc
        if spec.get("variant"):
            consumer_spec["variant"] = spec["variant"]
        agent_cfg[consumer_agents[role]] = consumer_spec

    small_key = effective["routing"].get("small_model")
    result: dict[str, Any] = {
        "default_agent": lead_agent_id(level),
        "model": model_full_id(effective, levels[level]["model"])[1],
        "enabled_providers": enabled_provider_order(effective),
        "agent": agent_cfg,
    }
    if small_key:
        result["small_model"] = model_full_id(effective, small_key)[1]

    merged = deep_merge(effective.get("base", {}), result)
    extra = effective.get("opencode")
    if isinstance(extra, dict):
        merged = deep_merge(merged, extra)
    return merged


# --------------------------------------------------------------------------
# Reporting
# --------------------------------------------------------------------------


def routing_rows(effective: dict[str, Any]) -> list[tuple[str, str, str, str]]:
    rows = []
    for role, spec in role_specs(effective).items():
        provider, full = model_full_id(effective, spec["model"])
        rows.append((role, consumer_agent_id(role), provider_label(effective, provider), full))
    return rows


def routing_text(effective: dict[str, Any]) -> str:
    lines = ["Consumer router (role -> model):", ""]
    for role, agent, provider, full in routing_rows(effective):
        lines.append(f"  {role:<13} {agent:<18} {provider:<24} {full}")
    small = effective["routing"].get("small_model")
    if small:
        provider, full = model_full_id(effective, small)
        lines += ["", f"  small_model   {provider_label(effective, provider)} -> {full}"]
    return "\n".join(lines)


def throttle_rows(effective: dict[str, Any]) -> list[tuple[str, str, str]]:
    rows = []
    for level, spec in effective["throttle"].get("levels", {}).items():
        provider, full = model_full_id(effective, spec["model"])
        variant = spec.get("variant") or "provider-default"
        rows.append((level, full, variant))
    return rows


def throttle_text(effective: dict[str, Any]) -> str:
    lines = ["Throttle (OpenAI Lead tier):", ""]
    for level, full, variant in throttle_rows(effective):
        lines.append(f"  {level:<6} {full:<24} {variant}")
    lines += ["", f"  default: {effective['throttle'].get('default')}"]
    return "\n".join(lines)


def status_text(
    effective: dict[str, Any],
    level: str,
    cwd: Path,
    applied_layers: list[tuple[str, Path]],
) -> str:
    lines = [
        "OpenCode Gear",
        "",
        f"  throttle      {level}",
        f"  default agent {lead_agent_id(level)}",
        f"  lead model    {model_full_id(effective, effective['throttle']['levels'][level]['model'])[1]}",
        f"  consumers     {len(role_specs(effective))}",
        f"  providers     {', '.join(enabled_provider_order(effective)) or '(none)'}",
        f"  cwd           {cwd}",
        "",
        "Config layers:",
    ]
    layers = config_layers(gear_home_from(effective), cwd)
    applied = {path for _, path in applied_layers}
    for name, path in layers:
        mark = "applied" if path in applied else "not found"
        lines.append(f"  {name:<8} {path}  [{mark}]")
    lines += ["", throttle_text(effective), "", routing_text(effective)]
    return "\n".join(lines)


def gear_home_from(effective: dict[str, Any]) -> Path:
    return Path(effective.get("_home", "."))


# --------------------------------------------------------------------------
# Observability (opt-in, local only, no prompts or source content)
# --------------------------------------------------------------------------


def observability_config(effective: dict[str, Any]) -> dict[str, Any]:
    cfg = effective.get("observability") or {}
    return cfg if isinstance(cfg, dict) else {}


def trace_path(effective: dict[str, Any]) -> Path | None:
    cfg = observability_config(effective)
    if not cfg.get("enabled"):
        return None
    raw = os.environ.get(TRACE_ENV) or cfg.get("path")
    if raw:
        return Path(str(raw)).expanduser()
    return Path.home() / ".local" / "state" / "opencode-gear" / "events.jsonl"


def record_event(effective: dict[str, Any], event: str, level: str) -> Path | None:
    path = trace_path(effective)
    if path is None:
        return None
    record = {
        "ts": _now_iso(),
        "event": event,
        "throttle": level,
        "default_agent": lead_agent_id(level),
        "lead": model_full_id(effective, effective["throttle"]["levels"][level]["model"])[1],
        "routing": {
            role: model_full_id(effective, spec["model"])[1]
            for role, spec in role_specs(effective).items()
        },
    }
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        with path.open("a", encoding="utf-8") as handle:
            handle.write(json.dumps(record, ensure_ascii=False, sort_keys=True) + "\n")
    except OSError:
        return None
    return path


def _now_iso() -> str:
    import datetime

    return datetime.datetime.now(datetime.timezone.utc).replace(microsecond=0).isoformat()


# --------------------------------------------------------------------------
# CLI
# --------------------------------------------------------------------------


def load_command_effective(args: argparse.Namespace) -> tuple[dict[str, Any], list[tuple[str, Path]]]:
    home = gear_home()
    cwd = Path(args.cwd).expanduser().resolve()
    effective, applied = build_effective(home, cwd, args.user_config, args.project_config)
    effective["_home"] = str(home)
    effective["_cwd"] = str(cwd)
    return effective, applied


def add_common(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--cwd", default=os.getcwd(), help="project directory used for project-local overrides")
    parser.add_argument("--throttle", help="override the throttle level for this command")
    parser.add_argument("--user-config", help="path to a user override file")
    parser.add_argument("--project-config", help="path to a project override file")


def cmd_build(args: argparse.Namespace) -> int:
    effective, _ = load_command_effective(args)
    level = resolve_throttle(effective, args.throttle)
    config = build_opencode_config(effective, level, Path(effective["_cwd"]))
    text = json.dumps(config, ensure_ascii=False, indent=2 if args.pretty else None, separators=None if args.pretty else (",", ":"))
    print(text)
    return 0


def cmd_validate(args: argparse.Namespace) -> int:
    effective, _ = load_command_effective(args)
    cwd = Path(effective["_cwd"])
    errors = validate(effective, cwd)
    if errors:
        print("OpenCode Gear configuration errors:", file=sys.stderr)
        for error in errors:
            print(f"  - {error}", file=sys.stderr)
        return 1
    level = resolve_throttle(effective, args.throttle)
    build_opencode_config(effective, level, cwd)
    print("configuration is valid")
    return 0


def cmd_routing(args: argparse.Namespace) -> int:
    effective, _ = load_command_effective(args)
    require_valid(effective, Path(effective["_cwd"]))
    print(routing_text(effective))
    return 0


def cmd_throttle(args: argparse.Namespace) -> int:
    effective, _ = load_command_effective(args)
    if not args.level:
        print(resolve_throttle(effective, args.throttle))
        return 0
    level = args.level
    if level not in effective["throttle"]["levels"]:
        raise ConfigError(f"unknown throttle level: {level!r} (available: {', '.join(effective['throttle']['levels'])})")
    path = user_config_path(args.user_config)
    if path is None:
        raise ConfigError("no user config path available")
    existing = read_json_if_exists(path)
    throttle = existing.setdefault("throttle", {})
    if not isinstance(throttle, dict):
        raise ConfigError(f"{path}: 'throttle' must be an object")
    throttle["default"] = level
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(existing, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(f"default throttle set to {level} in {path}")
    return 0


def cmd_status(args: argparse.Namespace) -> int:
    effective, applied = load_command_effective(args)
    level = resolve_throttle(effective, args.throttle)
    require_valid(effective, Path(effective["_cwd"]))
    print(status_text(effective, level, Path(effective["_cwd"]), applied))
    return 0


def cmd_layers(args: argparse.Namespace) -> int:
    effective, _ = load_command_effective(args)
    home = gear_home_from(effective)
    print(f"gear home: {home}")
    for name, path in config_layers(home, Path(effective["_cwd"]), args.user_config, args.project_config):
        print(f"{name:<8} {path}  [{'found' if path.is_file() else 'not found'}]")
    print(f"{'trace':<8} {trace_path(effective) or '(disabled)'}")
    return 0


def cmd_trace(args: argparse.Namespace) -> int:
    effective, _ = load_command_effective(args)
    level = resolve_throttle(effective, args.throttle)
    path = record_event(effective, args.event, level)
    if path is None:
        return 0
    print(str(path))
    return 0


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(prog="oc-config.py", description="OpenCode Gear configuration builder")
    sub = parser.add_subparsers(dest="command", required=True)

    build = sub.add_parser("build", help="print the resolved OpenCode config")
    add_common(build)
    build.add_argument("--pretty", action="store_true", help="pretty-print the JSON")
    build.set_defaults(func=cmd_build)

    validate = sub.add_parser("validate", help="validate the resolved configuration")
    add_common(validate)
    validate.set_defaults(func=cmd_validate)

    routing = sub.add_parser("routing", help="show role -> model routing")
    add_common(routing)
    routing.set_defaults(func=cmd_routing)

    throttle = sub.add_parser("throttle", help="print or persist the default throttle level")
    add_common(throttle)
    throttle.add_argument("level", nargs="?", help="level to persist as the user default")
    throttle.set_defaults(func=cmd_throttle)

    status = sub.add_parser("status", help="show throttle, routing and config layers")
    add_common(status)
    status.set_defaults(func=cmd_status)

    layers = sub.add_parser("layers", help="show configuration layers and trace state")
    add_common(layers)
    layers.set_defaults(func=cmd_layers)

    trace = sub.add_parser("trace", help="append a local routing event (opt-in)")
    add_common(trace)
    trace.add_argument("--event", default="launch")
    trace.set_defaults(func=cmd_trace)

    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> NoReturn:
    args = parse_args(argv if argv is not None else sys.argv[1:])
    try:
        code = args.func(args)
    except ConfigError as exc:
        print(f"oc-config: {exc}", file=sys.stderr)
        raise SystemExit(2) from exc
    raise SystemExit(code)


if __name__ == "__main__":
    main()

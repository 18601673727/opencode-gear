# Configuration reference

All configuration is JSON. There is no framework, no plugin system and no
database; the builder is `bin/oc_config.py`, standard library only.

## Layers

```text
1. gear defaults      <gear>/config/*.json
2. user config        ~/.config/opencode-gear/config.json
3. project config     <project>/.opencode-gear.json
4. CLI / environment  --throttle, OC_GEAR_THROTTLE
```

Layers are deep-merged in order. Later layers win; dictionaries merge key by
key and lists are replaced wholesale.

`oc layers` prints which files were found. `oc status` prints the resolved
values.

## Override file shape

An override file uses the same top-level keys as the gear's `config/` files:

| Key | Mirrors | Purpose |
| --- | --- | --- |
| `throttle` | `config/throttle.json` | `{ "default": "low", "levels": { ... } }` |
| `models` | `config/models.json` | `{ "providers": {...}, "models": {...} }` |
| `routing` | `config/routing.json` | `{ "small_model": "...", "roles": {...} }` |
| `permissions` | `config/permissions.json` | profiles, role bindings, isolation |
| `prompts` | `config/prompts/` | role -> path or `{"text": "..."}` |
| `observability` | — | `{ "enabled": bool, "path": "..." }` |
| `opencode` | — | raw OpenCode config merged into the result last |

Example:

```json
{
  "throttle": { "default": "mid" },
  "routing": { "roles": { "build": { "model": "glm-5.3", "variant": "max" } } },
  "opencode": { "username": "you" }
}
```

## Registries

### `config/models.json`

```json
{
  "providers": { "<provider-id>": { "label": "...", "note": "..." } },
  "models": {
    "<key>": {
      "provider": "<provider-id>",
      "id": "<real provider model id>",
      "label": "human name",
      "variants": ["low", "high"]
    }
  }
}
```

`variants` is optional and documents the reasoning variants the provider
really exposes. If present, `oc validate` rejects any configured variant that
is not in the list, which stops invented reasoning levels from sneaking in.

### `config/throttle.json`

```json
{
  "default": "low",
  "levels": {
    "low":  { "model": "<model key>", "variant": "medium" },
    "mid":  { "model": "<model key>", "variant": "high" },
    "high": { "model": "<model key>", "variant": "high" }
  }
}
```

All three levels must exist. Variant may be omitted to use the provider
default.

### `config/routing.json`

```json
{
  "small_model": "<model key>",
  "roles": {
    "<role>": {
      "model": "<model key>",
      "variant": "high",
      "description": "shown to the Lead",
      "fallback": [ { "model": "<model key>", "variant": "high" } ]
    }
  }
}
```

The shipped roles are `explore`, `explore-deep`, `build`, `verify`, `debug`
and `docs`. Adding a role requires a matching prompt and, if you want a
non-default profile, a `permissions.role_profiles` entry.

### `config/permissions.json`

```json
{
  "profiles": { "<name>": { "<permission>": "allow" | "deny" | "ask" } },
  "role_profiles": { "<role>": "<profile>" },
  "subagent": { "hidden": true, "permission": { "task": "deny" } },
  "lead": { "temperature": 0.1, "task_default": "deny" }
}
```

Permission objects are last-match-wins; `"*"` sets the default and specific
resource keys refine it. `bash` may be a pattern map (command glob -> effect).
The `task` permission is evaluated against subagent names.

## Prompts

One prompt per role. The Lead prompt is a template and supports:

| Placeholder | Replaced with |
| --- | --- |
| `{{explore}}`, `{{explore_deep}}`, `{{build}}`, `{{verify}}`, `{{debug}}`, `{{docs}}` | the consumer agent id |
| `{{throttle}}` | the active level |
| `{{routing}}` | a generated routing table (+ configured fallbacks) |

Consumer prompts may carry YAML front matter with `description` and
`temperature`; those become agent config.

Prompt values in an override may be:

| Form | Meaning |
| --- | --- |
| `"path/to/prompt.md"` | replace the gear prompt with this file |
| `{ "path": "..." }` | same as above |
| `{ "text": "..." }` | replace with inline text |
| `{ "append": ["a.md", {"text": "..."}] }` | keep the gear prompt and append blocks |
| `{ "path": "...", "append": [...] }` | replace, then append |

Appended blocks are joined to the base prompt with a `---` separator, and
placeholder substitution applies to the whole assembled prompt. This is the
supported way to layer repository-specific policy on top of the gear prompt
without forking it:

```json
{ "prompts": { "lead": { "append": [".opencode/lead-policy.md"] } } }
```

The gear keeps the unmodified prompt for every role in `_prompt_defaults`, so
an append-only override never has to restate the core prompt.

## Environment variables

| Variable | Effect |
| --- | --- |
| `OC_GEAR_HOME` | gear installation directory |
| `OC_GEAR_THROTTLE` | default throttle level |
| `OC_GEAR_USER_CONFIG` | path to the user override file |
| `OC_GEAR_PROJECT_CONFIG` | path to the project override file |
| `OC_GEAR_OPENCODE_BIN` | `opencode` binary to run |
| `OC_GEAR_PYTHON` | python interpreter |
| `OC_GEAR_TRACE` | trace file, read only when observability is enabled |

## Builder commands

`oc` wraps these; they are also usable directly:

```text
bin/oc_config.py build [--throttle LEVEL] [--cwd DIR] [--pretty]
bin/oc_config.py validate [--cwd DIR]
bin/oc_config.py routing [--cwd DIR]
bin/oc_config.py status [--throttle LEVEL] [--cwd DIR]
bin/oc_config.py throttle [LEVEL] [--cwd DIR]
bin/oc_config.py layers [--cwd DIR]
bin/oc_config.py trace --event launch [--cwd DIR]
```

`build` prints the merged OpenCode config consumed through
`OPENCODE_CONFIG_CONTENT`. `validate` exits non-zero on any configuration
error.

## Observability

Off by default. When enabled, `oc` appends one JSON line per launch to a local
file:

```json
{"ts":"2026-01-01T00:00:00+00:00","event":"launch","throttle":"low",
 "default_agent":"lead-low","lead":"openai/gpt-5.6-sol",
 "routing":{"explore":"volcengine-coding/kimi-k2.7-code","build":"opencode-go/deepseek-v4.1-flash"}}
```

- Local file only. No remote telemetry, no network calls.
- No prompts, no source code, no credentials.
- Captures routing decisions only. Success/failure, retry counts and duration
  require a session-level hook that this project does not ship.

Delete the file to delete the history. Disable by removing the
`observability.enabled` flag.

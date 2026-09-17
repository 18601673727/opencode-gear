# Configuration reference

All configuration is JSON. There is no framework, no plugin system and no
database. `ocg` is a single Rust binary that embeds the shipped defaults and
merges overrides on top.

## Layers

```text
1. embedded defaults  config/*.json (compiled into ocg)
2. user config        ~/.config/opencode-gear/config.json
3. project config     <project>/.opencode-gear.json
4. CLI / environment  ocg high, --throttle, OPENCODE_GEAR_THROTTLE
```

Layers are deep-merged in order. Later layers win; dictionaries merge key by
key and lists are replaced wholesale.

An explicit `OPENCODE_GEAR_HOME` (legacy `OC_GEAR_HOME`) directory containing
`config/` replaces layer 1 with files from disk.

`ocg layers` prints which files were found. `ocg status` prints the resolved
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
| `runtime` | — | managed OpenCode runtime policy (see below) |
| `context` | — | local context engine policy (see below) |
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
really exposes. If present, `ocg validate` rejects any configured variant that
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
and `docs`, but roles are not hardcoded. Adding a role requires a matching
prompt and, if you want a non-default profile, a `permissions.role_profiles`
entry. The generated agent is named `ocg-<role>`, and `{{<role>}}` placeholders
(underscores for dashes) are substituted in the Lead prompt.

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
| any custom role, e.g. `{{audit}}` | its `ocg-audit` agent id |
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

The gear keeps the unmodified prompt for every role, so an append-only override
never has to restate the core prompt.

## Runtime policy

The managed OpenCode runtime is configured by the top-level `runtime` object.
It is deliberately separate from the raw `opencode` config key.

```json
{
  "runtime": {
    "channel": "latest",
    "autoUpgrade": true,
    "checkIntervalHours": 24,
    "fallback": "project-local",
    "version": "1.18.31"
  }
}
```

| Field | Default | Meaning |
| --- | --- | --- |
| `channel` | `"latest"` | Only `latest` is supported today. |
| `autoUpgrade` | `true` | Allow optional upgrades of an existing compatible runtime when a check is due. |
| `checkIntervalHours` | `24` | How long an update check stays cached. |
| `fallback` | `"project-local"` | Where a bootstrap install goes. |
| `version` | absent | Exact semver pin; never advances, managed only (no system upgrade). |

Validation rejects unknown values and a pin below the OpenCode `1.18.0` floor.

Resolution order for a launch is: explicit `OPENCODE_GEAR_OPENCODE` -> existing
managed project runtime -> compatible system `opencode` on `PATH` ->
project-local bootstrap.

- A compatible system runtime with a due check runs its own
  `opencode upgrade`, reprobes the version and continues. A failed upgrade
  warns and keeps the old compatible runtime.
- An incompatible or unusable system runtime is upgraded with
  `opencode upgrade` when `autoUpgrade` is on; if that fails or stays
  incompatible, `ocg` bootstraps project-local.
- `autoUpgrade: false` disables the optional upgrades only. A missing or
  incompatible runtime is still bootstrapped, because the fallback is required.
- A managed runtime checks the GitHub release and installs the newer managed
  version. The release asset must carry a `sha256:` digest; a missing digest is
  refused.

The managed runtime lives at
`<project>/.opencode-gear/runtime/opencode/<version>/opencode`, with
`active.json` as the pointer. The executable path is always re-derived from the
version and must be executable; an arbitrary recorded path is never trusted.
The first bootstrap appends `.opencode-gear/` to the project `.gitignore`
exactly once.

The update cache lives in the platform cache directory
(`~/.cache/opencode-gear/` on Linux, `~/Library/Caches/opencode-gear/` on
macOS). Both successful and failed checks are recorded, so a runtime that just
failed to upgrade is not retried within the interval.

## Context policy

The optional local context engine is configured by the top-level `context`
object. Every field is optional and the defaults are conservative; no user
configuration is required.

```json
{
  "context": {
    "enabled": true,
    "cache": true,
    "maxFileBytes": 1000000,
    "maxRepositoryFiles": 100000,
    "maxCandidates": 200,
    "maxFiles": 24,
    "maxSlices": 48,
    "maxBytes": 262144,
    "maxDiffBytes": 131072,
    "maxHunks": 40,
    "maxSymbolsPerFile": 200,
    "includeUntracked": true
  }
}
```

| Field | Default | Meaning |
| --- | --- | --- |
| `enabled` | `true` | Allow context production. When false, `ocg context` prints an informational message, returns success and reads, indexes or caches nothing. |
| `cache` | `true` | Read and write `<project>/.opencode-gear/cache/`. |
| `maxFileBytes` | `1000000` | Files larger than this are indexed by metadata only. |
| `maxRepositoryFiles` | `100000` | Hard cap on files scanned/indexed. Beyond it the repo map, index and plan are marked truncated; git status is collected separately so changed paths stay complete. |
| `maxCandidates` | `200` | Maximum ranked candidates in a plan. |
| `maxFiles` | `24` | Maximum files selected into a plan. |
| `maxSlices` | `48` | Maximum content slices in a plan. |
| `maxBytes` | `262144` | Maximum total slice bytes. |
| `maxDiffBytes` | `131072` | Maximum retained git diff text, and the cap on the git stdout capture. |
| `maxHunks` | `40` | Maximum retained diff hunks; changed paths are always listed. |
| `maxSymbolsPerFile` | `200` | Maximum extracted symbols per file. |
| `includeUntracked` | `true` | Treat untracked files as added in the diff summary. |

`maxFileBytes` must be positive and at most 64 MiB; `maxRepositoryFiles` must be
at least 1 and at most 5,000,000; every other numeric limit must be positive.
`ocg validate` reports violations. The engine never reads, slices or caches
sensitive files (`.env`, `.envrc`, key material, `id_rsa`/`id_ed25519`,
`credentials*`, `secrets*`, `auth*`, `token*`, known OpenCode credential
locations); only their path metadata is indexed.

An ordinary `ocg` / `ocg run` launch does not build the context index. Use
`ocg context <task>` (or the library API); the shipped Lead prompt tells the
agent to prefer those commands. When orchestration is enabled a launch does
materialize the generated plugin and answers its bridge calls with bounded
dynamic context (see [Orchestration policy](#orchestration-policy)); the index
itself is still only built by `ocg context`. Cached plans are only returned
after their source slices, dependency fingerprints and git identity are
re-verified, and a failed cache write leaves the computed plan intact with a
warning note.

Token counts in plans and capsules are always **estimates** (`bytes / 4`) and
are labelled as such.

## Verification policy

The optional verification subsystem is configured by the top-level
`verification` object. Every field is optional. Stages start empty, so a
manifest existing never causes a command to run.

```json
{
  "verification": {
    "enabled": true,
    "defaultStage": "normal",
    "stopOnFailure": true,
    "maxRawLogBytes": 2000000,
    "maxLogStorageBytes": 52428800,
    "includeTestProposal": true,
    "stages": {
      "fast": { "commands": ["cargo fmt --check"] },
      "normal": { "commands": ["cargo check", "cargo test"] },
      "full": { "commands": [] }
    }
  }
}
```

| Field | Default | Meaning |
| --- | --- | --- |
| `enabled` | `true` | Allow `ocg verify` to run configured commands. When false nothing runs and the result is `not_run`. |
| `defaultStage` | `"normal"` | Stage used when the CLI does not name one; one of `fast`, `normal`, `full`. |
| `stopOnFailure` | `true` | Stop the stage at the first failing command. |
| `maxRawLogBytes` | `2000000` | Bound for each captured stdout/stderr stream (max 64 MiB). |
| `maxLogStorageBytes` | `52428800` | Bound for `.opencode-gear/logs/`, oldest files pruned first (max 2 GiB). The log referenced by the current report is never pruned, so a single bounded log always survives. |
| `includeTestProposal` | `true` | Attach the advisory targeted-test proposal to reports. |
| `stages` | all empty | Per-stage `description` and `commands`. |

A command is a `program` + `args` object, or a string parsed without a shell
(`"cargo check"`). Shell control operators, pipelines and redirections are
rejected. `ocg validate` reports violations.

## Capability policy

The optional top-level `capabilities` object registers custom capability names
that a task may select by keyword:

```json
{ "capabilities": { "enabled": true, "custom": ["warehouse"] } }
```

With `enabled: false` the planner is disabled end-to-end: the plan carries
`enabled = false` with no allowed capability, all built-ins and custom names are
denied, and `ocg tools` / the context plan report the disabled state.

Capability planning is a context/config diagnostic, not a security sandbox; see
[verification.md](verification.md).

## Telemetry policy

The optional top-level `telemetry` object controls local event collection:

```json
{ "telemetry": { "enabled": true, "localOnly": true } }
```

| Field | Default | Meaning |
| --- | --- | --- |
| `enabled` | `true` | Record an event for `ocg context`, `ocg verify` and the orchestration bridge. |
| `localOnly` | `true` | Must stay `true`; there is no remote mode, and `false` is rejected by `ocg validate`. |

Events are appended to `<project>/.opencode-gear/telemetry/events.jsonl`. The
schema never includes prompts, source code, command strings, command output,
headers or absolute paths; secret-shaped metadata is redacted before it is
written. A telemetry failure warns and never blocks a command. Set
`"enabled": false` or `OPENCODE_GEAR_TELEMETRY=0` to disable collection. See
[telemetry.md](telemetry.md).

## Orchestration policy

The optional top-level `orchestration` object controls the Rust orchestration
controller and whether a launch generates/injects the OpenCode plugin adapter:

```json
{
  "orchestration": {
    "enabled": true,
    "maxBuildRetries": 2,
    "maxDebugRetries": 1,
    "maxHandoffBytes": 16384,
    "maxHandoffRatioPercent": 60
  }
}
```

| Field | Default | Bounds | Meaning |
| --- | --- | --- | --- |
| `enabled` | `true` | boolean | Generate and inject the adapter; run the controller. Disabled emits no plugin and writes no state. |
| `maxBuildRetries` | `2` | `0`–`10` | Build retries after a failed verification, before Debug is recommended. |
| `maxDebugRetries` | `1` | `0`–`5` | Debug hand-offs before the controller reports escalation. |
| `maxHandoffBytes` | `16384` | `512`–`65536` | Absolute cap on a projected hand-off capsule. |
| `maxHandoffRatioPercent` | `60` | `1`–`100` | Hand-off cap as a percentage of the rich source context. |

`maxHandoffBytes` / `maxHandoffRatioPercent` are a **runtime size envelope**,
not a correctness rule: required evidence (goal, hard constraints, critical
findings, changed files, failing locations) is never dropped to satisfy them,
and an overage is recorded in the capsule's `omitted` notes. The deterministic
release fixture configures a much tighter `4096` / `40%` gate explicitly to
catch projection regressions; that gate is not the default.

`OPENCODE_GEAR_ORCHESTRATION=0` (legacy `OC_GEAR_ORCHESTRATION`) force-disables
orchestration for one process. `ocg doctor` reports the read-only state of the
plugin, state file, projection and verification integration. See
[architecture.md](architecture.md) for the mechanism and
[verification.md](verification.md) for the retry/Debug contract.

## Environment variables

`OPENCODE_GEAR_*` is canonical; the legacy `OC_GEAR_*` names are accepted as
fallbacks.

| Variable | Effect |
| --- | --- |
| `OPENCODE_GEAR_HOME` (legacy `OC_GEAR_HOME`) | load `config/` from this directory instead of the embedded defaults |
| `OPENCODE_GEAR_THROTTLE` (legacy `OC_GEAR_THROTTLE`) | default throttle level |
| `OPENCODE_GEAR_USER_CONFIG` (legacy `OC_GEAR_USER_CONFIG`) | path to the user override file |
| `OPENCODE_GEAR_PROJECT_CONFIG` (legacy `OC_GEAR_PROJECT_CONFIG`) | path to the project override file |
| `OPENCODE_GEAR_OPENCODE` | explicit `opencode` executable (authoritative) |
| `OPENCODE_GEAR_OPENCODE_BIN` / `OC_GEAR_OPENCODE_BIN` | compatibility aliases for the explicit executable |
| `OPENCODE_GEAR_TRACE` (legacy `OC_GEAR_TRACE`) | trace file, read only when observability is enabled |
| `OPENCODE_GEAR_CACHE_DIR` | override the update-check cache directory |
| `OPENCODE_GEAR_API_BASE` | override the GitHub API base (mirrors, tests) |
| `OPENCODE_GEAR_TELEMETRY` (legacy `OC_GEAR_TELEMETRY`) | `0`/`1` to force local telemetry off/on |
| `OPENCODE_GEAR_ORCHESTRATION` (legacy `OC_GEAR_ORCHESTRATION`) | `0`/`1` to force orchestration off/on for one process |

`OPENCODE_GEAR_OCG` and `OPENCODE_GEAR_PROJECT` are exported *to OpenCode* by
`ocg` at launch so the generated adapter can find the bridge and the project;
they are not read from the ambient environment.

## Commands

```text
ocg [low|mid|high] [--throttle LEVEL] [--project DIR] [--dry-run] [--pretty] [command] [args...]

ocg build    [--pretty] [--throttle LEVEL] [--project DIR]
ocg validate [--throttle LEVEL] [--project DIR]
ocg routing  [--project DIR]
ocg status   [--throttle LEVEL] [--project DIR]
ocg throttle [LEVEL] [--project DIR]
ocg layers   [--project DIR]
ocg trace    --event launch [--project DIR]
ocg context  <task...> [--pretty] [--project DIR]
ocg context  symbols <query> [--project DIR]
ocg cache    stats|clean [--project DIR]
ocg stats    [--pretty] [--project DIR]
ocg verify   [fast|normal|full] [--pretty] [--project DIR]
ocg tools    <task...> [--pretty] [--project DIR]
ocg checkpoint list|show <id>|save --phase P [--task T] [--decision D] [--pretty] [--project DIR]
ocg version  (read-only runtime report)
ocg doctor   (read-only environment check)
ocg upgrade  (self-update Gear, then maintain OpenCode)
```

`build` (and `--dry-run`) prints the merged OpenCode config consumed through
`OPENCODE_CONFIG_CONTENT`. `validate` exits non-zero on any configuration
error and prints every problem it finds.

## Observability

Off by default. When enabled, `ocg` appends one JSON line per launch to a local
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

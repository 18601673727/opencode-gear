# Configuration reference

All configuration is YAML. There is no framework, no plugin system and no
database. `ocg` is a single Rust binary that embeds the shipped defaults and
merges overrides on top.

## Layers

```text
1. embedded defaults  config/*.yaml (compiled into ocg)
2. user config        ~/.config/opencode-gear/config.yaml
3. project config     <project>/.opencode-gear.yaml
4. CLI / environment  ocg high, --throttle, OPENCODE_GEAR_THROTTLE
```

Layers are deep-merged in order. Later layers win; dictionaries merge key by
key and lists are replaced wholesale.

An explicit `OPENCODE_GEAR_HOME` (legacy `OC_GEAR_HOME`) directory containing
`config/` replaces layer 1 with files from disk.

The user and project paths can also be set with `OPENCODE_GEAR_USER_CONFIG` /
`OPENCODE_GEAR_PROJECT_CONFIG` or `--user-config` / `--project-config`.

### JSON is rejected, not migrated

The legacy JSON paths — `~/.config/opencode-gear/config.json` and
`<project>/.opencode-gear.json` — are **not** read. When an existing JSON
override is found next to a YAML layer (or an explicit `.json` path is passed),
`ocg` fails with the offending path named and asks you to convert it to YAML.
Config files are never migrated or merged. `ocg init` also refuses when a stale
`.opencode-gear.json` exists.

`ocg layers` prints which files were found. `ocg status` prints the resolved
values.

## Override file shape

An override file uses the same top-level keys as the gear's `config/` files:

| Key | Mirrors | Purpose |
| --- | --- | --- |
| `throttle` | `config/throttle.yaml` | `default` plus `levels` |
| `models` | `config/models.yaml` | `providers` and `models` |
| `routing` | `config/routing.yaml` | `small_model` and `roles` |
| `permissions` | `config/permissions.yaml` | profiles, role bindings, isolation |
| `prompts` | `config/prompts/` | role -> path or `text` |
| `observability` | — | `enabled` and `path` |
| `runtime` | — | managed OpenCode runtime policy (see below) |
| `context` | — | local context engine policy (see below) |
| `opencode` | — | raw OpenCode config merged into the result last |

Example:

```yaml
throttle:
  default: mid
routing:
  roles:
    build:
      model: glm-5.3
      variant: max
opencode:
  username: you
```

A comment-only override file (or the empty mapping `{}`) is a valid no-op.

## Registries

### `config/models.yaml`

```yaml
providers:
  <provider-id>:
    label: human name
    note: how to authenticate
models:
  <key>:
    provider: <provider-id>
    id: <real provider model id>
    label: human name
    variants: [low, high]
```

`variants` is optional and documents the reasoning variants the provider
really exposes. If present, `ocg validate` rejects any configured variant that
is not in the list, which stops invented reasoning levels from sneaking in.

### `config/throttle.yaml`

```yaml
default: low
levels:
  low:
    model: <model key>
    variant: low
  mid:
    model: <model key>
    variant: medium
  high:
    model: <model key>
    variant: low
```

All three levels must exist. `variant` is optional and provider-specific: when
omitted (or set to `null`), the Lead runs at the provider default and neither
the generated config nor the exported contract carries a `variant` key. When
present, static validation (`ocg validate`, `ocg doctor`) rejects any variant the
model does not declare. OCG exports the resolved contract to its generated
plugin, which enforces the selected Lead agent/model (and variant, when one is
configured) at `chat.message`; sticky TUI or reused-session state cannot override
it. Worker requests are left unchanged.

A coding launch also runs a read-only `opencode models` preflight against the
generated config. A definitely missing active Lead model is a launch error, not
a fallback opportunity. Missing non-active Execution Tiers and worker routes warn;
if the probe itself cannot run, launch continues with a warning. `ocg doctor`
reports all configured Execution Tiers and worker routes separately from static
config validation. `ocg models` remains a plain pass-through and neither
materializes nor loads the generated plugin.

### `config/routing.yaml`

```yaml
small_model: <model key>
roles:
  <role>:
    model: <model key>
    variant: high
    description: shown to the Lead
    fallback:
      - model: <model key>
        variant: high
```

The shipped roles are `explore`, `explore-deep`, `build`, `verify`, `debug`
and `docs`, but roles are not hardcoded. Adding a role requires a matching
prompt and, if you want a non-default profile, a `permissions.role_profiles`
entry. The generated agent is named `ocg-<role>`, and `{{<role>}}` placeholders
(underscores for dashes) are substituted in the Lead prompt.

### `config/permissions.yaml`

```yaml
profiles:
  <name>:
    <permission>: allow
role_profiles:
  <role>: <profile>
subagent:
  hidden: true
  permission:
    task: deny
lead:
  temperature: 0.1
  task_default: deny
```

Permission objects are last-match-wins; `"*"` sets the default and specific
resource keys refine it. `bash` may be a pattern map (command glob -> effect).
The `task` permission is evaluated against subagent names.

## Prompts

One prompt per role. The Lead prompt is a template and supports:

| Placeholder | Replaced with |
| --- | --- |
| `{{explore}}`, `{{explore_deep}}`, `{{build}}`, `{{verify}}`, `{{debug}}`, `{{docs}}` | the worker agent id |
| any custom role, e.g. `{{audit}}` | its `ocg-audit` agent id |
| `{{throttle}}` | the active level |
| `{{routing}}` | a generated routing table (+ configured fallbacks) |

Worker prompts may carry YAML front matter with `description` and
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

```yaml
prompts:
  lead:
    append:
      - .opencode/lead-policy.md
```

The gear keeps the unmodified prompt for every role, so an append-only override
never has to restate the core prompt.

## Runtime policy

The managed OpenCode runtime is configured by the top-level `runtime` object.
It is deliberately separate from the raw `opencode` config key.

```yaml
runtime:
  channel: latest
  autoUpgrade: true
  checkIntervalHours: 24
  fallback: project-local
  version: 1.18.31
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
project-local bootstrap. One cross-family exception applies to unpinned
projects: when the existing managed runtime belongs to an older supported
OpenCode family (1.18.x) and the system runtime belongs to a newer supported
family (2.x), the system runtime wins. The managed install stays on disk
untouched; it simply stops shadowing the newer family. A `runtime.version`
pin, a same-family system runtime, an unprobeable system binary, or an
unsupported system major all keep the managed preference.

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

### Runtime ownership and readiness

A coding launch owns its runtime. OCG starts a private loopback OpenCode 2
server for one invocation (`opencode serve --hostname 127.0.0.1 --port 0`) with
the generated config passed through `OPENCODE_CONFIG_CONTENT`, and terminates it
when the invocation ends. It never attaches a launch to an ambient background
service, because such a server carries a catalogue and configuration OCG cannot
reason about.

Readiness is a real API probe, not a port check: after reading the startup
handshake (`server listening on <url>`, `server password ...`), OCG polls the
API until an authenticated request succeeds. The probe is bounded — a first
immediate attempt, then up to 150 attempts 100 ms apart — so an already-ready
runtime costs one request and no sleep, and a runtime that never becomes ready
fails with a distinct error instead of hanging. The raw handshake is necessary
but not sufficient: the socket may not be bound, the process may have exited, or
the credentials may be rejected, and each of those is reported separately from a
runtime that is still starting.

## Effective runtime state

Configured, Resolved and Effective are three distinct states, and OCG never
fabricates one from another.

| State | Meaning |
| --- | --- |
| Configured | What the layered YAML requests: the active level's Lead agent, provider, model and (optional) variant. |
| Resolved | What OCG's own static validation accepts, plus runtime catalogue evidence that the requested provider/model is exposed. |
| Effective | What a live runtime session reports after OCG activates the resolved contract on it. |

`ocg status --effective` and `ocg doctor --effective` print all three from a live
observation. The Effective line is not derived from YAML: OCG starts the owned
server, activates the resolved Lead on a session, and reads the session back, so
the reported agent/provider/model/variant is what the executing runtime actually
holds. The private server is terminated before the command returns. Without
`--effective`, both commands stay read-only and fast.

The states stay distinct when they disagree:

| Situation | Reported as |
| --- | --- |
| Catalogue does not expose the provider | missing provider (failure) |
| Provider exposed, model absent | missing model (failure) |
| Catalogue probe could not run | not checked (warning, never "missing") |
| No session-level runtime could be observed | not observed (info, never "verified") |
| Runtime reached, activation/read-back failed | could not be verified (failure) |
| Session reports a different agent/model/variant | contradiction (failure) |

The OpenCode 2 session API accepts any provider/model id without validating it
(confirmed against a live 2.0.11 server); the session read-back therefore proves
*intent* while the catalogue probe proves *availability*, and OCG records both.
A configured model with no declared variant is satisfied by any reported variant
(the provider default, rendered as `provider-default`); a declared variant must
be observed exactly.

## Context policy

The optional local context engine is configured by the top-level `context`
object. Every field is optional and the defaults are conservative; no user
configuration is required.

```yaml
context:
  enabled: true
  cache: true
  maxFileBytes: 1000000
  maxRepositoryFiles: 100000
  maxCandidates: 200
  maxFiles: 24
  maxSlices: 48
  maxBytes: 262144
  maxDiffBytes: 131072
  maxHunks: 40
  maxSymbolsPerFile: 200
  includeUntracked: true
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

An ordinary `ocg` / `ocg run` launch materializes the generated plugin when
orchestration is enabled; it does not index until a context request is handled.
Repository context and index state may then be built or updated by explicit
`ocg context <task>` commands, the library API, or ordinary orchestration bridge
context requests (see [Orchestration policy](#orchestration-policy)). Cached
plans are only returned after their source slices, dependency fingerprints and
git identity are re-verified, and a failed cache write leaves the computed plan
intact with a warning note.

Token counts in plans and capsules are always **estimates** (`bytes / 4`) and
are labelled as such.

## Verification policy

The optional verification subsystem is configured by the top-level
`verification` object. Every field is optional. Stages start empty, so a
manifest existing never causes a command to run.

```yaml
verification:
  enabled: true
  defaultStage: normal
  stopOnFailure: true
  maxRawLogBytes: 2000000
  maxLogStorageBytes: 52428800
  includeTestProposal: true
  stages:
    fast:
      commands: [cargo fmt --check]
    normal:
      commands: [cargo check, cargo test]
    full:
      commands: []
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

```yaml
capabilities:
  enabled: true
  custom: [warehouse]
```

With `enabled: false` the planner is disabled end-to-end: the plan carries
`enabled = false` with no allowed capability, all built-ins and custom names are
denied, and `ocg tools` / the context plan report the disabled state.

Capability planning is a context/config diagnostic, not a security sandbox; see
[verification.md](verification.md).

## Telemetry policy

The optional top-level `telemetry` object controls local event collection:

```yaml
telemetry:
  enabled: true
  localOnly: true
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

```yaml
orchestration:
  enabled: true
  maxBuildRetries: 2
  maxDebugRetries: 1
  maxHandoffBytes: 16384
  maxHandoffRatioPercent: 60
  contextGovernor:
    enabled: true
    approachingPercent: 70
    rolloverPercent: 80
    # Optional safety budget when a model limit is unavailable.
    # absoluteCapTokens: 120000
    normal: continue
    approaching: warn
    rolloverRequired: rollover
    unknown: warn
    maxContinuationBytes: 16384
    retryCooldownSeconds: 60
```

| Field | Default | Bounds | Meaning |
| --- | --- | --- | --- |
| `enabled` | `true` | boolean | Generate and inject the adapter; run the controller. Disabled emits no plugin and writes no state. |
| `maxBuildRetries` | `2` | `0`–`10` | Build retries after a failed verification, before Debug is recommended. |
| `maxDebugRetries` | `1` | `0`–`5` | Debug hand-offs before the controller reports escalation. |
| `maxHandoffBytes` | `16384` | `512`–`65536` | Absolute cap on a projected hand-off capsule. |
| `maxHandoffRatioPercent` | `60` | `1`–`100` | Hand-off cap as a percentage of the rich source context. |
| `contextGovernor.enabled` | `true` | boolean | Enable bounded context telemetry and the same-Mission rollover coordinator. `false` is inert. |
| `contextGovernor.approachingPercent` | `70` | `1`–`100` | Warning threshold; must be below `rolloverPercent`. |
| `contextGovernor.rolloverPercent` | `80` | greater than approaching, at most `100` | Rollover-request threshold. It is only actionable at a verified safe boundary. |
| `contextGovernor.absoluteCapTokens` | unset | positive integer | Optional internal cap used when the runtime has no trustworthy model limit; it is not a provider-limit claim. |
| `contextGovernor.normal` | `continue` | `continue`, `warn`, `rollover` | Action in the normal band; an explicit `rollover` is honored conservatively at a safe boundary. |
| `contextGovernor.approaching` | `warn` | `continue`, `warn`, `rollover` | Action in the approaching band; an explicit `rollover` is honored conservatively at a safe boundary. |
| `contextGovernor.rolloverRequired` | `rollover` | `continue`, `warn`, `rollover` | Action after the rollover threshold. `rollover` requests replacement; it never fires on unknown telemetry. |
| `contextGovernor.unknown` | `warn` | `continue`, `warn` | Diagnostic action when usage, model limits or the runtime query is unavailable. Rollover is rejected. |
| `contextGovernor.maxContinuationBytes` | `16384` | `1`–`1048576` | Hard cap for a Mission-derived continuation packet. |
| `contextGovernor.retryCooldownSeconds` | `60` | non-negative integer | Cooldown after a failed rollover attempt; the old owner remains authoritative. |

`maxHandoffBytes` / `maxHandoffRatioPercent` are a **runtime size envelope**,
not a correctness rule: required evidence (goal, hard constraints, critical
findings, changed files, failing locations) is never dropped to satisfy them,
and an overage is recorded in the capsule's `omitted` notes. The deterministic
release fixture configures a much tighter `4096` / `40%` gate explicitly to
catch projection regressions; that gate is not the default.

The context governor is intentionally conservative. V2 token telemetry is
read from one completed message and projects `input + cache.read` for that
message; it never sums a transcript, counts `cache.write` as active input, or
invents a percentage when `/api/model` did not report a real limit. A failed
context/session/model query is recorded as `unknown` and cannot request a
rollover. An unsafe boundary records a pending request only. A safe rollover
creates a fresh V2 session, verifies its Lead and session identity, stages a
bounded Mission-derived continuation, and commits the replacement with a
Mission revision/owner CAS. The old session is retained. Credentials, service
URLs and provider data are invocation-scoped and never written to these
artifacts. A disconnected TUI, bridge or OpenCode client is an execution-messenger
failure, not a Mission failure: the durable Mission remains recoverable and
can be resumed by a replacement session.

`OPENCODE_GEAR_ORCHESTRATION=0` (legacy `OC_GEAR_ORCHESTRATION`) force-disables
orchestration for one process. It is also the explicit no-hook path: no plugin
is emitted, so Lead request enforcement is intentionally disabled as well.
`ocg doctor` reports the read-only state of the plugin, state file, projection
and verification integration. See
[architecture.md](architecture.md) for the mechanism and
[verification.md](verification.md) for the retry/Debug contract.

The bridge injects the full repository context snapshot on the first Lead
prompt of a session and records a deterministic `snapshot_id` per session. On
OpenCode v1 the snapshot is persisted into the conversation history, so if the
effective repository snapshot is unchanged, later prompts in the same session
receive an empty `context` with `cached: true` and the snapshot is not
duplicated across turns. On OpenCode v2 the snapshot is pushed onto the
outgoing request's system context at every root-Lead model dispatch and never
persisted, so every dispatch receives the full baseline; `cached: true` then
reports that the retained rendering was reused, and only a material repository
change re-renders it. The response also carries estimate-only metadata
(`estimated_tokens` = bytes / 4, `bytes`, `file_count`, `symbol_count`);
estimated tokens are never presented as exact provider billing tokens. On the
v1 adapter the metadata is rendered as a plain, compact line in the appended
context block; the v2 adapter injects the baseline without a presentation
header.

## Reports policy

The optional top-level `reports` object controls local artifacts written from
a running session. It is deliberately tiny — there is no report subsystem:

```yaml
reports:
  latestLeadOutput:
    enabled: true
```

| Field | Default | Meaning |
| --- | --- | --- |
| `latestLeadOutput.enabled` | `true` | Persist the raw user-visible text of the latest completed root Lead response to `<project>/.opencode-gear/reports/latest-lead-output.md`. |

The file is written byte-verbatim: no headers, timestamps, ids, metadata,
summaries or front matter. Only a *completed* root Lead assistant message is
written; streaming partials, errored/interrupted responses, worker sessions
(`ocg-*`) and non-OCG agents are never written, and an interrupted response
never replaces the last completed output. The replacement is atomic (temp file
+ rename) and every failure is soft — a broken report cannot break a session.

The capture is implemented by the generated OpenCode 2 adapter, so it is
active exactly when that adapter is (orchestration enabled, the default) and
the switch is on. OpenCode 1 has no equivalent event stream: there the switch
is inert and no file is produced. `.opencode-gear/` is already ignored, so the
artifact is never committed by accident.

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
| `OPENCODE_GEAR_DISABLE_PROXY` | `1`/`true`/`on`/`yes` to disable proxy use for one process |
| `HTTP_PROXY`, `http_proxy`, `HTTPS_PROXY`, `https_proxy`, `ALL_PROXY`, `all_proxy`, `NO_PROXY`, `no_proxy` | standard proxy variables; used when no CLI disable and no truthy `OPENCODE_GEAR_DISABLE_PROXY` |
| `GH_TOKEN` (preferred), `GITHUB_TOKEN` | optional token for OCG-owned GitHub API requests; attached only to `api.github.com` |

`OPENCODE_GEAR_OCG` and `OPENCODE_GEAR_PROJECT` are exported *to OpenCode* by
`ocg` at launch so the generated adapter can find the bridge and the project;
they are not read from the ambient environment. For an OpenCode 2 coding launch,
`OPENCODE_GEAR_V2_SERVER_URL`, `OPENCODE_GEAR_V2_SERVER_PASSWORD`,
`OPENCODE_GEAR_V2_TARGET_SESSION` and `OPENCODE_GEAR_V2_DIRECTORY` are also
exported only to that invocation's child process. The bridge uses them to query
the invocation-owned V2 API and verify the selected target; they are never
configuration defaults, telemetry, logs or artifacts. The generated adapter
also receives the resolved `OPENCODE_GEAR_CONTEXT_GOVERNOR_ENABLED` switch so a
disabled governor remains inert even when output reporting is separately
enabled. `OPENCODE_SERVER_PASSWORD` remains the runtime's own short-lived child
credential.

## Proxy policy

The effective proxy is resolved once per network-using command:

```text
1. --disable-proxy
2. OPENCODE_GEAR_DISABLE_PROXY (truthy)
3. a non-empty proxy variable, upper- or lower-case
4. static macOS discovery (/usr/sbin/scutil --proxy)
5. direct
```

Only `http://` and `https://` proxies are used by OCG's own client; an unusable
value is ignored with a warning. A `socks*://` value is not interpreted (OCG
does not enable SOCKS), but it is preserved verbatim for child processes so a
working SOCKS setup keeps working. A PAC configuration is detected and reported
but never interpreted. Static macOS discovery parses `HTTPProxy` /
`HTTPSProxy` and a safe `ExceptionsList`; it runs only when the environment
carries nothing.

Proxy URLs and any credentials are never rendered in `Debug`, errors or
diagnostics. `reqwest`'s hidden automatic discovery is always disabled before
the typed resolved endpoints are installed. A child OpenCode process (launch,
`upgrade`, `models`) receives the resolved values under both the upper- and
lower-case names after all eight spellings are cleared, so a client that reads
either convention sees the resolved policy; `--disable-proxy` and the
environment switch clear all eight and set none.

`ocg upgrade` additionally reads `GH_TOKEN` before `GITHUB_TOKEN` for its own
GitHub API requests and never attaches it to a non-`api.github.com` host.

## Commands

```text
ocg [low|mid|high] [--throttle LEVEL] [--project DIR] [--dry-run] [--pretty] [--disable-proxy] [--effective] [command] [args...]

ocg build    [--pretty] [--throttle LEVEL] [--project DIR]
ocg validate [--throttle LEVEL] [--project DIR]
ocg routing  [--project DIR]
ocg status   [--throttle LEVEL] [--project DIR] [--effective]
ocg config   lead [level] --model PROVIDER/MODEL [--variant V] [--scope user|project] --yes [--no-activate]
ocg throttle [LEVEL] [--project DIR]
ocg layers   [--project DIR]
ocg init     [--project DIR]
ocg trace    --event launch [--project DIR]
ocg context  <task...> [--pretty] [--project DIR]
ocg context  symbols <query> [--project DIR]
ocg cache    stats|clean [--project DIR]
ocg stats    [--pretty] [--project DIR]
ocg verify   [fast|normal|full] [--pretty] [--project DIR]
ocg tools    <task...> [--pretty] [--project DIR]
ocg checkpoint list|show <id>|save --phase P [--task T] [--decision D] [--pretty] [--project DIR]
ocg version  (read-only runtime report)
ocg doctor   (read-only environment check; --effective adds the live runtime state)
ocg upgrade  (self-update Gear, then maintain OpenCode)
```

`build` (and `--dry-run`) prints the merged OpenCode config consumed through
`OPENCODE_CONFIG_CONTENT`. Both follow the detected runtime family, so their
output matches what a real launch on that runtime would use (read-only
resolution; an absent runtime keeps the deterministic v1 contract). `validate`
exits non-zero on any configuration
error and prints every problem it finds; it is a static check and exercises the
runtime-independent v1 builder without probing any runtime.

`ocg config lead` writes a semantic override atomically and then, by default,
activates and verifies it: it starts an owned private runtime, activates the
resolved Lead on a session, reads the effective provider/model/variant back and
prints it (`effective: verified on <endpoint> (session <id>)`). `--no-activate`
skips that step. The commit point is the atomic write, so a failed activation
keeps the written change and reports `effective: NOT VERIFIED` or `not observed`
rather than leaving a half-switched state. See
[Effective runtime state](#effective-runtime-state).

## Observability

Off by default. When enabled, `ocg` appends one JSON line per launch to a local
file:

```json
{"ts":"2026-01-01T00:00:00+00:00","event":"launch","throttle":"low",
 "default_agent":"lead-low","lead":"openai/gpt-5.6-sol",
 "routing":{"explore":"volcengine-coding-plan/kimi-k2.7-code","build":"opencode-go/deepseek-v4.1-flash"}}
```

- Local file only. No remote telemetry, no network calls.
- No prompts, no source code, no credentials.
- Captures routing decisions only. Success/failure, retry counts and duration
  require a session-level hook that this project does not ship.

Delete the file to delete the history. Disable by removing the
`observability.enabled` flag.

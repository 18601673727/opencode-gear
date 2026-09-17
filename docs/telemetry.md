# Local telemetry, statistics and privacy

OpenCode Gear can record a small structured event for two local flows:
`ocg context` and `ocg verify`. This document describes the schema, the privacy
model, how to read `ocg stats`, how `ocg doctor` checks the store and what is
deliberately deferred.

## Boundaries

- **Local only.** Events are appended to a JSONL file inside the project. There
  is no remote service, no upload, no model API and no network path in the
  telemetry code.
- **No prompts, source code, command strings, command output, headers,
  environment dumps or absolute personal paths.** The schema has no field for
  them, and free-text metadata is redacted before it is written.
- **Fail soft.** A damaged, unwritable or disabled store warns and never blocks
  context preparation, verification or an ordinary launch.
- **No decisions.** The schema is wide enough to become the input of a future
  budget controller or capability router, but nothing reads it to make a
  decision, score a model or route a request.

## State locations

| Path (relative to the project) | Contents | Owner |
| --- | --- | --- |
| `.opencode-gear/telemetry/events.jsonl` | local telemetry events, one JSON object per line | `telemetry` module |
| `.opencode-gear/index/context-index.json` | incremental symbol index | `context` module |
| `.opencode-gear/cache/context/*.json` | dependency-checked context cache | `context` module |
| `.opencode-gear/logs/*.log` | raw verification logs (bounded and pruned) | `verification` module |
| `.opencode-gear/checkpoints/*.json` | phase checkpoints | `orchestration` module |
| `.opencode-gear/runtime/opencode/...` | managed OpenCode runtime | `runtime` module |

Creating any of this state adds `.opencode-gear/` to the project `.gitignore`
once, using the same idempotent helper the runtime uses. The directory is
project-local; it is never the platform remote cache used for update checks.

## Event schema

Each line is an object with `schema_version: 1`:

| Field | Meaning |
| --- | --- |
| `schema_version` | telemetry schema version (currently `1`) |
| `timestamp` | Unix seconds |
| `task_id` | a caller-supplied safe id or a deterministic SHA-256 prefix (`task-<hex>`), never a raw prompt |
| `session_id` | optional caller-supplied id; absent when unknown |
| `task_type` | `context` or `verification` |
| `role` | routing role when known, otherwise absent |
| `provider`, `model` | provider/model when known, otherwise absent |
| `input_tokens`, `output_tokens` | a [`TokenCount`](#token-provenance) whose source distinguishes exact, estimated and unavailable values |
| `duration_ms` | wall duration of the recorded flow |
| `context` | `candidate_bytes`, `selected_bytes`, `capsule_bytes`, `reduction_bytes` |
| `repo` | `files`, `symbols`, `index_reused`, `index_updated`, `cache_hit` |
| `verification` | `enabled`, `ran`, `attempts`, `passed`, `failed`, `not_run`, `targeted_candidates`, `stage` |
| `logs` | `raw_bytes`, `distilled_bytes`, `reduction_bytes` |
| `capabilities` | selected capability names only (no arguments, no tool output) |
| `outcome` | `success`, `failure` or `unknown` |

`cache_hit` is `null` (absent) for a flow that did not look at the context
cache, so "not applicable" is never counted as a cache miss.

## Token provenance

`TokenCount` is `{ total, source }` where `source` is one of:

| Source | Meaning | Shown by `ocg stats` as |
| --- | --- | --- |
| `provider_reported` | a number reported by the model provider | `<n> (exact)` |
| `opencode_reported` | a number reported by OpenCode's session accounting | `<n> (exact)` |
| `estimated` | a deterministic estimate (context bytes / 4) | `<n> (estimate only)` |
| `unknown` | no number is available; `total` is absent | `- (unavailable)` |

An estimate is never presented as exact. Provider- and OpenCode-reported counts
are supported by the typed API and tested, but no local flow can supply them yet:
OpenCode Gear has no OpenCode session hook, so those sources remain unavailable
until a future integration provides them. `ocg context` records an **estimated**
input count for the selected context; `ocg verify` records unknown tokens.

## What `ocg context` records

- candidate, selected and capsule byte sizes, plus the candidate/selected
  reduction;
- the estimated token count for the selected context, labelled `estimated`;
- repo files and symbols, index entries reused and updated, and the cache
  hit/miss;
- the selected capability names;
- the wall duration;
- a hashed task id. **The task text is never stored.**

## What `ocg verify` records

- attempt counts (`attempts`, `passed`, `failed`, `not_run`);
- the targeted-test candidate count when a proposal exists;
- raw and distilled byte sizes and their reduction;
- the deterministic outcome (`success`, `failure` or `unknown` for a stage that
  did not run);
- the wall duration and stage name.
- **Command strings and captured output are never stored.**

## Privacy enforcement

1. The schema has no field for prompts, code, command strings, output, headers,
   environment variables or absolute paths.
2. A task id is either verified safe (bounded, `[A-Za-z0-9._-]`) or replaced by a
   deterministic hash of the input.
3. Every free-text metadata field (`session_id`, `task_type`, `role`,
   `provider`, `model`, capability names, `verification.stage`) is passed through
   a redactor. A value shaped like an API key, private key, `Authorization` /
   `Bearer` header or `key=value` credential becomes `[redacted]`.
4. After serialization, the store re-checks the whole line and refuses to write
   one that still looks secret-shaped.

Tests exercise all four: a fake key assembled at runtime must never appear in
the raw JSONL, and raw prompt text must never become part of a task id.

## Configuration and disabling

The policy is a small top-level object:

```json
{ "telemetry": { "enabled": true, "localOnly": true } }
```

- Both fields are optional; the defaults are `enabled: true` and
  `localOnly: true`.
- `localOnly: false` is **rejected**: there is no remote mode to opt into.
  `ocg validate` reports it and the recording flows disable telemetry with a
  warning instead of failing.
- `OPENCODE_GEAR_TELEMETRY=0` (also `false`, `off`, `no`) disables collection;
  `1`/`true`/`on`/`yes` enables it. Any other value leaves the config alone.
  The legacy `OC_GEAR_TELEMETRY` alias is accepted.
- A disabled store writes nothing and creates no `.opencode-gear/` directory.

Example per-project disable file (`.opencode-gear.json`):

```json
{ "telemetry": { "enabled": false } }
```

## Corruption recovery

- Reading skips a line that is not valid JSON or not a valid event and counts
  it; a corrupt line never aborts the read.
- A valid JSON line with a newer, unsupported `schema_version` is counted
  separately and ignored.
- A crash that leaves a partial final line only affects that line.
- Corruption never blocks a later append, `ocg stats`, `ocg doctor` or a normal
  `ocg` command.

## `ocg stats`

`ocg stats` is offline and read-only. It never touches the network and never
creates state: a missing telemetry directory is reported as "no data". It shows
the policy (enabled / local-only / path), the event count, corrupt-line count,
the latest event summary, and a stable project aggregate.

Example with one context event and one verification event:

```text
OpenCode Gear telemetry (local only)
  enabled:     yes
  local-only:  yes
  path:        <project>/.opencode-gear/telemetry/events.jsonl
  file:        present
  events:      2
  corrupt:     0 line(s) skipped
  bytes:       1234
  window:      1789632000 .. 1789632090 (unix seconds)

latest event:
  time:        1789632090 (unix seconds)
  task:        task-49f1b7f9045a9c75
  session:     -
  type:        verification
  role:        -
  provider:    -
  model:       -
  duration:    4 ms
  outcome:     failure

tokens (source labelled; estimates are not exact):
  input:
    provider_reported: - (unavailable)
    opencode_reported: - (unavailable)
    estimated:         511 (estimate only)
    unknown:           1 event(s)
  output:
    provider_reported: - (unavailable)
    opencode_reported: - (unavailable)
    estimated:         - (unavailable)
    unknown:           2 event(s)

context (bytes):
  candidate:   12729
  selected:    2044
  capsule:     13250
  reduction:   10685 (83.9%)

repo / index / cache:
  files:       120
  symbols:     480
  index reused:  0
  index updated: 120
  cache hits:  0
  cache misses: 1

verification:
  attempts:    1
  passed:      0
  failed:      1
  not run:     0
  targeted candidates: 0

logs (bytes):
  raw:         14399
  distilled:   666
  reduction:   13733 (95.4%)

outcomes:
  success:     1
  failure:     1
  unknown:     0

capabilities (1):
  filesystem: 1
```

`ocg stats --pretty` emits the same information as JSON with a stable field
order.

## `ocg doctor`

`ocg doctor` is strictly read-only. It inspects existing artifacts and never
builds an index, runs a test suite, upgrades the runtime or creates
`.opencode-gear/`. Relevant lines:

- `repository map/index` — file/symbol counts, or `info` when not built, or a
  `warn` when the file is unreadable/corrupt;
- `symbol index` — symbol count from the same index;
- `context cache` — entry, byte and corrupt counts (read-only statistics);
- `task checkpoints` — readable checkpoint count, or a `warn` with the corrupt
  count;
- `verification config` — enabled/disabled, default stage and configured command
  count;
- `telemetry` — enabled/disabled, local-only, path, event count and skipped
  corrupt lines;
- `tool capability planner` — enabled state and custom capability count;
- `sensitive-file exclusions` — excluded path count in the current index.

A missing index, cache, checkpoint or telemetry store is `info`, never a
failure. Corruption produces a warning, never a secret and never a non-zero
exit solely for being corrupt.

## Cleanup

| Command | Removes | Keeps |
| --- | --- | --- |
| `ocg cache clean` | `.opencode-gear/cache/` only | index, logs, checkpoints, telemetry, runtime |
| delete `.opencode-gear/logs/` | raw verification logs | everything else |
| delete `.opencode-gear/telemetry/` | telemetry history | everything else |
| delete `.opencode-gear/` | all local state | nothing (it is regenerated) |

Raw verification logs are bounded by `verification.maxRawLogBytes` per stream
and pruned to `verification.maxLogStorageBytes` in total; the log referenced by
the current report is never pruned away. See
[verification.md](verification.md#raw-log-privacy-and-cleanup).

## Sensitive-file exclusions

The classifier in `src/context/classify.rs` marks `.env`/`.env.*`, `.envrc`,
`.netrc`, `.npmrc`, `.pypirc`, `id_rsa`/`id_ed25519` and other key material,
`credentials*`, `secrets*`, `auth*`, `token*`, `password*` and known credential
stores (`.ssh`, `.aws`, `.gnupg`, `.kube`, `.docker`, OpenCode's own
`auth.json` locations, ...) as sensitive **by path**. A sensitive file is never
read, parsed, sliced, cached, indexed for symbols or used as a context slice;
only its path metadata (size, language) may enter the repo map. When in doubt,
the engine excludes. `ocg doctor` reports how many paths the current index
excluded.

## OpenCode execution ownership

OpenCode Gear owns configuration, context preparation, verification execution
and local telemetry. **OpenCode owns the model session**: provider and model
selection, conversation state, tool invocation and token accounting. Gear does
not wrap, proxy or inspect a model call, which is exactly why provider- and
OpenCode-reported token counts are unavailable locally today.

## Tool Firewall limitation

The capability plan and the "Tool Context Firewall" view
(`src/capabilities.rs`, `ocg tools`) are **planning and diagnostics only**. They
describe which capability groups a task plausibly needs so the context planner
and CLI can state an explicit boundary. They do not activate a runtime tool
schema, grant or deny a provider permission, or block a real call; enforcement
remains OpenCode's job. `ocg doctor` reports the planner's enabled state, not an
enforcement guarantee.

## Deferred future boundaries

The following are **not implemented** and make no decisions or scores from the
recorded data:

- a Budget Controller;
- adaptive fan-out;
- a model capability router;
- provider health tracking;
- historical model success;
- failure/decision memory;
- capability-per-dollar evaluation.

The schema records the inputs these features would need (bytes, token
provenance, capability names, deterministic outcomes, provider/model when known)
but deliberately stops there. No scoring, ranking or purchasing decision is
derived from telemetry.

## Relation to the older observability trace

`observability` / `OPENCODE_GEAR_TRACE` is the older, optional launch-time
routing trace enabled by `"observability": {"enabled": true, "path": "..."}`.
It remains separate: it records routing decisions at launch and is disabled by
default. Local telemetry is about the measured local flows (`context` and
`verify`) and is enabled by default. Neither uploads anything.

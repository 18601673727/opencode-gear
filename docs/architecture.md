# Architecture

OpenCode Gear is deliberately small. This document explains the moving parts
and, more importantly, the invariants that keep the model sane.

`ocg` is a single self-contained Rust binary. It embeds the shipped registries
and prompts, resolves the layered configuration in memory, and launches
`opencode` with the result.

## The two axes

Most multi-model OpenCode setups start with a single word — "profile" or
"gear" — that means *the whole bundle of models*. That makes two independent
questions impossible to answer separately:

1. How much should the orchestrating model think?
2. Which specialist should do the delegated work?

OpenCode Gear splits them:

```text
Throttle  ──▶ Lead quality / reasoning / cost
Router    ──▶ delegated execution routing
```

The axes are independent:

- Changing the throttle must not move BUILD or VERIFY to another model.
- Changing routing must not change the Lead.

Everything else in the repository exists to keep that true.

## Resolution pipeline

```text
embedded defaults       config/*.yaml + config/prompts/*.md (compiled in)
config/base.yaml        shared OpenCode config (providers, disabled built-ins)
config/models.yaml      model key -> provider + real model id (+ variants)
config/throttle.yaml    level -> Lead model key + reasoning variant
config/routing.yaml     role  -> model key + reasoning variant
config/permissions.yaml isolation profiles and role -> profile mapping
config/prompts/*.md     one prompt per role
        │
        ▼
src/config.rs           merge defaults → user → project → CLI/env
        │
        ▼
generated OpenCode config
  agent.lead-low / lead-mid / lead-high   (mode: primary)
  agent.ocg-explore / ocg-explore-deep / ocg-build /
        ocg-verify / ocg-debug / ocg-docs  (mode: subagent, hidden)
  model / small_model / default_agent / enabled_providers
        │
        ▼
OPENCODE_CONFIG_CONTENT  →  opencode
```

The config pipeline never writes to the repository. Its only persisted state
is the default throttle level in the user config, and only via
`ocg throttle <level>`. The runtime layer adds an optional project-local
runtime tree and an update-check cache; see [Managed runtime](#managed-runtime)
below.

For development and tests, or to load a different set of defaults, set
`OPENCODE_GEAR_HOME` (legacy `OC_GEAR_HOME`) to a directory containing `config/`.
Otherwise the compiled-in defaults are used, which is what makes a released
binary self-contained.

## Managed runtime

The config pipeline is pure: it never needs a process. The runtime layer is
the opposite, so it is isolated behind three injected traits:

```text
HttpTransport   release metadata + archive downloads (reqwest blocking/rustls)
Clock           update-check timestamps and install times
ProcessHost     PATH lookup, version/model probes, and `opencode upgrade <target>`
```

The HTTP client and every child process receive one `ProxySelection`, resolved
from `--disable-proxy`, `OPENCODE_GEAR_DISABLE_PROXY`, the standard proxy
variables, and static macOS discovery (in that order). Hidden automatic proxy
discovery is always disabled first; only typed `http`/`https` endpoints are
installed. A GitHub token (`GH_TOKEN` then `GITHUB_TOKEN`) is attached only to
`api.github.com` requests and never rendered.

`RuntimeManager` combines those with the project root and the parsed `runtime`
policy. It resolves exactly one executable per launch:

```text
explicit OPENCODE_GEAR_OPENCODE
  -> existing managed project runtime
  -> compatible system opencode on PATH
  -> project-local bootstrap install
```

Invariants:

- **One floor.** `OPENCODE_CONFIG_CONTENT` requires OpenCode `>= 1.18.0`; the
  constant lives only in `runtime::policy`.
- **Explicit is authoritative.** A broken explicit executable errors; it never
  falls back to a managed or system runtime.
- **Managed beats system.** Once a project is bootstrapped it stays
  deterministic.
- **No redundant install.** A compatible system runtime is used as-is when no
  managed runtime exists. A due check runs OpenCode's own `opencode upgrade`
  in place, never a managed copy.
- **System runtimes upgrade themselves.** OCG resolves the latest OpenCode
  release once through its own transport and then runs
  `opencode upgrade <target>`; it never downloads a release archive for a
  system runtime. If the lookup or the upgrade fails, a compatible system
  runtime is kept and the failed check (including a rate-limited lookup) is
  cached. If the system is incompatible and the upgrade fails, it bootstraps a
  project-local runtime.
- **`autoUpgrade` is optional-only.** Setting it to `false` disables the
  optional system upgrades but never disables the required project-local
  fallback for a missing or incompatible runtime.
- **Pins are exact.** A `runtime.version` pin is never advanced, is managed
  only (no system upgrade), and `ocg upgrade` preserves it.
- **Checks are cached.** Successful *and* failed checks are cached for
  `checkIntervalHours` in the platform cache directory, so launches do not
  network (or re-upgrade) every time. `ocg upgrade` forces a check.
- **Installs are atomic and fail closed.** Archives are staged in a sibling
  directory, verified against the API `sha256:` digest (a missing digest is
  refused), then moved into place; `active.json` is written atomically. The
  recorded path is never trusted: the binary is re-derived from the semver and
  must be executable.
- **Self-update is safe.** `ocg upgrade` downloads the Gear binary plus
  `SHA256SUMS`, verifies the checksum, runs the staged binary's `version` and
  requires it to match the release, then renames it over the running
  executable. Any failure leaves the installed CLI untouched.

The update checks and self-update are the only network paths, and both are
bypassed by `ocg version` and `ocg doctor`, which are strictly read-only.

## OpenCode runtime compatibility

Gear supports two structurally different OpenCode major families. Every
version-specific decision is isolated behind one narrow boundary:

```text
runtime/common contract
├── v1 adapter   OpenCode 1.18.x
└── v2 adapter   OpenCode 2.x (verified against 2.0.11)
```

- **Detection is explicit.** A launch classifies the resolved runtime's
  `--version` into a major before it generates config. A parseable but
  unsupported major (for example `3.x`, or a `1.x` below `1.18.0`) is a launch
  failure. A runtime whose version genuinely cannot be probed keeps the
  historical v1 contract, so the supported 1.18.x path is never regressed.
- **One boundary, no scattered conditionals.** Unrelated modules never branch
  on the major version. They ask the adapter for the package-plugin key, the
  delegation tool/permission key (`task` vs `subagent`), local-plugin discovery,
  the Lead-selection mode and the launch mode.
- **V1 stays request-scoped.** The Rust-resolved Lead contract is enforced on
  the mutable `chat.message` request, and the runtime catalogue probe proves
  the active Lead model exists before launch.
- **V2 is session-scoped and private per invocation.** For the verified 2.0.11
  contract, Gear starts `opencode serve --hostname 127.0.0.1 --port 0` with the
  generated inline config and its local plugin directory. It reads the
  child-only URL/password startup output, connects the production SessionClient
  to that same child, resolves or creates the project session, selects the Lead
  agent/model (sending a variant only when configured), then reads the effective
  state back and fails on a mismatch. The OpenCode client is pointed at that
  same private server with the verified `--session` target and the child is
  terminated and reaped when it exits; Gear never restarts or reconfigures the
  user's shared service. The V2 URL, local password and target session are
  invocation-scoped child environment values only; they never enter Mission,
  telemetry, continuation or rollover artifacts.
- **Runtime ownership and readiness are explicit.** A V2 launch only ever uses
  the server it started for that invocation; its identity is reported as
  `ocg-managed-invocation (pid N)` without the endpoint or password, never as
  an ambient service. Startup reads the handshake and then waits for a real
  authenticated API response, bounded by a fixed budget, so a bound-but-unready
  or dead process fails distinctly instead of hanging.
- **Configured / Resolved / Effective are never conflated.** The effective state
  comes from a live session read-back, but the OpenCode 2 session API accepts
  any provider/model id, so that read-back proves *intent*; the catalogue probe
  proves *availability*. Gear records both and reports a missing provider, a
  missing model, an unrun probe, an unobserved runtime and a contradiction as
  distinct outcomes.
- **V2 config and plugins are owned together.** The 2.0.11 singular `provider`,
  `agent`, and package `plugin` surfaces, plus `enabled_providers` and
  `small_model`, are preserved. The generated local adapter is discovered from
  `OPENCODE_CONFIG_DIR/plugins`, rather than injected as a file URI. The config
  dir is a dedicated `.opencode-gear/orchestration/v2-config` root, isolated
  from the V1 `plugin/` state, so a V2 runtime can never discover a stale V1
  adapter; legacy generated artifacts from earlier layouts are migrated away
  on materialization. A missing
  variant is provider-default, never a serialized `"provider-default"`; an
  overlay changing a model clears an inherited variant while an unchanged model
  retains it.
- **The plugin adapter stays thin.** The generated adapter only transports
  bytes to and from `ocg __bridge`; it performs no ranking, projection or
  policy, and every bridge failure is swallowed so a broken bridge cannot
  destroy a session. A missing `tool.execute.after` delivery is ignored.
- **No credentials are read.** Gear never reads or writes OpenCode credential
  files or `auth.json`.

## Repository context

The context engine is the optional, local half of the runtime layer. It answers
"what should the agent look at?" without a model:

```text
repo map        dirs, source/test roots, languages, manifests, entrypoints,
                migrations, git state, per-file metadata
incremental     .opencode-gear/index/context-index.json
symbol index    Rust / TypeScript / JavaScript / Python symbols + line ranges
git diff        status entries (never omitted) + bounded hunks + changed symbols
ranking         integer, transparent task/path/symbol/import/changed/test scores
cache           .opencode-gear/cache/context/*.json, dependency-fingerprinted
capsules        versioned serde TaskCapsule for a later session
```

Invariants worth preserving:

- **Local and deterministic.** No network, no remote telemetry, no embeddings. Ordering
  is explicit (path, line, kind) and ranking uses integer scores, so the same
  tree always yields the same plan.
- **Sensitive content never enters an artifact.** `.env`, key material,
  `id_rsa`/`id_ed25519`, `credentials*`, `secrets*`, `auth*`, `token*` and known
  OpenCode credential locations are classified by path and never read, sliced or
  cached. When in doubt the engine excludes.
- **Git is optional.** Without git, fingerprints fall back to SHA-256 of file
  content and the diff is empty; nothing errors. With git, clean tracked files
  use blob ids so an unchanged file is never re-read or re-parsed.
- **Changed paths are never dropped.** The diff capture is bounded before it can
  allocate an arbitrarily large stdout (spawn + capped read + kill), and hunk
  bodies are separately bounded; an oversize diff adds a structural summary and
  keeps the full path list. Deleted paths are reported as unsourced and their
  symbols are never guessed.
- **The file walk is capped.** `context.maxRepositoryFiles` stops the
  deterministic walk; the repo map, index and plan are marked truncated and
  carry a note, while git status is collected separately so changed paths stay
  complete. A capped map is never presented as complete.
- **Git state is part of cache identity.** The cache key covers HEAD, branch and
  the complete sorted status entries, so a commit or status change can never
  serve a stale plan. Fine-grained source fingerprints still invalidate only
  entries that depend on the changed files.
- **Cached content is re-verified, never trusted.** Before a cached plan is
  returned, every slice is compared with the current non-sensitive source lines,
  selected paths must still be non-sensitive, and dependency fingerprints and
  git identity must match. A poisoned entry is discarded.
- **Fail-soft.** A failed cache write only warns and keeps the computed plan;
  the plan's `notes` carry the reason.
- **On-demand index production and optional activation.** The context *index*
  can be built or updated by `ocg context` / `ocg context symbols`, the library
  API, or an ordinary orchestration bridge context request. A launch only
  materializes the generated plugin adapter (see
  [Orchestration](#orchestration)); indexing begins when a context request is
  handled. `context.enabled: false` makes explicit context commands no-ops and
  makes orchestration dynamic context empty.
- **No invented OpenCode mechanism.** Activation uses the supported local JS
  plugin contract; the generated adapter is injected through the config's
  `plugin` array and calls the hidden `ocg __bridge` command. Nothing is
  smuggled into an undocumented config key.

`ocg cache clean` removes the cache subtree only; `runtime/`, `index/`, the
raw verification `logs/` and `checkpoints/` are never touched by it. Creating
the index or cache adds `.opencode-gear/` to the project `.gitignore` once via
the runtime's idempotent helper.

The plan carries a fixed conceptual section order (gear instructions, project
policy, repository map, capability/tool descriptions, task capsule, relevant
symbols/source, current git diff, verification state) and embeds the capability
plan and targeted-test proposal. The stable order is part of the contract and is
covered by deterministic tests.

## Runtime lifecycle boundary

Mission semantics and execution mechanics have separate owners:

```text
Mission / Controller / Bridge
              │
              ▼
runtime::lifecycle::RuntimeAdapter
              │
              ▼
OpenCode V2 adapter (V2SessionClient + OwnedV2Server)
```

`RuntimeAdapter` is the small, stateful per-invocation lifecycle seam. It
owns the operations the current controller and bridge actually consume:
resolve/create/inspect a runtime execution, apply and verify a runtime profile,
observe normalized context, and stage/resume a continuation. Its execution ID is
an opaque replaceable binding; it is never a Mission ID. The durable Mission
continues to use the existing `session_id` field on disk for backward
compatibility, with `runtime_execution_id()` as the typed view of that binding.

OpenCode V2 is the first concrete implementation. Its HTTP routes, response
JSON, service registration, agent/model selection, synthetic continuation
protocol, and V2 token/model projection stay in the OpenCode adapter. The
controller consumes typed execution/profile/continuation values. The generated
plugin/bridge remains the intentional OpenCode event-integration layer, but it
converts event payloads into the neutral observation request rather than
interpreting V2 session responses. V1 remains the existing request-scoped
compatibility path: it has no V2 lifecycle capabilities and does not emulate
session creation or continuation.

Capabilities are explicit. A caller can distinguish execution creation, profile
selection, context observation, continuation staging/resume, and persistent
lookup; unsupported operations return a typed `RuntimeError` rather than a
silent fallback. Errors retain a redacted detail alongside a small classification
(unavailable, unsupported, missing execution, authentication, transport,
invalid response, profile selection, or provider completion).

This boundary does not introduce a registry, scheduler, Resource Broker, second
runtime, generic plugin protocol, or Reconciler. The existing OpenCode
compatibility descriptor remains responsible for V1/V2 config, plugin, and
launch differences.


Orchestration is the layer that carries a task across roles. It is deliberately
split so policy cannot drift into the wrong language:

```text
Rust (authority)
  controller/state   phases, attempts/retries, freshness, retry/Debug policy
  mission            durable per-task record: identity, generation, lifecycle
  projection         typed ModelHandoffCapsule, caps, secret defense
  bridge             `ocg __bridge <event>` JSON translation + telemetry
  runtime boundary   neutral execution/profile/context/continuation contract
  OpenCode adapter   V1/V2 lifecycle and transport implementation
  plugin generator   materialize the adapter, inject the file:// URL

JavaScript (transport only)
  ocg-orchestration.js  enforces Lead requests, appends context, spawns bridge
```

- **One authority.** The adapter does no ranking, projection or policy; it only
  moves bytes between OpenCode hooks and the Rust bridge with a direct argv and
  stdin (never a shell). The bridge drains and caps stdin (4 MiB) before any
  early return, so a disabled or rejected payload cannot cause a BrokenPipe.
- **Exact Lead request contract.** Rust resolves the active throttle to one
  Lead agent/provider/model/variant and exports that typed contract at launch.
  The supported `chat.message` hook writes it to mutable `output.message`
  before OpenCode saves or executes the user request. This wins over sticky
  model/variant and reused-session state without mutating global OpenCode state.
  The guard leaves worker subagent requests untouched.
- **Runtime model preflight.** Coding launches probe `opencode models` with the
  generated config but without loading the local plugin. A definitely missing
  active Lead model blocks launch; probe failure is distinguished from absence
  and remains nonfatal. Doctor reports every Execution Tier and worker route.
- **No duplicate Lead context.** Only the Lead-context hook (`chat.message` on
  v1, `session.context` on v2) is scoped to the Lead session (the request
  agent starts with `lead-`); worker subagent sessions and requests with no
  identifiable agent skip it, while `tool.execute.before/after` remain active
  everywhere.
- **One baseline per repository generation.** The repository context carries a
  deterministic `snapshot_id` (the task-independent repository generation:
  SHA-256 over the indexed content) plus estimate-only metadata
  (`estimated_tokens` = bytes / 4, `bytes`, `file_count`, `symbol_count`). The
  bridge records the injected identity on the session in
  `.opencode-gear/orchestration/state.json`. On the v1 persisted-prompt path
  (`chat.message`) an unchanged identity returns an empty `context` with
  `cached: true`, so an unchanged repository snapshot is never appended twice
  to the persisted history. On the v2 model-dispatch path (`session.context`)
  nothing is persisted, so every root-Lead dispatch receives the full baseline
  again: an unchanged identity reuses the retained rendering (`cached: true`
  reports the reuse, never an empty context), and only a material repository
  change re-renders it. A bridge failure is still swallowed.
- **Typed hand-offs.** The rich `ProjectionInput` (a `TaskCapsule` plus
  selected source slices, a bounded diff summary and an optional verification
  block) is projected into a compact `ModelHandoffCapsule` for exactly one
  source→destination transition. Each destination clears the fields it does not
  need, so an Explore hand-off cannot leak verification residue and a Debug
  hand-off cannot leak exploratory narrative. Required fields survive; optional
  material is dropped deterministically and counted. Source slices are a
  separate dynamic-context block.
- **Bounded (runtime optimization envelope).** `orchestration.maxHandoffBytes`
  (default `16384`) and `maxHandoffRatioPercent` (default `60`) bound every
  capsule. These are size optimizations, not correctness rules: required
  evidence is never dropped to satisfy them, and an overage is recorded. The
  deterministic release fixture uses a tight `4096` / `40%` regression gate.
- **Freshness.** Checkpoints are revalidated before reuse; a stale or corrupt
  checkpoint is reported, not silently trusted. The post-Build Verify hand-off
  is rebuilt from a refreshed context plan (current changed files, symbols and a
  real bounded diff) plus the verification result.
- **Retry then Debug.** `after_build` runs only the explicitly configured
  verification commands, checkpoints Build→Verify, and either passes, allows a
  bounded Build retry, or recommends Debug with an explainable reason. Debug
  gets only failures, evidence, the relevant bounded diff, raw-log references and
  the distilled verification block. Returning from Debug to Build is a distinct
  `DebugToBuild` transition with its own checkpoint.
- **Real diff.** The context plan preserves the actual `DiffSummary` it ranked
  against; Build/Verify/Debug hand-offs render a bounded, deterministic view of
  its retained entries and hunks, filtered for sensitive paths and secret-shaped
  content, with structural truncation stated.
- **Fail-soft caches, strict product state.** Corrupt session state, checkpoint
  or cache recovers (session state falls back to empty); a disabled
  orchestration emits no plugin, writes no state and records nothing. A corrupt
  or unsupported **Mission** record is never recovered that way: it is
  quarantined for inspection and the call fails explicitly, so committed work
  cannot silently restart from zero.
- **Capabilities stay advisory.** Capability narrowing is included as advice in
  the dynamic context; runtime permission enforcement remains OpenCode's job.

### Mission and session

**Session is disposable execution state. Mission is durable product state.**

A session (`state.json`, keyed by the OpenCode session id) is bounded, evicted
and recoverable-to-empty: it may die, be compacted, replaced or rolled over at
any time. A Mission is the versioned record of one admitted task and lives
outside that lifetime:

- **Identity.** `mission_id` is the existing deterministic task id, derived
  from the admitted task text and never from a session, so the same Mission
  identity is recovered across session changes, restarts and rebinds.
- **Binding.** The current runtime execution is `session_id` on the Mission:
  replaceable execution metadata, not identity. The serialized name remains for
  compatibility with existing Mission files; the typed runtime view is
  `RuntimeExecutionId`. A fresh execution that admits the same task binds to the
  same Mission and is seeded from it, keeping findings, attempts, checkpoint
  references and the exact next action.
- **Lifecycle.** `Active` is the only non-terminal status; `Completed`,
  `Failed` and `Cancelled` are unambiguous terminal states with a persisted
  reason. A conflicting transition fails explicitly; re-admitting a terminal
  task starts the next `generation` with fresh progress and retained history.
- **Idempotency.** A bounded history records consequential transitions
  (admission, binding, `ExploreToBuild`, `BuildToVerify`, `VerifyToDebug`,
  `DebugToBuild`, terminal states). Each event has a deterministic identity, so
  replaying the same transition is a no-op instead of a duplicate effect.
- **Persistence.** One atomically written file per Mission under
  `.opencode-gear/orchestration/missions/<mission_id>.json`, with an explicit
  `schema_version`; unknown versions are quarantined, never silently adopted.
  Checkpoints stay the shared evidence store — Missions store references to
  them, not copies. Sessions predate Missions and adopt one lazily on their
  next touch, keeping their progress.
- **UI/client failure is not Mission failure.** The OpenCode TUI, event
  subscription, bridge process and local V2 HTTP client are replaceable
  messengers. If one disappears, the bridge records an `unknown` observation
  (or no observation at all) and leaves the Mission owner, generation,
  progress and terminal state untouched. A later session can reload the Mission
  and continue. Only a durable, artifact-backed transition may change the
  execution binding; a client disconnect, failed report or interrupted process
  can never imply completion or failure.

### Context pressure and same-generation rollover

The context governor is a small, artifact-backed layer above the Mission. It
observes one runtime message at the completed root-Lead boundary, after the
output event has been handled and the disposable execution view has been
synchronized. The OpenCode V2 adapter supplies the normalized observation; the
policy layer does not know its HTTP or JSON shape:

```text
session.step.ended(stop)
  → lead.output / output acknowledgement
  → context.observe (no transcript text)
  → atomic context telemetry
  → Mission CAS + bounded continuation artifact
  → fresh V2 session + verified Lead
  → staged synthetic continuation
  → owner cutover and acknowledgement
```

- The active-context projection is **one message's** `input + cache.read`.
  Output, reasoning and `cache.write` remain raw telemetry fields; message
  usage is never summed into a cumulative transcript denominator. Overflow,
  missing usage, missing model limits and failed V2 queries are explicit
  `unknown` observations and cannot trigger automatic replacement.
- Policy defaults are an ordered 70% approaching warning and 80% rollover
  request. Thresholds require a trustworthy runtime model limit reported by the
  adapter; an optional absolute cap can operate without one. No percentage is
  fabricated.
- A required observation at an unsafe boundary is durably marked pending. Only
  a completed root-Lead `stop` after output handling is a safe boundary. Tool
  errors, partial output, missing clients and `tool-calls` steps do not claim
  safety.
- The target is created explicitly rather than through `resolve_session`. Its
  agent/model/variant are selected and read back, the target session identity
  is verified, and the launched V2 client is given that `--session` target.
  The old session/transcript remains retained for diagnostics.
- Continuation is a bounded packet derived from Mission state (goal,
  constraints, findings, files, verification references, checkpoints, phase,
  attempts and next action), not a transcript copy. The packet is embedded in
  the rollover artifact and mirrored as an inspectable sidecar under
  `.opencode-gear/orchestration/context/continuations/`; it is staged with
  `resume:false` and resumed with the same stable message id only after the
  owner cutover.
- Mission writes use an atomic sibling lock plus revision/owner compare-and-
  swap at the cutover boundary. A concurrent progress update or stale owner
  produces a conflict; it is never overwritten. A failed pre-cutover step
  preserves the old binding and records a bounded retry cooldown. A failure
  after cutover is recorded as active-but-unacknowledged and is recoverable by
  a later target session. If a new explicit user admission replaces that
  session, it marks the old artifact conflicted, preserves the same generation
  and progress, and prevents stale workers from writing over the new owner;
  conflicted Missions require operator review. Terminal Missions are never
  reopened.

> **Refresh policy.** OCG injects the full repository context snapshot on the
> first Lead prompt of a session. On OpenCode v1 the snapshot is persisted into
> the conversation history, so if the effective repository snapshot is
> unchanged, later prompts in the same session receive no additional repository
> context. On OpenCode v2 the snapshot is injected at model dispatch into the
> ephemeral per-request system context, so every root-Lead model call — first
> turn, later turns and tool-driven continuations alike — receives exactly one
> current baseline; an unchanged repository reuses the retained rendering
> rather than re-deriving it from new task wording. On both runtimes, when the
> snapshot materially changes (for example file edits, new findings or new
> verification state), the new snapshot is supplied at the next injection point
> and becomes the new session baseline.

State lives under `.opencode-gear/orchestration/` (sessions in `state.json`,
Missions in `missions/`) and the adapter under
`.opencode-gear/orchestration/plugin/`; all of it is ignored local state.

## Verification, distillation and checkpoints

Verification is the explicit, configured half of the quality loop. It is a
sibling of the context engine, not part of the model conversation:

```text
config          top-level `verification` policy, stages fast/normal/full
commands        structured program + args, parsed without a shell
capture         centralized in src/process.rs via the CaptureRunner trait
distillation    progress/duplicate removal + errors/warnings/locations/tests
raw logs        .opencode-gear/logs/, bounded and pruned, never committed
tests proposal  conservative, complete=false, advisory only
checkpoints     .opencode-gear/checkpoints/, versioned and freshness-checked
```

Invariants worth preserving:

- **No command is discovered.** A manifest existing is never a reason to run
  anything; all stages start empty. Commands come only from trusted
  configuration or an explicit `ocg verify`.
- **No shell.** Command strings are parsed by a strict word splitter; control
  operators, pipelines, redirections and substitutions are rejected.
- **No fabricated results.** Counts and conclusions are read from the output
  only; an unreadable count is `None`, not zero.
- **One process path.** Every child process, including verification and git,
  is constructed in `src/process.rs`; tests inject fakes and spawn nothing.
- **Advisory capabilities, not a sandbox.** The capability planner and Tool
  Context Firewall describe an intended boundary in context/config. They do not
  activate or enforce runtime tool schemas.
- **Stale is explicit.** A checkpoint revalidates its sources and Git identity
  on load; a stale one is marked and never silently reused, and a corrupt one is
  reported without blocking `ocg`.

The full detail lives in [verification.md](verification.md).

## Local telemetry

The optional context and verification flows record a small local event so the
local reductions can be measured over time:

```text
events          <project>/.opencode-gear/telemetry/events.jsonl (one JSON/line)
schema          task/session id, timestamps, task type, role/provider/model when
                known, token counts with source, context/repo/verification/log
                metrics, capability names, deterministic outcome
tokens          provider_reported | opencode_reported | estimated | unknown
privacy         no prompt/source/command/output/header; metadata redacted
reader          `ocg stats` (offline, read-only, no state creation)
doctor          read-only inspection; missing state is info, corruption a warn
```

Invariants worth preserving:

- **Local only.** No upload, no model API, no network path. The store lives in
  the project, not in a platform remote cache.
- **Estimates stay labelled.** The only token number a local flow can produce is
  an estimate (bytes / 4); it carries `source: "estimated"` and is never shown
  as exact. Provider/OpenCode-reported values are schema-supported but need a
  session hook that is not shipped.
- **Fail soft.** A telemetry configuration, write or corruption problem warns
  and never blocks context, verification or an ordinary launch.
- **No decisions.** The schema is an input for a future budget/capability
  feature; nothing here scores, routes or purchases. Those features are
  explicitly deferred.

The full detail lives in [telemetry.md](telemetry.md); one recorded measurement
is in [token-efficiency.md](token-efficiency.md).

## Why one agent per throttle level

OpenCode binds one model per agent, and the Task tool has no per-call model
parameter. Two consequences:

1. The Lead needs one agent per throttle level (`lead-<level>`), because the
   Lead model differs per level. These are the only visible primary agents, so
   the TUI can cycle them.
2. Because workers are **not** throttle-dependent, they do not need to be
   duplicated per level. There is exactly one `ocg-build`, one `ocg-verify`,
   and so on, shared by every Lead. This is the structural expression of "the
   throttle does not route workers".

## Roles are durable, models are replaceable

```text
LEAD      explore  build  verify  debug  docs      <- durable roles
  │           │       │       │       │      │
OpenAI    Volcano   Go      Go      Go     Go      <- replaceable providers
  │           │       │       │       │      │
Sol/Astra  K2.7/K3  DS4.1   GLMfl   GLM5.3 DS4.1   <- replaceable models
```

- Roles are the abstraction. They appear in `config/routing.yaml` and in
  `config/permissions.yaml`.
- Model ids appear only in `config/models.yaml`.
- Reasoning variants are validated against the `variants` list a model
  declares, so a future `Kimi K4` or `DeepSeek V5` is a config edit, not a
  rewrite.
- Roles are not hardcoded. Adding a role means adding a routing entry, a
  prompt, and (optionally) a permission profile binding. The generator creates
  an `ocg-<role>` worker and substitutes `{{role}}` placeholders in the Lead
  prompt.

## Provider binding is deterministic

A model is bound to one provider. If two providers happen to offer the same
family, the gear does not load-balance between them: the routing table says
exactly where each role goes, and `enabled_providers` is derived from that
table so the session cannot quietly pull a model from somewhere else.

Fallbacks are the only exception, and they are explicit:

- declared per role as `fallback`,
- surfaced in the rendered Lead prompt and validated by `ocg validate`,
- intended for provider failure or quota exhaustion only, never a silent
  permanent switch.

## Isolation model

Isolation is enforced by OpenCode permissions, not by asking nicely in a
prompt:

```text
User
 └─ Lead (primary)
     └─ worker (subagent, task: deny, hidden)
```

- Workers cannot delegate (`task: deny`), so the tree is one level deep.
- The Lead's `permission.task` is `deny` by default with an explicit allow for
  its own workers.
- EXPLORE is read-only. VERIFY and DEBUG can read and run checks but cannot
  edit. BUILD and DOCS can edit.

## Escalation as policy, not machinery

OpenCode has no scheduler that can enforce "retry twice, then escalate". The
gear therefore encodes the rules in the Lead prompt:

- two-strike handoff to DEBUG,
- scope-explosion stop-and-report,
- worker disagreement returns to the Lead.

This is honest about what can and cannot be enforced mechanically. If a future
OpenCode release exposes a routing hook, the rules are already written down in
one place (`config/prompts/lead.md`) and `config/routing.yaml`.

## Extension points

| You want to | Edit |
| --- | --- |
| Use a newer model | `config/models.yaml` + `config/routing.yaml` (or an override) |
| Change Execution Tiers | `config/throttle.yaml` |
| Add a specialist role | add a prompt, a routing role, a permission profile binding |
| Pin a project to different models | `<project>/.opencode-gear.yaml` |
| Add repository-specific Lead policy | `prompts.lead.append` in `<project>/.opencode-gear.yaml` |
| Replace a prompt for one project | `prompts` override pointing at a file |
| Add raw OpenCode settings | `opencode` key in an override, or `config/base.yaml` |

## Project policy stays out of the core

The gear prompt is a project-agnostic baseline. Repository-specific rules are
layered on **per project**, never written into the core:

```text
gear default prompt  (config/prompts/lead.md, project-agnostic)
        +
project override     (<project>/.opencode-gear.yaml -> prompts.lead.append)
        =
rendered Lead prompt
```

Two consequences worth keeping true:

1. Upgrading the gear never overwrites project policy, because project policy
   lives outside the gear tree.
2. The public core never accumulates one project's domain rules, so it stays
   usable by an unrelated Rust, Go, Python, Java or TypeScript repository.

`ocg layers` shows which override file was applied, and `ocg --dry-run` shows
the rendered prompt.

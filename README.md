# OpenCode Gear

**OpenCode Gear** (`ocg`) — project-agnostic multi-model orchestration for
[OpenCode](https://opencode.ai).

OpenCode Gear is a small configuration layer that separates two things a lot of
agent setups accidentally fuse together:

```text
THROTTLE
    how strong (and how expensive) the Lead is
        low  /  mid  /  high

WORKER ROUTER
    which specialist model does the delegated work
        EXPLORE / BUILD / VERIFY / DEBUG
```

They are independent on purpose:

- **Throttle** only ever changes the Lead. Bumping the throttle must not
  silently move your build work onto a different model.
- **Routing** never depends on throttle. Kimi explores, DeepSeek builds, GLM
  verifies and debugs, at every throttle level.

The result is a predictable setup: one Lead that reasons and decides, plus
specialists that are good at their one job and cheap enough to use often.

> OpenCode Gear is a community project. It is **not** an official OpenCode,
> OpenAI, Volcano, DeepSeek, GLM or Kimi product, and it is not endorsed by any
> of those providers. Model names and plans change; the mapping in this
> repository is an opinionated default you are expected to override.

---

## Contents

- [How it works](#how-it-works)
- [Install](#install)
- [Requirements](#requirements)
- [Build from source](#build-from-source)
- [Quick start](#quick-start)
- [Managed runtime](#managed-runtime)
- [Throttle semantics](#throttle-semantics)
- [Worker Router semantics](#worker-router-semantics)
- [Provider and model mapping](#provider-and-model-mapping)
- [Configuration and overrides](#configuration-and-overrides)
- [Project-local policy](#project-local-policy)
- [Commands](#commands)
- [Environment variables](#environment-variables)
- [Isolation and permissions](#isolation-and-permissions)
- [Security and secrets](#security-and-secrets)
- [Observability (optional)](#observability-optional)
- [Repository context (optional)](#repository-context-optional)
- [Orchestration (optional)](#orchestration-optional)
- [Troubleshooting](#troubleshooting)
- [Repository layout](#repository-layout)
- [Tests](#tests)
- [Migrating from an older Gear/profile setup](#migrating-from-an-older-gearprofile-setup)

---

## How it works

```text
User
 │
 ▼
Lead                             ← throttle picks the tier
 │
 ├── understand
 ├── reason
 ├── decompose
 ├── decide
 ├── delegate ────────────────┐
 └── accept / reject results  │
                              ▼
                     Worker Router          ← config/routing.yaml
                              │
      ┌───────────────┬───────┴────────┬────────────────┐
      ▼               ▼                ▼                ▼
   EXPLORE          BUILD           VERIFY        HARD VERIFY /
   Kimi K2.7 / K3   DeepSeek V4.1   GLM-5.3       DEBUG
   (Volcano)        Flash (Go)      Flash (Go)    GLM-5.3 (Go)
```

`ocg` is a self-contained Rust binary. It embeds the shipped configuration and
prompts at build time, resolves the effective configuration in layers, then
launches `opencode` with the merged result through `OPENCODE_CONFIG_CONTENT`:

```text
OpenCode Gear embedded defaults  (compiled into ocg)
        ↓
user config                      (~/.config/opencode-gear/config.yaml)
        ↓
project config                   (<project>/.opencode-gear.yaml)
        ↓
CLI / environment                (ocg high, --throttle, OPENCODE_GEAR_THROTTLE)
```

The config pipeline writes nothing back into the repository or the project.
The persisted state is the default throttle level (only when you ask for it
with `ocg throttle <level>`), plus the optional managed runtime and its update
cache described in [Managed runtime](#managed-runtime).

OpenCode binds exactly one model per agent, so the gear materialises:

- one **Lead agent per throttle level** (`lead-low`, `lead-mid`, `lead-high`)
  so the TUI can switch tiers live, and
- one **worker agent per role** (`ocg-explore`, `ocg-build`, ...) shared by
  all Lead levels.

Because the worker agents do not depend on the throttle, switching throttle
during a session cannot silently re-route BUILD or VERIFY.

The shipped Lead request contracts are exact:

| Throttle | Agent | Provider/model | Variant |
| --- | --- | --- | --- |
| low | `lead-low` | `openai/gpt-5.6-sol` | `low` |
| mid | `lead-mid` | `openai/gpt-5.6-sol` | `medium` |
| high | `lead-high` | `openai/gpt-6-astra` | `low` |

Before a coding launch, OCG asks the selected OpenCode runtime for its model
catalogue. A definitely missing active Lead model blocks the launch instead of
silently falling back; an unavailable probe warns and continues. The generated
plugin then enforces the selected Lead agent/model/variant on each mutable
`chat.message`, so sticky TUI or reused-session state cannot change the request.
Worker subagent requests are not rewritten. The explicit no-hook escape hatch
(`OPENCODE_GEAR_ORCHESTRATION=0` or `orchestration.enabled=false`) intentionally
disables the generated plugin and therefore this runtime enforcement.

## Install

One line, no `sudo`, no Homebrew, no Node, no `git`:

```bash
curl -fsSL https://raw.githubusercontent.com/18601673727/opencode-gear/main/install.sh | sh
```

The installer downloads the `ocg` binary for your platform, verifies it
against the release `SHA256SUMS`, and atomically installs it to
`~/.local/bin/ocg`. It then prints the `PATH` line to add if that directory is
not already on your `PATH`:

```bash
export PATH="$HOME/.local/bin:$PATH"
```

To pin a release, or to choose another directory:

```bash
OPENCODE_GEAR_VERSION=v0.3.0 sh install.sh
OPENCODE_GEAR_INSTALL_DIR="$HOME/bin" sh install.sh
```

Supported platforms:

| OS | Architecture | Artifact |
| --- | --- | --- |
| Linux | x86_64 / amd64 | `ocg-linux-x86_64` |
| Linux | arm64 / aarch64 | `ocg-linux-arm64` |
| macOS (Darwin) | x86_64 | `ocg-darwin-x86_64` |
| macOS (Darwin) | arm64 / Apple silicon | `ocg-darwin-arm64` |

The installer is a plain POSIX `sh` script using only `curl`, `mktemp` and
`shasum`/`sha256sum`. It downloads into a unique temporary file inside the
install directory, verifies the release `SHA256SUMS`, runs the staged binary's
own `version` before replacing anything, and only then moves it into place.
A failure at any step keeps the existing installation untouched. It never
edits your shell startup files and never installs anything outside the install
directory.

## Requirements

| Requirement | Notes |
| --- | --- |
| Rust 1.88+ | Build-time only; the released `ocg` binary is self-contained. |
| OpenCode | **1.18.x or 2.x.** Version-specific behaviour lives behind one compatibility boundary (v1/v2 adapters); `ocg` always uses `OPENCODE_CONFIG_CONTENT`. |
| OpenAI plan/credentials | For the Lead (`openai` provider). |
| Volcano Coding Plan Pro | For the EXPLORE models (Kimi). Declared in `config/base.yaml`. |
| OpenCode Go | For BUILD / VERIFY / DEBUG (DeepSeek and GLM). |

`ocg` can install and manage OpenCode itself (see
[Managed runtime](#managed-runtime)), so a separate OpenCode install is
optional. You still need a compatible `opencode` on `PATH` if you prefer to
manage it yourself.

> The Rust test suite and the installer tests run on Linux here. macOS support
> is provided by the release artifacts and the installer's Darwin mapping, but
> it has **not** been runtime-tested on a real Mac in this repository.

Authenticate the providers once with OpenCode itself:

```bash
opencode auth login openai
opencode auth login volcengine-coding-plan
opencode auth login opencode-go
```

The exact provider ids depend on your OpenCode build; check the list with
`opencode auth login` or `ocg models`.

> **Note on first-time `/connect` or `auth login`**: these steps authenticate
> the provider for OpenCode. OpenCode may prompt you to pick a default model
> and reasoning effort; any choice is fine for OCG. Those are OpenCode's
> session defaults and do **not** configure OCG's Lead or Worker Router.
> OCG applies its own contracts from `config/models.yaml` + throttle at
> runtime. The provider must be authenticated and the referenced models
> (`volcengine-coding-plan/kimi-*` etc.) must be visible.

## Build from source

```bash
git clone https://github.com/18601673727/opencode-gear.git
cd opencode-gear
cargo build --release
# binary: target/release/ocg
```

Put `target/release/ocg` on your `PATH` (for example by copying it into a
directory that is already there), or run `make package` to build the
platform artifact and a local `SHA256SUMS`.

`ocg` is the OpenCode Gear CLI. If you already have a different tool installed
under a similar name, choose the name you invoke accordingly; `ocg` does not
shadow it.

## Quick start

```bash
ocg status                  # throttle, routing and the config layers in use
ocg routing                 # role -> model table
ocg mid                     # interactive session with the mid Lead
ocg --throttle high         # same thing spelled explicitly
ocg run "summarise the failing tests"   # one-shot run
ocg --dry-run               # print the merged OpenCode config, launch nothing
```

Run these from any project directory. OpenCode starts in the current working
directory, and project-local overrides are read from `.opencode-gear.yaml`
there.

## Managed runtime

`ocg` is not only a config layer: it also decides which `opencode` executable
to run, and can install and update a project-local one so a project does not
depend on whatever happens to be on `PATH`.

### Runtime precedence

For every launch the executable is resolved in this exact order:

```text
1. explicit executable   OPENCODE_GEAR_OPENCODE (or compat aliases)
2. managed project       <project>/.opencode-gear/runtime/opencode/...
3. system PATH           an `opencode` on PATH, if 1.18.x or 2.x
4. project-local bootstrap   install a managed runtime for this project
```

The resolved runtime's `--version` is classified explicitly. A `1.18.x`
runtime uses the v1 adapter (`plugin`, `task`, request-scoped Lead); a `2.x`
runtime uses the v2 adapter (`plugins`, `subagent`, session-scoped Lead). An
unsupported major fails clearly instead of guessing.

Rules that fall out of this:

- An explicit executable is authoritative. If it is missing or not
  executable, `ocg` errors instead of silently falling back.
- An existing managed runtime always wins over the system `opencode`, so a
  project stays deterministic once bootstrapped.
- If there is no managed runtime and the system `opencode` is compatible,
  nothing is installed; the system runtime is used.
- When a compatible system runtime has a **due** check, `ocg` resolves the
  latest release once, runs OpenCode's own `opencode upgrade <version>`,
  reprobes the version, records the check and launches the result. If lookup or
  upgrade fails, `ocg` warns, records the failed check and continues with the
  old compatible version.
- An incompatible or unusable system `opencode` is upgraded with
  `opencode upgrade <resolved-version>` when `autoUpgrade` is on; if that fails
  or leaves it incompatible, `ocg` bootstraps a project-local runtime.
- `autoUpgrade: false` disables those optional system upgrades, but it does
  **not** disable the project-local fallback: a missing or incompatible
  runtime is still bootstrapped on launch.
- The managed runtime is project-local: `<project>/.opencode-gear/` is created
  and `.opencode-gear/` is appended once to the project `.gitignore`.

The exact executable environment variable is `OPENCODE_GEAR_OPENCODE`. The
Commit-1 `OPENCODE_GEAR_OPENCODE_BIN` and legacy `OC_GEAR_OPENCODE_BIN` names
remain valid compatibility aliases; the canonical name wins.

### Runtime policy

Policy lives in a small top-level `runtime` object (never in the raw
`opencode` config key):

```yaml
runtime:
  channel: latest          # only supported channel
  autoUpgrade: true        # upgrade managed runtimes when due
  checkIntervalHours: 24   # update-check cache interval
  fallback: project-local  # bootstrap target
  version: 1.18.31         # optional exact semver pin
```

All fields are optional; the defaults above are used when the object is
absent. `ocg validate` rejects an unknown channel, a non-boolean
`autoUpgrade`, a non-positive `checkIntervalHours`, an unknown `fallback`, a
non-semver pin, or a pin below the `1.18.0` floor.

### Update checks and cache

Update checks are cached for `checkIntervalHours` (default 24) in the platform
cache directory:

| OS | Cache directory |
| --- | --- |
| Linux | `~/.cache/opencode-gear/` (`$XDG_CACHE_HOME` respected) |
| macOS | `~/Library/Caches/opencode-gear/` |

A launch does **not** network on every start: it only checks when the cache is
missing or older than the interval. Both successful and failed checks are
recorded, so a runtime that just failed to upgrade is not retried on every
launch within the interval. A failed check warns and keeps the working
runtime. `ocg upgrade` ignores the cache and checks immediately.

How a check is performed depends on the active source:

- **System runtime:** `ocg` resolves the latest OpenCode release through its own
  HTTP transport and then runs OpenCode's own `opencode upgrade <version>`. A
  compatible system runtime is kept if the lookup or the upgrade fails; OCG
  never fetches a release archive itself for a system runtime and never
  replaces a system runtime with a managed copy.
- **Managed runtime:** `ocg` checks the GitHub release and installs the newer
  managed version.

Managed downloads **fail closed**: a release asset that lacks a `sha256:`
digest is refused rather than installed unverified.

### Pinning

Set `runtime.version` to an exact semver (for example `"1.18.31"`) to pin a
project to one OpenCode version. A pin never silently advances, is not
replaced by the system runtime, and is preserved by `ocg upgrade`.

### Runtime commands

```bash
ocg version   # Gear, platform, resolved OpenCode version/source/path (read-only)
ocg doctor    # layered config, Lead contracts, OpenCode, proxy, provider and runtime (read-only)
ocg upgrade   # self-update Gear, then force-maintain the active OpenCode
```

`ocg version` and `ocg doctor` never install, upgrade or bootstrap anything.
`ocg doctor` explains what a bootstrap would do when no runtime is present.

### `ocg doctor`

One command to answer "why is my configuration not taking effect?". It is
read-only and deterministic, and it never prints a credential.

```bash
ocg doctor
```

```text
OpenCode Gear doctor
  platform               [PASS] linux-x86_64
  gear                   [PASS] /usr/local/bin/ocg (Gear 0.3.0)
  ...
config layering
  defaults               [PASS] embedded in the binary (no OPENCODE_GEAR_HOME)
  user config            [INFO] ~/.config/opencode-gear/config.yaml (not present)
  project config         [PASS] /work/app/.opencode-gear.yaml
  project root           [PASS] /work/app
effective Lead contracts
  lead-low               [PASS] openai/gpt-5.6-sol variant low (active)
  lead-mid               [PASS] openai/gpt-5.6-sol variant medium
  lead-high              [PASS] openai/gpt-6-astra variant low
  default throttle       [INFO] low
  default agent          [INFO] lead-low
worker router (independent of throttle)
   ocg-explore            [PASS] volcengine-coding-plan/kimi-k2.7-code (explore)
  ...
environment / proxy
  HTTP_PROXY             [INFO] not set
  ALL_PROXY              [WARN] uses a SOCKS scheme OCG does not interpret; OCG's own
                                network calls ignore it, but the value is preserved
                                verbatim for the child OpenCode process
...
doctor summary
  23 passed, 1 warnings, 0 failures (9 informational)
```

It reports:

- **config layering** — embedded/disk defaults, user config, project config and
  the project root, each found or missing;
- **effective Lead contracts** — the model and variant behind each of `low` /
  `mid` / `high`, the default throttle and the default agent;
- **worker router** — the role → provider/model summary, which is independent
  of the throttle and never prints a secret;
- **OpenCode** — the runtime OCG would launch (managed / project-local / system
  / explicit), its version, the `opencode` on `PATH`, and a WARN when they
  differ;
- **environment / proxy** — presence of `HTTP_PROXY` / `HTTPS_PROXY` /
  `ALL_PROXY` / `NO_PROXY` (values are never shown) and the exact SOCKS
  pass-through contract;
- **provider / auth readiness** — whether each configured model is currently
  exposed by the resolved OpenCode runtime, via its own model catalogue. OCG
  never reads or prints provider credentials.

Each line is **PASS / INFO / WARN / FAIL**. Only a FAIL makes `ocg doctor` exit
non-zero, so a warning (for example a SOCKS proxy) is safe in a scripted smoke.

### Runtime layout and cleanup

```text
<project>/.opencode-gear/runtime/opencode/
  <version>/opencode     managed executable (atomic install)
  active.json            pointer: version, path, installed_at
```

`active.json` records the version; the executable path is always re-derived
from that version as `<version>/opencode`, so an arbitrary recorded path is
never trusted. The binary must be executable; a partial or non-executable
install is repaired on the next install.

To remove a managed runtime, delete the project's `.opencode-gear/` directory.
To remove the update-check cache, delete the cache directory above. Neither
belongs in version control; the project `.gitignore` entry is added on first
bootstrap.

## Throttle semantics

Throttle selects the Execution Tier only. The Lead is provider-agnostic: the shipped
defaults use OpenAI, but any configured provider/model works, and a Lead model
may omit a reasoning variant to run at the provider default.

| Level | Lead | Reasoning | Intent |
| --- | --- | --- | --- |
| `low` | `openai/gpt-5.6-sol` | `low` | Default. Economical Lead. |
| `mid` | `openai/gpt-5.6-sol` | `medium` | Balanced: stronger reasoning on the same model. |
| `high` | `openai/gpt-6-astra` | `low` | Premium Lead: larger, stronger model at its lowest variant. |

Notes:

- The model ids and reasoning variants are centralised in `config/models.yaml`
  and `config/throttle.yaml`. Swap a model there and nothing else changes.
- **Throttle only changes the Lead contract.** It never rebuilds, copies or
  follows the Worker Router, so EXPLORE / BUILD / VERIFY / DEBUG stay bound to
  the same models at every level.
- If a provider does not expose a reasoning variant, omit `variant` and the
  provider default is used.
- **Throttle never decides which worker handles EXPLORE / BUILD / VERIFY.**
- Precedence: positional `ocg high` / `--throttle` > `OPENCODE_GEAR_THROTTLE`
  (legacy `OC_GEAR_THROTTLE`) > project config > user config >
  `config/throttle.yaml` default.

Persist a default (writes `~/.config/opencode-gear/config.yaml`):

```bash
ocg throttle mid
ocg throttle          # print the resolved default
```

## Worker Router semantics

| Role | Responsibility | Read/write |
| --- | --- | --- |
| EXPLORE | Repository reconnaissance, call paths, data flow, impact mapping, context compression for the Lead. Must not start a large rewrite just because it found the affected scope. | read-only |
| EXPLORE (deep) | Very large context, whole-repository reasoning, hard cross-cutting analysis. | read-only |
| BUILD | Implement approved, bounded work; edit code; add/update tests; iterate. | read/write |
| VERIFY | Independently inspect a completed diff against acceptance criteria; look for missed files, API/type/state inconsistencies, regressions. | read + tests |
| DEBUG | Escalation target: difficult debugging, root-cause analysis, architecture-sensitive review, problems BUILD failed to resolve. | read + tests |
| DOCS | Factual closeout reports and handoff notes after work is verified. | read/write |

Roles are arbitrary: you can add a new role with its own model and prompt in an
override. The shipped six roles are the baseline, not a hard limit.

The Lead model is **not** part of the normal worker pool: it is never a
worker target. In the shipped routing, no OpenAI model appears in the
Worker Router.

### Escalation rules

These are prompt- and policy-level rules, not a scheduler. OpenCode cannot
enforce them mechanically, so the Lead prompt encodes them explicitly:

- **Two-strike handoff.** If the same worker fails at substantially the same
  problem twice, do not issue a third identical retry. Escalate to DEBUG. If
  DEBUG cannot resolve it either, the Lead takes the problem back and decides.
- **Scope explosion.** If a worker finds that approved work is much larger
  than expected (a three-file change turning into a schema + backend +
  frontend + migration redesign), it stops, summarises the new scope, and
  returns to the Lead. Use EXPLORE for impact analysis if useful. A worker
  must never silently redefine the task.
- **Worker disagreement.** If two workers disagree on an architectural or
  semantic decision, neither is the final authority: both opinions go to the
  Lead, which decides.

### Fallbacks

A role may declare an optional `fallback` model. Fallbacks are shown in the
Lead prompt (and validated by `ocg validate`), and are meant for provider
failure or quota exhaustion only. A fallback must never become the silent
default. By default no fallbacks are configured.

## Provider and model mapping

Deterministic by design — the same model is not moved between providers just
because another provider also offers it:

| Model | Provider | Roles |
| --- | --- | --- |
| Kimi K2.7 (`kimi-k2.7-code`) | Volcano Coding Plan Pro | EXPLORE |
| Kimi K3 (`kimi-k3`) | Volcano Coding Plan Pro | EXPLORE deep |
| DeepSeek V4.1 Flash | OpenCode Go | BUILD, DOCS |
| GLM-5.3 Flash | OpenCode Go | VERIFY |
| GLM-5.3 | OpenCode Go | DEBUG |
| GPT-5.6 Sol | OpenAI | Lead (low / mid) |
| GPT-6 Astra | OpenAI | Lead (high) |

Model ids live in `config/models.yaml`; role assignments live in
`config/routing.yaml`; throttle levels live in `config/throttle.yaml`.

## Configuration and overrides

An override file is a partial copy of the gear registries. Everything is
deep-merged, so you only write the keys you want to change. The top-level keys
mirror the files in `config/`: `throttle`, `models`, `routing`, `permissions`,
plus the gear-only extras `prompts`, `observability`, `runtime` and `opencode`.

```yaml
# pick a different default Execution Tier
throttle:
  default: mid

# re-point one role
routing:
  roles:
    build:
      model: glm-5.3
      variant: max

# add or re-point a model (same shape as config/models.yaml)
models:
  models:
    sol:
      provider: openai
      id: gpt-5.6-sol
      label: GPT-5.6 Sol
      variants: [none, low, medium, high, xhigh, max]

# extend a prompt instead of replacing it (recommended for project policy)
prompts:
  lead:
    append:
      - .opencode/lead-policy.md

# or replace a prompt entirely (path, or text)
# prompts:
#   lead: ~/prompts/my-lead.md

# optional local routing trace
observability:
  enabled: true
  path: ~/state/opencode-gear/events.jsonl

# raw OpenCode config merged into the result last
opencode:
  username: you
```

To redefine a throttle level, override the `throttle` registry itself:

```yaml
throttle:
  levels:
    high:
      model: astra
      variant: xhigh
```

Locations:

| Layer | Default path | Override with |
| --- | --- | --- |
| User | `~/.config/opencode-gear/config.yaml` | `OPENCODE_GEAR_USER_CONFIG` (legacy `OC_GEAR_USER_CONFIG`) or `--user-config` |
| Project | `<project>/.opencode-gear.yaml` | `OPENCODE_GEAR_PROJECT_CONFIG` (legacy `OC_GEAR_PROJECT_CONFIG`) or `--project-config` |

`ocg --project DIR` resolves the project layer against `DIR` and launches
OpenCode there. Precedence is project over user over defaults. Validate at any
time:

```bash
ocg validate
```

Validation catches unknown model keys, unknown providers, reasoning variants
that a model does not expose, missing prompts and malformed fallbacks, and it
refuses to build a config that would otherwise fail at runtime.

Prompt paths may be absolute, or relative — relative paths are resolved against
the project directory first, then the gear's `config/` directory when a gear
home is in use.

## Project-local policy

OpenCode Gear core is project-agnostic. Repository-specific instructions should
be supplied through project-local configuration/instruction layers rather than
by editing Gear's core prompts.

The recommended shape is an **append**, not a replacement, so the gear prompt
keeps improving while project policy stays independent:

```text
<project>/.opencode-gear.yaml      # project override
prompts:
  lead:
    append:
      - .opencode/lead-policy.md

<project>/.opencode/lead-policy.md # the project's own rules
```

The rendered Lead prompt is then:

```text
OpenCode Gear generic core prompt
        +
---
project policy
---
```

Why append:

- Updating the gear (a new model, a new escalation rule) never overwrites
  project policy, and project policy never freezes the core prompt.
- The gear repository stays free of any single project's conventions, domain
  rules or private context.
- The override is per project: another repository can append a completely
  different policy, or none at all.

`{{role}}` placeholders and `{{routing}}` are substituted in appended text too,
so project policy can refer to the configured agents without hardcoding model
ids. Appending affects only the role you target — worker prompts stay
project-agnostic.

## Commands

```text
ocg [low|mid|high] [--throttle low|mid|high] [--project DIR] [--dry-run] [--disable-proxy] [command] [args...]

(none)              launch interactive OpenCode with the gear config
run <args...>       launch `opencode run`
models [args...]    run `opencode models`
status              throttle, routing and config layers
routing             worker role -> model table
throttle [level]    print, or persist, the default throttle level
validate            validate the merged configuration
layers              show config layers and trace state
init                create a minimal project .opencode-gear.yaml
build               print the resolved OpenCode config
context <task...>   deterministic local repository context plan
context symbols <q> find indexed symbols by name (diagnostic)
cache clean|stats   manage the local context cache (never the runtime)
stats [--pretty]    read-only local telemetry aggregate (offline)
verify [fast|normal|full]
                    run only explicitly configured trusted commands
tools <task...>     capability plan / Tool Context Firewall view (advisory)
checkpoint list|show|save
                    inspect or record a versioned phase checkpoint
version             Gear, platform and the resolved OpenCode runtime
doctor              read-only layering/config/OpenCode/proxy/runtime diagnosis
upgrade             self-update Gear, then maintain the active OpenCode
help                print usage
```

Inside the TUI, `Tab` / `Shift+Tab` cycle the three Lead agents. The cycle
order depends on the active `default_agent`; the default configuration starts
at `lead-low`. If you do not want the keybind, remove `keybinds` from
`config/base.yaml` or override it in your project config.

## Environment variables

`OPENCODE_GEAR_*` is the canonical prefix. The older `OC_GEAR_*` names are
accepted as fallbacks for compatibility, and `OC_GEAR_OPENCODE_BIN` keeps
working explicitly.

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
| `OPENCODE_GEAR_TELEMETRY` (legacy `OC_GEAR_TELEMETRY`) | force local telemetry off/on |
| `OPENCODE_GEAR_ORCHESTRATION` (legacy `OC_GEAR_ORCHESTRATION`) | force orchestration off/on for this process (`0` emits no plugin and no state) |
| `OPENCODE_GEAR_DISABLE_PROXY` | `1`/`true`/`on`/`yes` disables all proxy use for this process |
| `HTTP_PROXY` / `HTTPS_PROXY` / `ALL_PROXY` / `NO_PROXY` | standard proxy variables (upper- or lower-case); used only when `OPENCODE_GEAR_DISABLE_PROXY` is not truthy |
| `GH_TOKEN` / `GITHUB_TOKEN` | optional token for OCG's own GitHub API requests (`GH_TOKEN` wins); sent only to `api.github.com` |

## Proxy and network access

OCG resolves exactly one proxy policy per launch, in this order:

```text
1. --disable-proxy on the command line
2. OPENCODE_GEAR_DISABLE_PROXY=1/true/on/yes
3. a non-empty HTTP_PROXY / HTTPS_PROXY / ALL_PROXY / NO_PROXY
   (either spelling)
4. static macOS discovery via /usr/sbin/scutil --proxy
5. direct
```

- Only `http://` and `https://` proxy URLs are used by OCG's own HTTP client; an
  unusable value is ignored with a warning.
- A `socks*://` value is not interpreted by OCG (no SOCKS feature is enabled),
  but it is preserved verbatim for child OpenCode processes so an existing
  SOCKS setup is not silently broken.
- A PAC configuration is detected and reported, but never interpreted.
- The proxy URLs and any embedded credentials are never printed, logged or
  written to a file.
- OCG disables `reqwest`'s hidden automatic proxy discovery first and installs
  only the resolved endpoints. Child OpenCode processes (launch, `upgrade`,
  `models`) receive the resolved values under both the upper- and lower-case
  names, with all eight spellings cleared first; `--disable-proxy` or the
  environment switch clears all eight and exports none.

## GitHub API token

When OCG makes its own GitHub API requests (release metadata, self-update,
system runtime maintenance) it uses `GH_TOKEN` first and `GITHUB_TOKEN` second.
The token is attached only when the request host is exactly `api.github.com`;
it is never attached to asset downloads on other hosts, never persisted and
never placed in telemetry. Without a token, requests are anonymous and the
public rate limit (and its reset) is reported in the error message.

## Isolation and permissions

Enforced through OpenCode agent permissions (`config/permissions.yaml`), not
just prompt convention:

- **No recursive delegation.** Every worker has `task: deny`; only a Lead
  orchestrates, so the tree is `User -> Lead -> one level`.
- **The Lead can only reach its own workers.** `permission.task` is
  `{"*": "deny", "<worker agents>": "allow"}`.
- **Hidden internals.** Workers are `hidden: true`, so they stay out of the
  `@` autocomplete while remaining callable by the Lead.
- **The explorer is read-only.** `read` / `glob` / `grep` / `list` / `lsp`
  allowed; `edit`, `bash`, `webfetch`, `websearch` and `external_directory`
  denied.
- **VERIFY and DEBUG can read and run checks but not modify the tree.** `bash`
  is a minimal allowlist of read-only git commands and common test, typecheck,
  lint and build commands for several languages, with `git commit*` and
  `git push*` explicitly denied.
- **BUILD and DOCS** keep normal edit/test ability; only `task` is denied.

All of this is data in `config/permissions.yaml`; profiles can be edited or
replaced per project.

## Security and secrets

- OpenCode Gear ships **no credentials**. Provider credentials are managed by
  OpenCode (`opencode auth login`) and stay in OpenCode's own credential store.
- Do not put tokens, API keys, cookies or private keys in an override file or
  in prompts. Use OpenCode's own credential store (`opencode auth login`) for
  provider keys. If a provider needs a variable name in config, declare the
  variable in the provider's `env` list and keep the value outside the
  repository.
- The repository's own tests scan the tree for common credential shapes,
  private-key headers and absolute home paths. Run `make test` before you push
  a fork.
- `GH_TOKEN` / `GITHUB_TOKEN` are read from the environment for OCG's own
  GitHub API requests. They are attached only to `api.github.com`, never
  persisted, never written to telemetry, and never printed (the wrapper's
  `Debug`/`Display` redacts them).
- Proxy URLs and embedded credentials are likewise never rendered in
  `Debug`, errors or diagnostics.
- Override files (`.opencode-gear.yaml`, `~/.config/opencode-gear/config.yaml`)
  are for routing and prompts only. Keep project privacy rules in your
  project's own agent instructions.
- Local telemetry is local-only and never records prompts, source code, command
  strings, command output or headers. Metadata that looks like a credential is
  redacted before it is written. See [docs/telemetry.md](docs/telemetry.md).

## Observability (optional)

Disabled by default. When enabled, `ocg` appends one JSON line per launch to a
local file — never to a remote service:

```json
{"ts":"...","event":"launch","throttle":"mid","default_agent":"lead-mid",
 "lead":"openai/gpt-5.6-sol",
   "routing":{"explore":"volcengine-coding-plan/kimi-k2.7-code","build":"opencode-go/deepseek-v4.1-flash"}}
```

- Local-only; there is no remote telemetry.
- No prompts, no source code, no credentials.
- Records routing decisions (`role`, `model`, `provider`, `throttle`) only.
  Success/failure, retry counts and duration are **not** captured — that needs
  a session-level hook, which this project deliberately does not ship yet.
- Enable with `"observability": {"enabled": true, "path": "~/..."}`, or point
  `OPENCODE_GEAR_TRACE` at a file. Delete the file to delete the history.

## Repository context (optional)

`ocg` can build a deterministic, local description of a project: a repository
map, an incremental symbol index, a bounded git diff summary and a ranked
context plan. It is **local only** — no network, no telemetry, no embeddings
and no paid API.

```bash
ocg context fix the failing parser        # human-readable plan
ocg context --pretty summarise the cache   # full plan as JSON
ocg context symbols parse                  # symbol/definition diagnostic
ocg cache stats                           # cache + index status
ocg cache clean                           # clear the context cache only
ocg stats                                  # local telemetry aggregate
```

**Integration boundary.** Context is produced on demand through `ocg context` or
the library API, and it is also the substrate for optional orchestration:
when `orchestration.enabled` is true, an ordinary `ocg` / `ocg run` launch
materializes a thin generated OpenCode plugin adapter and exports the bridge
environment, so the running session can request bounded dynamic context and
role hand-offs. The adapter performs no ranking or policy itself; every
decision stays in the `ocg` Rust bridge (see
[Orchestration](#orchestration-optional)). With orchestration disabled (the
`OPENCODE_GEAR_ORCHESTRATION=0` escape hatch or `"orchestration":
{"enabled": false}`) no plugin is emitted and launch writes no orchestration
state. Repository context and index state may be built or updated by explicit
`ocg context` commands and by ordinary orchestration bridge context requests.
The subsystem fails soft: a failed cache write or a bounded git capture only
produces a warning, never a lost plan.

Set `"context": {"enabled": false}` to disable it entirely. Then `ocg context`
prints an informational message, returns success, and never reads, indexes or
caches anything.

The walker is bounded: `context.maxRepositoryFiles` (default `100000`, max
`5000000`) is a hard cap on files scanned and indexed. When it is reached, the
repo map, index and plan are marked truncated with an honest note instead of
claiming a complete map; git status is collected separately, so `changed_paths`
stays complete.

State layout, alongside the managed runtime:

```text
<project>/.opencode-gear/
  runtime/opencode/...        managed OpenCode (untouched by cache clean)
  index/context-index.json    inspectable, incrementally updated symbol index
  cache/context/*.json        fine-grained plan cache
  logs/*.log                  raw verification logs (never swept by cache clean)
  checkpoints/*.json          phase checkpoints (inspectable JSON)
  orchestration/state.json    bounded controller state (phases, attempts, findings)
  orchestration/plugin/       generated OpenCode JS adapter (file:// plugin)
  telemetry/events.jsonl      local-only telemetry events (inspectable JSONL)
```

Sensitive files (`.env`, `.envrc`, key material, `id_rsa`/`id_ed25519`,
`credentials*`, `secrets*`, `auth*`, `token*`, known OpenCode credential
locations, ...) are never read, parsed, sliced or cached — only their path
metadata is indexed. `ocg cache clean` removes `cache/` only, never the
runtime, the index, the verification logs, the checkpoints or the telemetry.
Creating any state adds `.opencode-gear/` to the project `.gitignore` once,
using the same idempotent helper the runtime uses.

The plan carries a stable conceptual section order — gear instructions, project
policy, repository map, capability/tool descriptions, task capsule, relevant
symbols/source, current git diff, verification state — and embeds the advisory
capability plan and targeted-test proposal. Task capsules and phase checkpoints
are structured, versioned JSON; `ocg checkpoint list|show|save` inspects them.

### Local telemetry and stats

`ocg context` and `ocg verify` append one small JSON object per run to
`<project>/.opencode-gear/telemetry/events.jsonl`: byte sizes, estimated token
counts, index/cache hits, verification attempts and outcomes, and durations.
It is **local only** — no upload, no model API — and never records a prompt,
source code, command string or command output. `ocg stats` reads it offline:

```bash
ocg stats            # project aggregate + latest event
ocg stats --pretty   # same information as JSON
```

Telemetry is on by default and always local-only. Disable it with
`{"telemetry": {"enabled": false}}` or `OPENCODE_GEAR_TELEMETRY=0`. Missing
telemetry is reported as "no data" and never blocks a command. See
[docs/telemetry.md](docs/telemetry.md) for the schema and privacy model, and
[docs/token-efficiency.md](docs/token-efficiency.md) for one measured example.

### Orchestration (optional)

Orchestration moves a task through Explore → Build → Verify (→ Debug) without
adding a second source of policy. **All ranking, projection, freshness, retry
and telemetry logic lives in Rust**; the JavaScript that OpenCode loads is a
thin, generated adapter. It is off only when you turn it off: set
`"orchestration": {"enabled": false}` or `OPENCODE_GEAR_ORCHESTRATION=0`.

> **Filesystem requirement (fail-fast).** An ordinary `ocg` / `ocg run` is a
> coding session and **creates `<project>/.opencode-gear/` and requires it to be
> writable**. If the adapter cannot be materialized, the launch fails clearly
> rather than injecting a broken `file://` plugin. `OPENCODE_GEAR_ORCHESTRATION=0`
> is the escape hatch when the project directory is read-only. `ocg models` is
> **not** a coding session: it never materializes the plugin, never writes state
> and does not require a writable project directory.

How activation works (uses only supported OpenCode mechanisms):

```text
ocg launch
  ├─ materializes <project>/.opencode-gear/orchestration/plugin/ocg-orchestration.js
  ├─ injects its file:// URL into the generated config `plugin` array
  │    (existing user plugins are preserved; never added twice)
  └─ exports OPENCODE_GEAR_OCG + OPENCODE_GEAR_PROJECT to OpenCode

OpenCode hook                adapter action (no policy)      Rust bridge
  chat.message               append delimited suffix   ──▶  prepare_lead_context
  tool.execute.before(task)  append hand-off to prompt ──▶  prepare_handoff
  tool.execute.after(explore) append bounded summary  ──▶  consume_explore_result
  tool.execute.after(build)  append verification note ──▶  after_build
```

The bridge is a hidden `ocg __bridge <event>` command. The adapter spawns it
with a direct argv and JSON on stdin (never a shell), and swallows any bridge
failure so it can never break a session. A stable delimiter
(`<<<OCG:DYNAMIC_CONTEXT v1>>> … <<<OCG:END>>>`) is appended after the original
prompt, so the user/agent prompt always stays first.

Explore and ExploreDeep hand-offs also append a compact, advisory response
contract asking for one JSON object (`goal`, `constraints`, `findings`,
`files`, `symbols`). It is not a prompt rewrite, and the deterministic fallback
parser still works if a model ignores it.

The `chat.message` hook only injects dynamic context into the **Lead** session
(the request agent starts with `lead-`); worker subagent sessions do not
receive a duplicate Lead context, and a request whose agent cannot be
established is left untouched. The task before/after hooks stay active in every
session. The bridge reads and caps its stdin (4 MiB) before any early
return, so a disabled or rejected payload never produces a BrokenPipe.

Policy, honestly bounded:

- **Typed hand-offs.** The rich planning state (`TaskCapsule` plus selected
  source slices, a bounded diff and optional verification) is projected into a
  compact `ModelHandoffCapsule` for one role transition. Each destination clears
  the fields it does not need: Explore never sees verification failures or raw
  logs, Build sees only high-confidence findings and fix feedback, Verify sees
  the change and verification rather than exploratory narrative, Debug gets only
  failures/evidence/verification/raw logs/the relevant bounded diff, and Docs
  gets the goal, findings, files, decisions and verification. The projection
  keeps required fields,
  drops optional material deterministically and reports what it dropped.
  Selected source is a *separate* dynamic-context block, never part of the
  compact capsule, and it is not appended for a Debug delegation.
- **Bounded (runtime optimization envelope).** Every hand-off is capped by
  `maxHandoffBytes` (default `16384`) and as a percentage of the rich task
  context (`maxHandoffRatioPercent`, default `60`). This is a size optimization,
  **not** a correctness rule: required evidence (goal, hard constraints,
  critical findings, changed files, failing locations) is never dropped to fit.
  The deterministic release fixture deliberately configures a tighter
  `4096` / `40%` regression gate to catch projection regressions.
- **Retry then Debug.** After Build, `ocg` runs the stage configured under
  `verification` (only trusted, structured commands). A pass ends the task with
  no Debug. A failure allows up to `maxBuildRetries` (default `2`) Build
  retries; when the budget is exhausted it recommends Debug with an explainable
  reason (stage, outcome, attempt/retry count, failing-command count and first
  distilled location — **no configured command string and no raw output**). Each
  Debug delegation counts against `maxDebugRetries`; once exceeded, the hand-off
  and dynamic context carry an explicit **user-escalation** instruction instead
  of continuing automatically. The Build→Verify
  hand-off is rebuilt from a refreshed context plan (current changed files,
  symbols and a real bounded diff) plus the verification result. Debug receives
  only failures, evidence, the relevant bounded diff, raw-log references and the
  distilled verification block; returning from Debug to Build is a distinct
  `DebugToBuild` transition with its own checkpoint and fix constraints.
- **Real diff, not a reference.** The context plan preserves the actual
  `DiffSummary` it ranked against; Build/Verify/Debug hand-offs carry a bounded,
  deterministic rendering of its retained entries and hunks (with structural
  truncation stated), filtered for sensitive paths and secret-shaped content.
- **Secrets.** Every hand-off field is sanitized at the projection boundary:
  secret-shaped task/goal/constraint/finding/verification/decision/evidence/
  reference text is omitted or redacted, sensitive paths are dropped, and a
  selected source slice whose *content* looks secret-shaped is dropped whole
  even when its path is innocuous. This reuses the existing path classifier and
  secret detector.
- **Fail-soft.** Corrupt state, checkpoint or cache recovers to empty/rebuild,
  never fatal.
- **Advisory capabilities.** The capability plan is included in the dynamic
  context as advice. Actual runtime permission enforcement remains OpenCode's
  job; this version does **not** claim to sandbox or silently change agent
  permissions.

Configuration:

```yaml
orchestration:
  enabled: true
  maxBuildRetries: 2
  maxDebugRetries: 1
  maxHandoffBytes: 16384
  maxHandoffRatioPercent: 60
```

There is no learned router and no automatic model switching. `ocg doctor`
reports orchestration health read-only.

## Verification, checkpoints and capabilities (optional)

`ocg` can run **only explicitly configured** checks, distill their output and
record phase checkpoints. It never discovers a command from a manifest and never
executes model output. All three subsystems are local and deterministic.

```bash
ocg verify normal                 # run the configured 'normal' commands
ocg verify full --pretty          # structured JSON report
ocg tools "write a SQL migration" # advisory capability plan
ocg checkpoint list
ocg checkpoint show <id>
ocg checkpoint save --phase verify-to-debug --task "fix parser"
```

Configured commands are structured `program` + `args`. A convenience string
such as `"cargo check"` is parsed by a strict, shell-free word splitter: shell
control operators, pipelines, redirections and substitutions are rejected, and
shell-interpreter escape hatches (`sh -c`, `bash -lc`, `cmd /c`,
`powershell -Command`, ...) plus control characters are rejected in both forms.
Nothing runs without configuration — all stages start empty.

```json
{
  "verification": {
    "defaultStage": "normal",
    "stopOnFailure": true,
    "maxRawLogBytes": 2000000,
    "maxLogStorageBytes": 52428800,
    "stages": {
      "fast": { "commands": ["cargo fmt --check"] },
      "normal": { "commands": ["cargo check", "cargo test"] },
      "full": {
        "commands": ["cargo clippy --all-targets --all-features -- -D warnings"]
      }
    }
  },
  "capabilities": { "enabled": true, "custom": [] }
}
```

- **Log distillation** is deterministic: it removes progress noise, collapses
  exact duplicate lines and repeated blocks, groups identical failures and
  extracts errors, warnings, source locations, failed tests and test counts.
  Counts are only reported when a known summary shape parses cleanly; they are
  never inferred from an exit status.
- **Raw logs** live under `.opencode-gear/logs/`, are bounded by
  `verification.maxRawLogBytes` per stream and pruned to
  `verification.maxLogStorageBytes` in total. Names are collision-resistant
  (second + pid + monotonic sequence + content hash), pruning never deletes the
  log the current report references, they stay inspectable and `ocg cache clean`
  never removes them. Set `verification.enabled: false` to disable automation
  entirely.
- **Capture** drains stdout and stderr concurrently to EOF with a bounded
  retained prefix: verbosity is never a false failure and the command's own exit
  status stays authoritative. Truncation is stated explicitly. There is no
  timeout yet (documented limitation).
- **Targeted test selection** proposes candidate tests from changed files,
  naming conventions and **index name matches** (same-name indexed symbols, not
  proof of a textual reference). Every proposal is `complete = false` with
  explicit evidence: an unselected test may still fail, and when nothing matches
  the proposal reports the configured fallback stage.
- **Capabilities / Tool Context Firewall** are a deterministic context and
  config plan, *not* a security sandbox. A Git-only task exposes
  `filesystem` + `git` only, a docs lookup exposes `filesystem` +
  `documentation`/`web`, a DB task exposes `filesystem` + `database`, generic
  coding exposes `filesystem` (adding `git` only with evidence) and an unknown
  task never exposes `cloud`, `browser` or `database`. `capabilities.enabled:
  false` disables planning entirely: no capability is allowed or exposed, and
  `ocg tools` / the context plan report the disabled state. The mode does not
  activate runtime tool schemas: OpenCode owns execution, conversation,
  provider and tool semantics.
- **Checkpoints** are versioned JSON with a task capsule, Git state
  fingerprint, verification state, provenance, decisions and `created_at`.
  Loading checks the schema and revalidates sources and Git identity: a stale
  checkpoint is marked stale and is never silently reused; a corrupt one is
  reported and ignored without ever blocking normal `ocg`.

`ocg verify` is a deterministic complement to the independent VERIFY agent, not
a replacement. The Lead prompt tells the model to prefer it for routine
mechanical checks before spending verifier tokens.

## Troubleshooting

**`ocg: configuration is invalid`** — run `ocg validate`; the builder refuses to
emit an invalid config, and the error names the offending key.

**`opencode` is not found or is too old** — `ocg` resolves a runtime in the
order documented in [Managed runtime](#managed-runtime). Run `ocg doctor` to
see the resolved source, then either install OpenCode (>= 1.18.0), let `ocg`
bootstrap a project-local runtime, or point at an explicit executable:

```bash
export OPENCODE_GEAR_OPENCODE=/path/to/opencode
```

The Commit-1 `OPENCODE_GEAR_OPENCODE_BIN` and legacy `OC_GEAR_OPENCODE_BIN`
names still work. A broken explicit path is an error, not a fallback.

**Provider/model errors on launch** — the model names and provider ids move
faster than this README. Check what your OpenCode actually exposes:

```bash
ocg models                       # everything OpenCode can see with this config
opencode models openai
```

Then update `config/models.yaml` (or your user override).

**`volcengine-coding-plan` model rejected as unsupported** — ensure the
Volcengine Ark Coding Plan provider is authenticated via `/connect` (or
`opencode auth login`) and that the Kimi models are visible in the runtime
catalogue (`ocg doctor`). OCG now references the native provider.

**A worker ignores its read-only permission** — permissions are OpenCode
agent config, not prompt text. Confirm the active agent is the generated one
(`ocg build --pretty` and inspect `agent.<name>.permission`), and that your
project does not override it.

**Tab does not cycle the Lead** — the cycle depends on the three `lead-*`
agents and on `keybinds`. Inspect the generated `keybinds` with
`ocg --dry-run`, and note that the order shifts with `default_agent`.

**Overrides seem ignored** — `ocg layers` shows exactly which files were found.
Check `OPENCODE_GEAR_USER_CONFIG` / `OPENCODE_GEAR_PROJECT_CONFIG` (or their
legacy names) are not pointing somewhere unexpected.

**`unsupported JSON ... file`** — OpenCode Gear 0.3 reads YAML only. The error
names the stale `.json` file and its YAML target. Convert it to `config.yaml` /
`.opencode-gear.yaml` and delete the JSON file; existing JSON is never migrated
or merged. `ocg init` refuses to create a project YAML while
`.opencode-gear.json` exists.

## Repository layout

```text
opencode-gear/
  Cargo.toml             crate manifest; builds the `ocg` binary
  src/main.rs            binary entry point
  src/lib.rs             library root
  src/cli.rs             argument parsing, environment, dispatch
  src/config.rs          layered configuration and paths
  src/yaml.rs            YAML parsing/serialization for layered config
  src/defaults.rs        embedded defaults + optional on-disk gear home
  src/model.rs           model registry and role resolution
  src/prompt.rs          frontmatter, append and Lead rendering
  src/validate.rs        whole-configuration validation
  src/build.rs           deterministic OpenCode config generation
  src/report.rs          status / routing / layers output
  src/observability.rs   opt-in local routing trace
  src/process.rs         the single place that executes a child process
  src/platform.rs        OS/arch normalization and release asset mapping
  src/clock.rs           injectable time source
  src/http.rs            blocking HTTP transport abstraction + reqwest impl
  src/context/           deterministic local context engine (repo map, index,
                         symbols, git diff, ranking, cache, capsules)
  src/verification/      explicit verification, log distillation, test selection
  src/orchestration/     typed hand-offs, Rust controller/state, checkpoints,
                         generated OpenCode plugin adapter and hidden bridge
  src/telemetry/         local-only JSONL events, token provenance, stats
  src/runtime/           managed OpenCode runtime (policy, resolve, install,
                         cache, release, archive, self-update)
  install.sh             portable POSIX installer
  scripts/package-release.sh  local artifact + SHA256SUMS packaging
  .github/workflows/     release build for the four supported targets
  config/base.yaml        shared OpenCode config (providers, disabled built-ins)
  config/models.yaml      model/provider registry
  config/throttle.yaml    throttle level -> Lead model + reasoning variant
  config/routing.yaml     role -> model + reasoning variant
  config/permissions.yaml agent isolation profiles
  config/prompts/*.md     one prompt per role
  examples/               override file examples
  tests/                  Rust unit and integration tests + installer shell tests
  docs/                   architecture, configuration, telemetry, verification,
                          token efficiency, migration, troubleshooting
  Makefile               `make test`, `make check`, `make validate`, `make package`
```

## Tests

```bash
make test        # cargo test + the installer shell tests
make check       # cargo fmt --check + clippy -D warnings + make test
make validate    # validate the shipped configuration
make package     # build the current platform artifact + SHA256SUMS locally
```

The Rust suite covers configuration parsing, throttle selection, Lead
selection, EXPLORE (normal and deep), BUILD, VERIFY, DEBUG, provider binding,
missing-provider and missing-model behaviour, variant validation, override
precedence, arbitrary roles, prompt appending, isolation rules, the opt-in
trace, embedded-vs-disk parity, and repository hygiene (no private tokens, no
credential shapes, no absolute home paths).

The context suite is deterministic and offline. It covers polyglot repo maps,
build-directory and binary/oversized exclusion, incremental index reuse and
update, Rust/TypeScript/JavaScript/Python symbol extraction, git
modified/added/deleted/renamed diffs (skipped when `git` is unavailable),
bounded hunks, ranking limits and stable ordering, fine-grained cache
invalidation, corrupt-cache recovery, capsule round-tripping and sensitive
content exclusion.

The runtime suite uses an in-memory HTTP transport, a fake process host, a
fixed clock and temporary directories to cover platform mappings, the four
runtime sources, explicit/managed/system/missing resolution, fresh/expired/
forced update checks, upgrade success and failure fallback, incompatible
system runtimes, pins, safe install cleanup and atomic activation, Gear
self-update checksum verification, and the `version`/`doctor`/`upgrade`
commands. `tests/installer_test.sh` exercises `install.sh` offline with local
fixtures.

The telemetry and state suites are deterministic and offline: token provenance
(reported, estimated and unknown kept distinct), aggregation, corrupt-JSONL
recovery, secret redaction, `ocg stats` output and read-only behavior,
`ocg doctor` state checks, and a reproducible context/log measurement fixture.

### Development workflow

```bash
cargo fmt
cargo clippy --all-targets --all-features -- -D warnings
cargo test
sh tests/installer_test.sh
```

The runtime tests never touch the network or the real home directory.

## Migrating from an older Gear/profile setup

Older setups often used one word — "Gear" — for two different things: the
whole model bundle and the Execution Tier. That made two questions impossible to
answer independently.

The new model is:

```text
Throttle        = Execution Tier only            (low / mid / high)
Worker Router   = delegated execution routing    (EXPLORE / BUILD / VERIFY / DEBUG)
```

Practical mapping:

| Old concept | New home |
| --- | --- |
| "Gear" as the Execution Tier | Throttle (`low` / `mid` / `high`) |
| "Gear" as the full model bundle | removed; worker roles are fixed and independent |
| Old per-gear duplicate worker agents | one worker agent per role, shared by all Leads |
| Per-gear explorer/builder/verifier model swaps | `config/routing.yaml` |
| "Mode" / provider-scope presets | project or user override files |
| `oc use <mode>` interactive switching | `ocg <level>` / `ocg --throttle <level>` / Tab in the TUI |

If you have an existing `oc` that switched whole profiles, keep it working by
leaving it on your `PATH` under a different name while you migrate, or map its
profile names onto override files. See `docs/migration.md` for a step-by-step
walkthrough.

---

## Further documentation

| Document | Contents |
| --- | --- |
| [docs/architecture.md](docs/architecture.md) | The two axes, resolution pipeline, invariants, extension points |
| [docs/configuration.md](docs/configuration.md) | Every registry, override shape, environment variable and command |
| [docs/verification.md](docs/verification.md) | Verification, log distillation, test selection, capabilities/firewall, checkpoints, stable ordering |
| [docs/telemetry.md](docs/telemetry.md) | Telemetry schema, privacy model, token provenance, orchestration accounting, `ocg stats`, doctor checks, deferred boundaries |
| [docs/token-efficiency.md](docs/token-efficiency.md) | Recorded deterministic context, log-distillation and orchestration hand-off measurements |
| [docs/migration.md](docs/migration.md) | Step-by-step migration from a whole-bundle Gear/profile setup |
| [docs/troubleshooting.md](docs/troubleshooting.md) | Failure modes and how to diagnose them |

## License

MIT. See [LICENSE](LICENSE).

OpenCode Gear is a community project and is not affiliated with, or endorsed
by, OpenCode, OpenAI, Volcano Engine, DeepSeek, Zhipu/GLM or Moonshot/Kimi.
All product names are trademarks of their respective owners.

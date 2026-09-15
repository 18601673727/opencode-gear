# OpenCode Gear

**OC Gear** — project-agnostic multi-model orchestration for OpenCode.

OpenCode Gear is a small configuration layer for [OpenCode](https://opencode.ai)
that separates two things a lot of agent setups accidentally fuse together:

```text
THROTTLE
    how strong (and how expensive) the OpenAI Lead is
        low  /  mid  /  high

CONSUMER ROUTER
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
- [Requirements](#requirements)
- [Install](#install)
- [Quick start](#quick-start)
- [Throttle semantics](#throttle-semantics)
- [Consumer Router semantics](#consumer-router-semantics)
- [Provider and model mapping](#provider-and-model-mapping)
- [Configuration and overrides](#configuration-and-overrides)
- [Commands](#commands)
- [Isolation and permissions](#isolation-and-permissions)
- [Security and secrets](#security-and-secrets)
- [Observability (optional)](#observability-optional)
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
OpenAI Lead                      ← throttle picks the tier
 │
 ├── understand
 ├── reason
 ├── decompose
 ├── decide
 ├── delegate ────────────────┐
 └── accept / reject results  │
                              ▼
                     Consumer Router        ← config/routing.json
                              │
      ┌───────────────┬───────┴────────┬────────────────┐
      ▼               ▼                ▼                ▼
   EXPLORE          BUILD           VERIFY        HARD VERIFY /
   Kimi K2.7 / K3   DeepSeek V4.1   GLM-5.3       DEBUG
   (Volcano)        Flash (Go)      Flash (Go)    GLM-5.3 (Go)
```

`oc` resolves the configuration in layers, then launches `opencode` with the
merged result:

```text
OpenCode Gear defaults  (this repository)
        ↓
user config             (~/.config/opencode-gear/config.json)
        ↓
project config          (<project>/.opencode-gear.json)
        ↓
CLI / environment       (oc --throttle high, OC_GEAR_THROTTLE)
```

Nothing is written back into the repository or into the project. The only
persisted state is the default throttle level, and only when you ask for it
with `oc throttle <level>`.

OpenCode binds exactly one model per agent, so the gear materialises:

- one **Lead agent per throttle level** (`lead-low`, `lead-mid`, `lead-high`)
  so the TUI can switch tiers live, and
- one **consumer agent per role** (`ocg-explore`, `ocg-build`, ...) shared by
  all Lead levels.

Because the consumer agents do not depend on the throttle, switching throttle
during a session cannot silently re-route BUILD or VERIFY.

## Requirements

| Requirement | Notes |
| --- | --- |
| OpenCode | 1.18.x or newer. `oc` uses `OPENCODE_CONFIG_CONTENT`. |
| Python 3.9+ | Standard library only. Used by `bin/oc_config.py`. |
| Bash | `bin/oc` is a POSIX-ish bash script. |
| OpenAI plan/credentials | For the Lead (`openai` provider). |
| Volcano Coding Plan Pro | For the EXPLORE models (Kimi). Declared in `config/base.json`. |
| OpenCode Go | For BUILD / VERIFY / DEBUG (DeepSeek and GLM). |

Authenticate the providers once with OpenCode itself:

```bash
opencode auth login openai
opencode auth login volcengine-coding
opencode auth login opencode-go
```

The exact provider ids depend on your OpenCode build; check the list with
`opencode auth login` or `oc models`.

## Install

Global install (recommended):

```bash
git clone https://github.com/18601673727/opencode-gear.git ~/.local/share/opencode-gear
ln -s ~/.local/share/opencode-gear/bin/oc ~/.local/bin/oc   # ~/.local/bin must be on PATH
oc validate
```

`oc` follows symlinks, so the symlink above is enough. If you prefer an
explicit location, set `OC_GEAR_HOME`:

```bash
export OC_GEAR_HOME=~/code/opencode-gear
```

Per-project vendoring (useful if you want the gear pinned inside a repo):

```bash
git clone https://github.com/18601673727/opencode-gear.git <project>/.opencode-gear
<project>/.opencode-gear/bin/oc --project <project> --dry-run
```

## Quick start

```bash
oc status                  # throttle, routing and the config layers in use
oc routing                 # role -> model table
oc --throttle mid          # interactive session with the mid Lead
oc run "summarise the failing tests"   # one-shot run
oc --dry-run               # print the merged OpenCode config, launch nothing
```

Run these from any project directory. OpenCode starts in the current working
directory, and project-local overrides are read from `.opencode-gear.json`
there.

## Throttle semantics

Throttle selects the OpenAI Lead tier only.

| Level | Lead | Reasoning | Intent |
| --- | --- | --- | --- |
| `low` | `openai/gpt-5.6-sol` | `medium` | Default. Economical Lead. |
| `mid` | `openai/gpt-5.6-sol` | `high` | Stronger reasoning on the same model. |
| `high` | `openai/gpt-6-astra` | `high` | Premium Lead, larger context. |

Notes:

- The model ids and reasoning variants are the ones the providers actually
  expose, and they are centralised in `config/models.json` and
  `config/throttle.json`. Swap a model there and nothing else changes.
- If a provider does not expose a reasoning variant, omit `variant` and the
  provider default is used.
- **Throttle never decides which consumer handles EXPLORE / BUILD / VERIFY.**
- Precedence: `--throttle` > `OC_GEAR_THROTTLE` > project config > user config
  > `config/throttle.json` default.

Persist a default (writes `~/.config/opencode-gear/config.json`):

```bash
oc throttle mid
oc throttle          # print the resolved default
```

## Consumer Router semantics

| Role | Responsibility | Read/write |
| --- | --- | --- |
| EXPLORE | Repository reconnaissance, call paths, data flow, impact mapping, context compression for the Lead. Must not start a large rewrite just because it found the affected scope. | read-only |
| EXPLORE (deep) | Very large context, whole-repository reasoning, hard cross-cutting analysis. | read-only |
| BUILD | Implement approved, bounded work; edit code; add/update tests; iterate. | read/write |
| VERIFY | Independently inspect a completed diff against acceptance criteria; look for missed files, API/type/state inconsistencies, regressions. | read + tests |
| DEBUG | Escalation target: difficult debugging, root-cause analysis, architecture-sensitive review, problems BUILD failed to resolve. | read + tests |
| DOCS | Factual closeout reports and handoff notes after work is verified. | read/write |

OpenAI is **not** part of the normal consumer pool. The Lead is the only
OpenAI agent.

### Escalation rules

These are prompt- and policy-level rules, not a scheduler. OpenCode cannot
enforce them mechanically, so the Lead prompt encodes them explicitly:

- **Two-strike handoff.** If the same consumer fails at substantially the same
  problem twice, do not issue a third identical retry. Escalate to DEBUG. If
  DEBUG cannot resolve it either, the Lead takes the problem back and decides.
- **Scope explosion.** If a consumer finds that approved work is much larger
  than expected (a three-file change turning into a schema + backend +
  frontend + migration redesign), it stops, summarises the new scope, and
  returns to the Lead. Use EXPLORE for impact analysis if useful. A consumer
  must never silently redefine the task.
- **Consumer disagreement.** If two consumers disagree on an architectural or
  semantic decision, neither is the final authority: both opinions go to the
  Lead, which decides.

### Fallbacks

A role may declare an optional `fallback` model. Fallbacks are shown in
`oc routing` and injected into the Lead prompt, and are meant for provider
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

Model ids live in `config/models.json`; role assignments live in
`config/routing.json`; throttle levels live in `config/throttle.json`.

## Configuration and overrides

An override file is a partial copy of the gear registries. Everything is
deep-merged, so you only write the keys you want to change. The top-level keys
mirror the files in `config/`: `throttle`, `models`, `routing`, `permissions`,
plus the gear-only extras `prompts`, `observability` and `opencode`.

```jsonc
{
  // pick a different default Lead tier
  "throttle": { "default": "mid" },

  // re-point one role
  "routing": { "roles": { "build": { "model": "glm-5.3", "variant": "max" } } },

  // add or re-point a model (same shape as config/models.json)
  "models": {
    "models": {
      "sol": { "provider": "openai", "id": "gpt-5.6-sol", "label": "GPT-5.6 Sol",
               "variants": ["none", "low", "medium", "high", "xhigh", "max"] }
    }
  },

  // replace a prompt (path, or {"text": "..."})
  "prompts": { "lead": "~/prompts/my-lead.md" },

  // optional local routing trace
  "observability": { "enabled": true, "path": "~/state/oc-gear-events.jsonl" },

  // raw OpenCode config merged into the result last
  "opencode": { "username": "you" }
}
```

To redefine a throttle level, override the `throttle` registry itself:

```json
{ "throttle": { "levels": { "high": { "model": "astra", "variant": "xhigh" } } } }
```

Locations and how to point `oc` at a different file:

| Layer | Default path | Override with |
| --- | --- | --- |
| User | `~/.config/opencode-gear/config.json` | `OC_GEAR_USER_CONFIG` (or `--user-config` on `bin/oc_config.py`) |
| Project | `<project>/.opencode-gear.json` | `OC_GEAR_PROJECT_CONFIG` (or `--project-config` on `bin/oc_config.py`) |

Precedence is project over user over defaults. Validate at any time:

```bash
oc validate
```

Validation catches unknown model keys, unknown providers, reasoning variants
that a model does not expose, missing prompts and malformed fallbacks, and it
refuses to build a config that would otherwise fail at runtime.

Prompt paths may be absolute, or relative — relative paths are resolved against
the project directory first, then the gear's `config/` directory.

## Commands

```text
oc [--throttle low|mid|high] [--project DIR] [--dry-run] [command] [args...]

(none)              launch interactive OpenCode with the gear config
run <args...>       launch `opencode run`
models [args...]    run `opencode models`
status              throttle, routing and config layers
routing             consumer role -> model table
throttle [level]    print, or persist, the default throttle level
validate            validate the merged configuration
layers              show config layers and trace state
version             print the version
help                print usage
```

Inside the TUI, `Tab` / `Shift+Tab` cycle the three Lead agents. The cycle
order depends on the active `default_agent`; the default configuration starts
at `lead-low`. If you do not want the keybind, remove `keybinds` from
`config/base.json` or override it in your project config.

## Isolation and permissions

Enforced through OpenCode agent permissions (`config/permissions.json`), not
just prompt convention:

- **No recursive delegation.** Every consumer has `task: deny`; only a Lead
  orchestrates, so the tree is `User -> Lead -> one level`.
- **The Lead can only reach its own consumers.** `permission.task` is
  `{"*": "deny", "<consumer agents>": "allow"}`.
- **Hidden internals.** Consumers are `hidden: true`, so they stay out of the
  `@` autocomplete while remaining callable by the Lead.
- **The explorer is read-only.** `read` / `glob` / `grep` / `list` / `lsp`
  allowed; `edit`, `bash`, `webfetch`, `websearch` and `external_directory`
  denied.
- **VERIFY and DEBUG can read and run checks but not modify the tree.** `bash`
  is a minimal allowlist of read-only git commands and common test, typecheck,
  lint and build commands for several languages, with `git commit*` and
  `git push*` explicitly denied.
- **BUILD and DOCS** keep normal edit/test ability; only `task` is denied.

All of this is data in `config/permissions.json`; profiles can be edited or
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
- Override files (`.opencode-gear.json`, `~/.config/opencode-gear/config.json`)
  are for routing and prompts only. Keep project privacy rules in your
  project's own agent instructions.

## Observability (optional)

Disabled by default. When enabled, `oc` appends one JSON line per launch to a
local file — never to a remote service:

```json
{"ts":"...","event":"launch","throttle":"mid","default_agent":"lead-mid",
 "lead":"openai/gpt-5.6-sol",
 "routing":{"explore":"volcengine-coding/kimi-k2.7-code","build":"opencode-go/deepseek-v4.1-flash"}}
```

- Local-only; there is no remote telemetry.
- No prompts, no source code, no credentials.
- Records routing decisions (`role`, `model`, `provider`, `throttle`) only.
  Success/failure, retry counts and duration are **not** captured — that needs
  a session-level hook, which this project deliberately does not ship yet.
- Enable with `"observability": {"enabled": true, "path": "~/..."}`, or point
  `OC_GEAR_TRACE` at a file. Delete the file to delete the history.

## Troubleshooting

**`oc: could not build the OpenCode Gear config`** — run `oc validate`; the
builder refuses to emit an invalid config. The error names the offending key.

**`opencode: command not found`** — `oc` runs `opencode` from `PATH`. Set
`OC_GEAR_OPENCODE_BIN=/path/to/opencode`.

**Provider/model errors on launch** — the model names and provider ids move
faster than this README. Check what your OpenCode actually exposes:

```bash
oc models                       # everything OpenCode can see with this config
opencode models openai --verbose
```

Then update `config/models.json` (or your user override).

**`volcengine-coding` model rejected as unsupported** — the Volcano Coding
Plan endpoint accepts coding-plan aliases, which may differ from the raw Ark
catalogue. `config/base.json` declares `glm-5.3`, `glm-5.3-flash`,
`kimi-k2.7-code` and `kimi-k3`; add a model to that provider block if you need
another alias.

**A consumer ignores its read-only permission** — permissions are OpenCode
agent config, not prompt text. Confirm the active agent is the generated one
(`oc --dry-run` and inspect `agent.<name>.permission`), and that your project
does not override it.

**Tab does not cycle the Lead** — the cycle depends on the three `lead-*`
agents and on `keybinds`. Inspect the generated `keybinds` with
`oc --dry-run`, and note that the order shifts with `default_agent`.

**Overrides seem ignored** — `oc layers` shows exactly which files were found.
Check `OC_GEAR_USER_CONFIG` / `OC_GEAR_PROJECT_CONFIG` are not pointing
somewhere unexpected.

## Repository layout

```text
opencode-gear/
  bin/oc                 bash entry point (`oc` command)
  bin/oc_config.py       configuration builder (stdlib-only Python)
  config/base.json        shared OpenCode config (providers, disabled built-ins)
  config/models.json      model/provider registry
  config/throttle.json    throttle level -> Lead model + reasoning variant
  config/routing.json     role -> model + reasoning variant
  config/permissions.json agent isolation profiles
  config/prompts/*.md     one prompt per role
  examples/               override file examples
  tests/                  unit tests + CLI smoke tests
  docs/                   architecture, configuration, migration, troubleshooting
  Makefile               `make test`, `make validate`
```

## Tests

```bash
make test        # python3 -m unittest discover -s tests -v  +  bash tests/test_cli.sh
make validate    # validate the shipped configuration
```

The suite covers configuration parsing, throttle selection, Lead selection,
EXPLORE (normal and deep), BUILD, VERIFY, DEBUG, provider binding,
missing-provider and missing-model behaviour, variant validation, override
precedence, isolation rules, the opt-in trace, and repository hygiene (no
private tokens, no credential shapes, no absolute home paths).

## Migrating from an older Gear/profile setup

Older setups often used one word — "Gear" — for two different things: the
whole model bundle and the Lead tier. That made two questions impossible to
answer independently.

The new model is:

```text
Throttle        = OpenAI Lead tier only        (low / mid / high)
Consumer Router = delegated execution routing  (EXPLORE / BUILD / VERIFY / DEBUG)
```

Practical mapping:

| Old concept | New home |
| --- | --- |
| "Gear" as the Lead tier | Throttle (`low` / `mid` / `high`) |
| "Gear" as the full model bundle | removed; consumer roles are fixed and independent |
| Old per-gear duplicate consumer agents | one consumer agent per role, shared by all Leads |
| Per-gear explorer/builder/verifier model swaps | `config/routing.json` |
| "Mode" / provider-scope presets | project or user override files |
| `oc use <mode>` interactive switching | `oc --throttle <level>`, `oc throttle <level>`, or Tab in the TUI |

If you have an existing `oc` that switched whole profiles, keep it working by
leaving it on your `PATH` under a different name while you migrate, or map its
profile names onto override files. See `docs/migration.md` for a step-by-step
walkthrough.

---

## Further documentation

| Document | Contents |
| --- | --- |
| [docs/architecture.md](docs/architecture.md) | The two axes, resolution pipeline, invariants, extension points |
| [docs/configuration.md](docs/configuration.md) | Every registry, override shape, environment variable and builder command |
| [docs/migration.md](docs/migration.md) | Step-by-step migration from a whole-bundle Gear/profile setup |
| [docs/troubleshooting.md](docs/troubleshooting.md) | Failure modes and how to diagnose them |

## License

MIT. See [LICENSE](LICENSE).

OpenCode Gear is a community project and is not affiliated with, or endorsed
by, OpenCode, OpenAI, Volcano Engine, DeepSeek, Zhipu/GLM or Moonshot/Kimi.
All product names are trademarks of their respective owners.

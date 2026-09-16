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
embedded defaults       config/*.json + config/prompts/*.md (compiled in)
config/base.json        shared OpenCode config (providers, disabled built-ins)
config/models.json      model key -> provider + real model id (+ variants)
config/throttle.json    level -> Lead model key + reasoning variant
config/routing.json     role  -> model key + reasoning variant
config/permissions.json isolation profiles and role -> profile mapping
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

`ocg` never writes to the repository. The only persisted state is the default
throttle level in the user config, and only via `ocg throttle <level>`.

For development and tests, or to load a different set of defaults, set
`OPENCODE_GEAR_HOME` (legacy `OC_GEAR_HOME`) to a directory containing `config/`.
Otherwise the compiled-in defaults are used, which is what makes a released
binary self-contained.

## Why one agent per throttle level

OpenCode binds one model per agent, and the Task tool has no per-call model
parameter. Two consequences:

1. The Lead needs one agent per throttle level (`lead-<level>`), because the
   Lead model differs per level. These are the only visible primary agents, so
   the TUI can cycle them.
2. Because consumers are **not** throttle-dependent, they do not need to be
   duplicated per level. There is exactly one `ocg-build`, one `ocg-verify`,
   and so on, shared by every Lead. This is the structural expression of "the
   throttle does not route consumers".

## Roles are durable, models are replaceable

```text
LEAD      explore  build  verify  debug  docs      <- durable roles
  │           │       │       │       │      │
OpenAI    Volcano   Go      Go      Go     Go      <- replaceable providers
  │           │       │       │       │      │
Sol/Astra  K2.7/K3  DS4.1   GLMfl   GLM5.3 DS4.1   <- replaceable models
```

- Roles are the abstraction. They appear in `config/routing.json` and in
  `config/permissions.json`.
- Model ids appear only in `config/models.json`.
- Reasoning variants are validated against the `variants` list a model
  declares, so a future `Kimi K4` or `DeepSeek V5` is a config edit, not a
  rewrite.
- Roles are not hardcoded. Adding a role means adding a routing entry, a
  prompt, and (optionally) a permission profile binding. The generator creates
  an `ocg-<role>` consumer and substitutes `{{role}}` placeholders in the Lead
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
     └─ consumer (subagent, task: deny, hidden)
```

- Consumers cannot delegate (`task: deny`), so the tree is one level deep.
- The Lead's `permission.task` is `deny` by default with an explicit allow for
  its own consumers.
- EXPLORE is read-only. VERIFY and DEBUG can read and run checks but cannot
  edit. BUILD and DOCS can edit.

## Escalation as policy, not machinery

OpenCode has no scheduler that can enforce "retry twice, then escalate". The
gear therefore encodes the rules in the Lead prompt:

- two-strike handoff to DEBUG,
- scope-explosion stop-and-report,
- consumer disagreement returns to the Lead.

This is honest about what can and cannot be enforced mechanically. If a future
OpenCode release exposes a routing hook, the rules are already written down in
one place (`config/prompts/lead.md`) and `config/routing.json`.

## Extension points

| You want to | Edit |
| --- | --- |
| Use a newer model | `config/models.json` + `config/routing.json` (or an override) |
| Change Lead tiers | `config/throttle.json` |
| Add a specialist role | add a prompt, a routing role, a permission profile binding |
| Pin a project to different models | `<project>/.opencode-gear.json` |
| Add repository-specific Lead policy | `prompts.lead.append` in `<project>/.opencode-gear.json` |
| Replace a prompt for one project | `prompts` override pointing at a file |
| Add raw OpenCode settings | `opencode` key in an override, or `config/base.json` |

## Project policy stays out of the core

The gear prompt is a project-agnostic baseline. Repository-specific rules are
layered on **per project**, never written into the core:

```text
gear default prompt  (config/prompts/lead.md, project-agnostic)
        +
project override     (<project>/.opencode-gear.json -> prompts.lead.append)
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

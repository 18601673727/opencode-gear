# Troubleshooting

## `oc: could not build the OpenCode Gear config`

The builder refuses to emit an invalid config. Run:

```bash
oc validate
```

The errors name the offending key, for example:

- `routing role 'build' references unknown model 'gpt-x'` — the model key is
  not in `config/models.json`.
- `model 'mystery' uses provider 'nope', which is not declared in
  models.providers`.
- `routing role 'build': variant 'impossible' is not exposed by model
  'deepseek-v4.1-flash'`.
- `prompt file for role 'docs' not found: ...`.

## `opencode: command not found`

`oc` runs `opencode` from `PATH`. Install it, or point the gear at it:

```bash
export OC_GEAR_OPENCODE_BIN=/path/to/opencode
```

## Provider or model errors at launch

Model catalogues change faster than documentation. Ask OpenCode what it
actually exposes with the gear config:

```bash
oc models
opencode models openai --verbose
opencode models volcengine-coding --verbose
```

Then update `config/models.json` (or your override). Reasoning variants are
listed under `variants`; if a model has none, omit `variant`.

## `volcengine-coding` rejects a model as unsupported

The Volcano Coding Plan endpoint accepts coding-plan aliases, which are not
always the same as the raw public catalogue. `config/base.json` declares the
models this project expects for that provider:

```json
"models": {
  "glm-5.3": { "name": "GLM-5.3" },
  "glm-5.3-flash": { "name": "GLM-5.3-Flash" },
  "kimi-k2.7-code": { "name": "Kimi K2.7" },
  "kimi-k3": { "name": "Kimi K3" }
}
```

If your plan exposes a different alias, add it to that block and to
`config/models.json`.

## A consumer ignores its read-only permission

Permissions are OpenCode agent config, not prompt text.

```bash
oc --dry-run | python3 -m json.tool | less
```

Confirm the agent's `permission` block, and check that your project does not
override `permissions` or `opencode.agent`.

## Tab does not cycle the Lead

The cycle depends on the three `lead-*` agents and on `keybinds` (shipped in
`config/base.json`). Two things to know:

- The order depends on `default_agent`, because OpenCode starts the cycle from
  the default agent. With the default `lead-low` startup, `Tab` runs
  low → mid → high.
- If you override `keybinds`, your override wins. Inspect the resolved value
  with `oc --dry-run`.

## Overrides seem to be ignored

```bash
oc layers
```

prints every layer and whether it was found. Common causes:

- `OC_GEAR_USER_CONFIG` / `OC_GEAR_PROJECT_CONFIG` point somewhere unexpected.
  An explicitly configured path wins over directory discovery.
- You edited the gear's `config/` but a project override re-sets the same key.
- The override is not valid JSON. `oc validate` reports the parse error.

## The Lead delegates too much (or too little)

Delegation policy is prompt policy. Edit `config/prompts/lead.md`, or replace
it per project with a `prompts` override. Remember that prompts shape
behaviour; they do not enforce it.

## My project policy is not in the Lead prompt

Project policy is applied through the project override. Check:

```bash
oc layers                      # is the project layer [found]?
oc --dry-run | python3 -c "import json,sys; print(json.load(sys.stdin)['agent']['lead-low']['prompt'])" | tail -40
```

`oc` reads `<cwd>/.opencode-gear.json`; run it from the project root, or point
`OC_GEAR_PROJECT_CONFIG` at the file. If the override sets
`prompts.lead.path`, it **replaces** the gear prompt, so append instead:

```json
{ "prompts": { "lead": { "append": ["path/to/policy.md"] } } }
```

## `opencode run --agent <consumer>` does not run the consumer

OpenCode only accepts a **primary** agent in `--agent`; a subagent is rejected
or ignored (`default agent "..." is a subagent`). Consumers are reached through
the Lead's Task tool, which is the normal path. To smoke-test routing, ask the
Lead to delegate explicitly, for example:

```bash
oc run 'Call the task tool once with subagent_type "ocg-build" and prompt "reply OK".'
```

You can confirm which model actually ran by checking the OpenCode log or the
session store for the `providerID` / `modelID` / `variant` of the assistant
message.

## I want to pin the whole project to different models

Create `<project>/.opencode-gear.json`:

```json
{
  "routing": {
    "roles": {
      "build": { "model": "glm-5.3", "variant": "max" },
      "verify": { "model": "glm-5.3", "variant": "high" }
    }
  }
}
```

Project configuration is not committed anywhere by the gear; whether you commit
it is your project's decision.

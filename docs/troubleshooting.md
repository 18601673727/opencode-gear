# Troubleshooting

## `ocg: configuration is invalid`

The builder refuses to emit an invalid config. Run:

```bash
ocg validate
```

The errors name the offending key, for example:

- `routing role 'build' references unknown model 'gpt-x'` — the model key is
  not in `config/models.json`.
- `model 'mystery' uses provider 'nope', which is not declared in
  models.providers`.
- `routing role 'build': variant 'impossible' is not exposed by model
  'deepseek-v4.1-flash'`.
- `prompt file for role 'docs' not found: ...`.

## `opencode` is missing or too old

`ocg` resolves a runtime in a fixed order: explicit executable, existing
managed project runtime, compatible system `opencode` on `PATH`, then a
project-local bootstrap. Diagnose it with:

```bash
ocg doctor
ocg version
```

To use a specific executable, set the canonical variable:

```bash
export OPENCODE_GEAR_OPENCODE=/path/to/opencode
```

The Commit-1 `OPENCODE_GEAR_OPENCODE_BIN` and legacy `OC_GEAR_OPENCODE_BIN`
names keep working. A broken explicit path is an error rather than a silent
fallback.

A compatible system runtime is upgraded with OpenCode's own
`opencode upgrade` when a check is due. Setting `runtime.autoUpgrade` to
`false` disables that optional upgrade, but the required project-local
fallback still runs: a missing or incompatible runtime is bootstrapped rather
than left unusable.

## Update checks seem stuck or slow

Update checks are cached for `runtime.checkIntervalHours` (default 24) under
the platform cache directory. Successful and failed checks are both recorded,
so a runtime that just failed to upgrade is not retried on every launch. To
force a check:

```bash
ocg upgrade
```

To reset the cache, delete `~/.cache/opencode-gear/` on Linux or
`~/Library/Caches/opencode-gear/` on macOS. A failed check prints a warning
and keeps the working runtime.

## A managed install refuses to download

Managed installs fail closed: a release asset without a `sha256:` digest is
refused rather than installed unverified. This should not happen for official
OpenCode releases; check `ocg doctor` and the GitHub release metadata if it
does.

## `ocg upgrade` did not change my system OpenCode

For a system runtime, `ocg upgrade` runs `opencode upgrade` and never installs
a managed copy or downgrades a newer system runtime. If `opencode upgrade`
fails, the old compatible runtime is kept with a warning. A system runtime
that is still incompatible falls back to a managed install.

## A pinned OpenCode version will not move

That is intended. `runtime.version` is an exact pin: it never advances, is
never replaced by the system runtime, and is preserved by `ocg upgrade`.
Remove the `version` field to return to the `latest` channel.

## `ocg upgrade` could not self-update

Self-update downloads the Gear binary and `SHA256SUMS` from this repository's
releases and verifies the checksum before replacing the running executable. If
it fails, the installed CLI is left untouched and the error is printed; the
OpenCode runtime maintenance still runs. Re-run later, or reinstall with
`install.sh`.

## Managed runtime left behind

Delete the project's `.opencode-gear/` directory to remove a project-local
runtime and its `active.json`. Delete the platform cache directory to remove
update-check state.

## Provider or model errors at launch

Model catalogues change faster than documentation. Ask OpenCode what it
actually exposes with the gear config:

```bash
ocg models
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
ocg build --pretty | less
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
  with `ocg --dry-run`.

## Overrides seem to be ignored

```bash
ocg layers
```

prints every layer and whether it was found. Common causes:

- `OPENCODE_GEAR_USER_CONFIG` / `OPENCODE_GEAR_PROJECT_CONFIG` (or their legacy
  `OC_GEAR_*` names) point somewhere unexpected. An explicitly configured path
  wins over directory discovery.
- You edited the gear's `config/` but a project override re-sets the same key.
  Note that a released binary embeds its defaults; to load `config/` from a
  directory at runtime, set `OPENCODE_GEAR_HOME`.
- The override is not valid JSON. `ocg validate` reports the parse error.

## The Lead delegates too much (or too little)

Delegation policy is prompt policy. Edit `config/prompts/lead.md`, or replace
it per project with a `prompts` override. Remember that prompts shape
behaviour; they do not enforce it.

## My project policy is not in the Lead prompt

Project policy is applied through the project override. Check:

```bash
ocg layers                      # is the project layer [found]?
ocg build --pretty | less       # inspect agent.lead-low.prompt
```

`ocg` reads `<cwd>/.opencode-gear.json`; run it from the project root, or point
`OPENCODE_GEAR_PROJECT_CONFIG` at the file. If the override sets
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
ocg run 'Call the task tool once with subagent_type "ocg-build" and prompt "reply OK".'
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

## `ocg verify` runs nothing

Expected by default: all stages start empty and no command is discovered from a
manifest. Configure the commands you trust, for example:

```json
{
  "verification": {
    "stages": { "normal": { "commands": ["cargo check", "cargo test"] } }
  }
}
```

`ocg verify` reports `not_run` with an explanatory note when no command is
configured, and when `verification.enabled` is false.

## `ocg verify` rejects my command

The command string is parsed without a shell, so shell syntax is refused:
control operators (`;`, `&&`, `||`), pipelines (`|`), redirections (`>`, `<`)
and substitutions (`` ` ``, `$(...)`) are errors. Split the work into several
commands, or use the explicit object form
`{"program": "...", "args": ["..."]}`. `ocg validate` reports the exact
problem.

## `ocg verify` rejects my shell invocation

Shell-interpreter escape hatches are refused in both the string and object
forms: `sh`/`bash`/`dash`/`zsh`/`ksh`/`ash`/`busybox` with `-c`, `cmd[.exe]`
with `/c` or `/k`, and `powershell`/`pwsh` with `-Command` or an
encoded-command switch. Control characters and newlines in the program or
arguments are also rejected. Put the logic in a script file and execute that
path directly (`{"program": "./tools/check.sh"}`), or configure the real
program and arguments.

## Unknown options now error

Only `checkpoint` accepts subcommand options such as `--phase` or `--task`.
Every other command keeps a strict parse: `ocg validate --bogus`,
`ocg status --bogus` and friends exit with status 2. Global options
(`--pretty`, `--project`, `--throttle`, ...) still work anywhere. `ocg verify`
accepts at most one positional stage argument.

## A checkpoint is marked stale

That is the intended behaviour. A checkpoint revalidates the source
fingerprints and the Git state it was built from. If a source changed or the
working tree moved, it is marked stale with reasons instead of being silently
reused. Recompute the capsule and save a new checkpoint.

## Where did my raw verification logs go?

They are under `<project>/.opencode-gear/logs/`. They are pruned to
`verification.maxLogStorageBytes` (oldest first) and `ocg cache clean` never
removes them. Removing `.opencode-gear/` removes them with the rest of the local
state.

## `ocg stats` says "no telemetry events recorded yet"

That is the intended output when no event exists yet. `ocg stats` is read-only
and never creates `.opencode-gear/`. Run `ocg context <task>` or
`ocg verify <stage>` first, or check that collection is enabled:

```bash
ocg stats --pretty        # inspect enabled/local_only/path
ocg doctor                # read-only telemetry check line
```

If `enabled` is `no`, either the project config sets
`{"telemetry": {"enabled": false}}` or `OPENCODE_GEAR_TELEMETRY=0` is set.

## `telemetry.localOnly=false` is rejected

There is no remote telemetry mode. `ocg validate` reports it and `ocg context` /
`ocg verify` disable collection with a warning. Remove the override or set
`localOnly` to `true`.

## `ocg stats` reports corrupt telemetry lines

A partially written or hand-edited line is skipped and counted; good events
still load and later writes still succeed. `ocg doctor` shows the same corrupt
count as a warning. To start over, delete
`<project>/.opencode-gear/telemetry/events.jsonl` (or the whole
`.opencode-gear/telemetry/` directory); nothing else depends on it.

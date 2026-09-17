# Verification, log distillation, test selection, capabilities and checkpoints

This document describes the deterministic verification, distillation, test
selection, capability planning and checkpoint subsystems added on top of the
repository context engine. Everything here is **local, offline and
deterministic**. No part of it calls a model, a network service or a paid API,
and none of it executes model output.

The one-line rule: **OpenCode owns execution, the conversation, provider
semantics and tool semantics. `ocg` produces plans and artifacts and runs only
explicitly configured checks.**

## Verification

### Trusted commands only

A verification command is always a structured program plus arguments:

```json
{"program": "cargo", "args": ["clippy", "--all-targets", "--all-features", "--", "-D", "warnings"]}
```

The convenient string form (`"cargo check"`) is parsed by a strict word
splitter that never invokes `sh -c`. Single and double quotes and backslash
escapes are supported; shell control operators (`;`, `|`, `&`, `<`, `>`,
backticks, `$`) are rejected outside quotes. Quoted operators are literal
arguments, exactly like the object form.

Both forms are also checked for shell-interpreter escape hatches: `sh` / `bash`
/ `dash` / `zsh` / `ksh` / `ash` / `busybox` with a `-c`-style cluster, `cmd[.exe]`
with `/c` or `/k`, and `powershell` / `pwsh` with `-Command`, `-c` or an
encoded-command switch are rejected. Program names and arguments may not contain
control characters or newlines, so a raw log header cannot be forged. Direct
execution of a script path (`{"program": "./tools/check.sh"}` or `bash
script.sh`) is still a normal structured command.

Commands come only from trusted defaults, the project/user configuration or an
explicit `ocg verify` invocation. There is no command discovery: a manifest
existing on disk is never a reason to run anything. Every stage starts empty.

### Stages and configuration

Stages are `fast`, `normal` and `full`; `defaultStage` selects the default.

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
      "fast": { "description": "quick", "commands": ["cargo fmt --check"] },
      "normal": { "commands": ["cargo check", "cargo test"] },
      "full": { "commands": [] }
    }
  }
}
```

- `enabled: false` disables automation; `ocg verify` reports `not_run` and runs
  nothing.
- `stopOnFailure` (default `true`) stops the stage at the first failing
  command.
- `maxRawLogBytes` bounds each captured stdout/stderr stream (default 2 MB, max
  64 MiB). `maxLogStorageBytes` bounds the whole raw-log directory (default
  50 MiB).
- `includeTestProposal` attaches the advisory targeted-test proposal to the
  report. It is never executed.
- When `context.enabled` is false, `ocg verify` still runs configured commands
  but skips targeted-test selection entirely, creates no context index or cache,
  and states the skip explicitly in the report notes.

`ocg verify [fast|normal|full]` returns exit code `1` when a configured command
fails and `0` otherwise. A command that cannot even spawn becomes a failed
result with an explicit unknown exit status, not a crash.

### Capture semantics

Capture drains stdout and stderr concurrently to EOF while retaining only a
bounded prefix per stream, so **the command's own exit status is always
authoritative** and ordinary verbosity is never turned into a false failure.
Truncation is reported explicitly (`raw_truncated`, `output.truncated` and a
note saying only the retained prefix was distilled and the command completed).
Memory stays bounded; a genuinely non-terminating command is a documented
limitation because there is no timeout yet.

## Log distillation

`src/verification/distill.rs` is a deterministic distiller for compiler, test,
build, lint and generic output. It:

- strips ANSI sequences and `\r` progress overwrites;
- removes progress-only lines (percentages, braille spinners, download verbs,
  file-lock waits);
- collapses exact duplicate lines and repeated adjacent blocks;
- groups identical error lines, recording a count;
- extracts errors and warnings, `path:line:column` source locations (Rust,
  GCC/Clang, TypeScript, Python, JS stacks) and useful stack context;
- extracts failed tests for Cargo, Go, pytest and jest/vitest shapes;
- parses `passed`/`failed`/`ignored` counts only from known summary shapes.

It **never fabricates a count or a conclusion**. When a count cannot be read
reliably, `counts` is `None` and a note explains why. Conflicting summaries from
different formats are not trusted.

## Targeted test selection

`src/verification/select.rs` produces a conservative proposal:

- adjacent/naming conventions for Rust, TypeScript/JavaScript and Python;
- a test whose indexed symbols have the same name as a changed symbol (an
  **index name match**, not proof of a textual reference);
- a changed test file proposed directly.

Every candidate carries explicit `reasons` and a `confidence` label. The
proposal always sets `complete = false`: **an unselected test may still fail**.
Unsupported languages are reported, not guessed at. With no candidates, the
proposal names the configured fallback stage to run instead. The proposal is
integrated into the context plan and the verification report but is never
auto-run.

## Capabilities and the Tool Context Firewall

`src/capabilities.rs` defines typed capability groups: `filesystem`, `git`,
`github`, `web`, `documentation`, `browser`, `database`, `cloud` and configured
custom names. The planner is deterministic and conservative:

| Task evidence | Exposed |
| --- | --- |
| git keywords only | `filesystem`, `git` |
| docs keywords | `filesystem`, `documentation` (+ `web` with web keywords) |
| database keywords | `filesystem`, `database` |
| generic coding | `filesystem` (+ `git` only with changed-path evidence) |
| unknown | `filesystem` only |

`cloud`, `browser` and `database` are never exposed for an unknown task.

When `capabilities.enabled` is `false`, the planner is disabled end-to-end: the
resulting `CapabilityPlan` has `enabled = false`, no allowed capability, every
built-in and custom capability in `denied`, and an explicit note. `ocg tools`
and the context plan report the disabled state instead of planning or exposing
anything.

**This is context/config planning and diagnostics, not a security sandbox.**
`ocg tools <task...>` and the context plan describe the intended boundary; the
mode does not activate, mutate or enforce runtime tool schemas. Dynamic runtime
tool-schema activation is not currently claimed.

## Checkpoints

`src/orchestration/checkpoint.rs` stores versioned JSON under
`.opencode-gear/checkpoints/`:

- phases `ExploreToBuild`, `BuildToVerify`, `VerifyToDebug`, `Decision`, in
  canonical order;
- a `TaskCapsule`, Git state and a Git fingerprint, optional verification state,
  provenance, decisions and `created_at`;
- a safe, deterministic id (`cp-<phase>-<hash>`, only `[a-z0-9-]`).

`save` writes atomically and ensures `.opencode-gear/` is ignored. `load`
enforces the schema and revalidates the recorded sources and Git identity: a
stale checkpoint is marked stale with reasons and is never silently reused. A
corrupt checkpoint is counted and ignored by `list`, and it never blocks normal
`ocg`.

## Stable context ordering

The context plan has an exact, stable conceptual section order:

1. gear instructions
2. project policy
3. repository map
4. capability/tool descriptions
5. task capsule
6. relevant symbols/source
7. current git diff
8. verification state

The order is part of the plan contract and is covered by deterministic tests.
The plan also embeds the capability plan and targeted-test proposal without
destabilizing cache provenance.

## Raw log privacy and cleanup

- Raw logs live under `<project>/.opencode-gear/logs/` and are never committed;
  creating state adds `.opencode-gear/` to the project `.gitignore` once.
- Filenames include the creation second, the process id, a monotonic
  process-local sequence and a content hash, so two runs in the same second
  cannot silently overwrite each other. There is no randomness.
- Each stream is bounded by `verification.maxRawLogBytes` and the directory is
  pruned to `verification.maxLogStorageBytes`; truncated content is marked, not
  silently dropped. Pruning never deletes the log the current report references,
  even if that one log alone exceeds the cap.
- Bytes are preserved lossily, so invalid UTF-8 never panics a reader.
- `ocg cache clean` removes the context cache only. It never deletes raw logs,
  checkpoints, the index or the managed runtime.

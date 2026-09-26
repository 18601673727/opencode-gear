# OpenCode Gear Repository Guide

## Repository Identity

OpenCode Gear (OCG) is a project-agnostic Rust orchestration layer and user
interface for OpenCode. This repository is the canonical OCG source tree.
Tasks must begin by verifying that the current directory is this repository,
that the intended worktree is active, and that the requested branch and
baseline match the task contract.

Verify the boundary before changing files:

```bash
git rev-parse --show-toplevel
git branch --show-current
git worktree list --porcelain
git status --short --branch
```

The writable project root is the path returned by `git rev-parse
--show-toplevel`. Do not silently work in a sibling checkout, another worktree,
or an unrelated project. OCG intermediate state belongs under the project
`.opencode-gear/` directory and is ignored by Git.

Top-level layout:

| Path | Purpose |
| --- | --- |
| `src/` | Rust CLI, runtime adapters, orchestration, persistence, providers, and verification code |
| `tests/` | Rust integration tests and fixtures |
| `config/` | Embedded OCG configuration and prompts |
| `docs/` | Architecture, operations, control, migration, and verification documentation |
| `frontend/` | Canonical Next.js OCG UI project |
| `scripts/` | Release and packaging tooling |
| `examples/` | Example project and user configuration |
| `.opencode-gear/` | Ignored local state, reports, checkpoints, and caches |

The Rust package and executable are defined by the root `Cargo.toml`. The
frontend package is defined by `frontend/package.json` and uses pnpm.

## Build, Test, and Verification

Run backend commands from the repository root:

```bash
cargo fmt --all -- --check
cargo check
cargo build
cargo test --all-targets
cargo test --test substrate_tests
cargo test --test orchestration_tests
git diff --check
```

Run frontend commands from `frontend/`:

```bash
pnpm install --frozen-lockfile
pnpm exec tsc --noEmit
pnpm lint
pnpm test
pnpm build
```

`pnpm test` runs the repository's domain, runtime reconciliation, mission,
execution, layout, and related frontend tests through the script in
`frontend/package.json`. Do not substitute npm or regenerate the lockfile
without an explicit reason. `frontend/AGENTS.md` contains scoped frontend
rules.

The strongest practical repository check is all backend commands above plus
all frontend commands above, followed by:

```bash
git status --short --branch
git diff --check
git grep -n -I -E '^(<<<<<<<|=======|>>>>>>>)' -- ':!frontend/pnpm-lock.yaml'
find . -path './.git' -prune -o -path './target' -prune -o -path './frontend/node_modules' -prune -o -path './frontend/.next' -prune -o -name '.git' -print
```

Generated output, dependency directories, local environment files, and
runtime state must not be committed. Check secrets and configuration changes
before committing; do not add `.env*`, credentials, tokens, or local
`.opencode-gear/` state.

## Frozen OCG Execution Architecture

These are the canonical OCG execution decisions. Do not reopen or redesign
them casually:

- The Mission is the root WorkNode.
- Work recursively forms a WorkNode ownership tree.
- Dependencies are a separate DAG and may connect nodes across the ownership tree.
- Serial and parallel behavior emerges from readiness and dependencies.
- A Run is one execution attempt for one WorkNode.
- A Run has a frozen executor contract after dispatch.
- Lead, Worker, and Sub-agent share WorkNode -> Run; their difference is role and authority, not separate execution architecture.
- A Worker may create child WorkNodes for its subtree.
- Run Replacement is the unified failure, preemption, and provider-loss mechanism.
- Root Run replacement is Lead failover; child Run replacement is Worker replacement.
- Fenced or replaced Runs must not regain authority through late results.

The canonical substrate is in `src/orchestration/substrate.rs`, with controller
and bridge integration in `src/orchestration/controller.rs` and
`src/orchestration/bridge.rs`. The current production lifecycle still has a
legacy authority path; `docs/architecture/live-worknode-cutover-draft.md`
records that boundary and must be read before extending the migration.

## Engineering Constraints

- Tart development VMs should not assume Docker is available.
- Docker requires an explicit task-specific exception.
- Avoid large clones, dependency trees, builds, or research data on tmpfs `/tmp`.
- Prefer persistent, disk-backed scratch space such as `/tmp/opencode-gear` when appropriate.
- Prefer the project `.opencode-gear/` directory for OCG intermediate artifacts.
- Reuse canonical abstractions instead of creating parallel representations.
- Enforce deterministic invariants below the inference boundary wherever practical.
- Do not rely on prompts alone for correctness, identity, authority, or workspace boundaries.
- Preserve valuable incomplete work explicitly and describe unfinished cutovers honestly.

## Frontend Rules

`frontend/AGENTS.md` is the scoped guide for the Next.js UI. The root guide is
the canonical repository entry point. Preserve the existing RuntimeStore,
snapshot/event reconciliation, generation and sequence handling, deduplication,
stale-event rejection, project isolation, command correlation, logs, ledger,
attention, runtime projections, Mission Control, and Execution Graph behavior.
Do not replace the existing UI with a toy scaffold or move frontend source
outside `frontend/` without an explicit architecture decision.

Use the frontend's existing components, domain modules, and Lucide/icon
conventions. Keep backend integration assumptions explicit in code and docs.

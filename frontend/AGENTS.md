<!-- BEGIN:nextjs-agent-rules -->

# This is NOT the Next.js you know

This version has breaking changes — APIs, conventions, and file structure may all differ from your training data. Read the relevant guide in `node_modules/next/dist/docs/` (resolved from this file's directory; in monorepos the `next` package may not be visible from the repo root) before writing any code. Heed deprecation notices.

This block is written and re-added by `next dev` — verify at `node_modules/next/dist/server/lib/generate-agent-files.js`. Removing it from a diff only re-creates the uncommitted change; committing it with your work keeps the tree clean.

<!-- END:nextjs-agent-rules -->

# OCG Frontend Scope

This directory is the canonical OCG Next.js frontend. Work from the parent
repository root when a task spans frontend and Rust backend code; verify the
root with `git rev-parse --show-toplevel` before editing.

## Commands

Use pnpm with the committed lockfile:

```bash
pnpm install --frozen-lockfile
pnpm exec tsc --noEmit
pnpm lint
pnpm test
pnpm build
```

The scripts and test selection are authoritative in `package.json`.
`pnpm-workspace.yaml` is intentionally minimal; do not introduce a workspace
rewrite for a local task.

## Existing Runtime Contract

Preserve the existing runtime implementation and its tests. In particular,
`components/ocg/runtime/` owns snapshot/event reconciliation, generation and
sequence ordering, deduplication, stale-event rejection, project isolation,
transport, and command correlation. Mission, execution, logs, ledger,
attention, project, layout, sidebar, and topbar components consume those
projections. Extend those canonical modules instead of creating a second
runtime store or a parallel Mission representation.

Keep local `.env*`, `.next/`, `node_modules/`, build output, TypeScript build
metadata, and `.opencode-gear/` out of commits. Do not copy a nested `.git`
directory into this repository.

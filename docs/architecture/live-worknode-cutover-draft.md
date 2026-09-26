# Live WorkNode cutover: incomplete draft

Baseline: `6764eb42ee5203bb0ad5ac76e7aeb975218979b9`.

This draft is not a completed live cutover. The `work.*` bridge API uses SQLite
only, but the generated plugin still calls the existing JSON-authoritative
Controller workflow. No automatic mirroring or partial admission cutover is
enabled. The new API must not be described as compatibility projection of the
existing live lifecycle.

## Traced live path

| Live operation | Current authority | Canonical treatment still required |
| --- | --- | --- |
| V2 `session.prompt` -> `admit_user_task` / `admit_or_resume` | replay Mission and SessionState | Cut over admission, root WorkNode and resolved Lead Run atomically |
| V1 `chat.message` -> `prepare_lead_context` | same legacy Mission | Defer V1 path |
| `tool.execute.before` -> `prepare_handoff` | legacy Mission phase, Attempts and capsule | Create child with ownership, spawn Run and explicit dependencies; capsule may be projection |
| Work selection / retries | legacy phase, destination, Attempts | Cut over to dependency readiness and active Run |
| Runtime execution binding | legacy Mission session_id | Canonical Run binding; keep RuntimeExecutionId distinct |
| `tool.execute.after` -> `consume_explore_result` / `after_build` | legacy findings, verification, phase and retry counters | Complete the correlated canonical Run; project context afterward |
| fail/cancel -> `terminal_mission` | legacy Mission terminal state | Close canonical Run and WorkNode |
| provider failover / rollover / reconciliation | legacy Mission binding and receipts | Shared Run Replacement with existing routing policy |
| restart | replay/state.json and legacy Mission reads | SQLite -> validated TiVec -> readiness -> projections |
| event emission | replay DomainEvent / Mission history | Canonical domain_events transaction; diagnostics remain separate |

`persist`, `ensure_mission`, `admit_or_resume`, and terminal methods are live
legacy decision points. Legacy lifecycle state is not merely a projection yet.
P0-J/P0-K paths remain unchanged.

## Draft API

`ocg __bridge work.mission.create` requires an invocation-resolved Lead and
verified root session. `work.child.create`, `work.ready`, `work.dispatch`,
`work.finish`, `work.replace`, `work.result.late`, and `work.inspect` carry
explicit Mission/WorkNode/Run witnesses. Their Controller methods read/write
only the substrate. Root creation is atomic with the initial bound Run;
dispatch requires parent authority; root and child replacement share a single
transaction primitive. Recovery reconstructs SQLite arenas without replay.

The draft adds an initialization marker to detect database disappearance,
but no JSON fallback. Terminal histories and immutable contracts are retained.

## Blocking contract gap

The generated subagent-before and subagent-after payloads have session, role,
prompt, and result, but no stable canonical dispatch witness. The before hook
also lacks the child runtime session identity. Correlating by role, prompt, or
the next ready child is ambiguous for parallel children and unsafe after Run
replacement. Adding SQLite admission alone while leaving legacy handoff and
completion decisions active creates split authority.

A complete cutover needs verified runtime delivery correlation from dispatch
to child binding and completion, persisted for restart, with stale deliveries
mapped to their original Run. That caller integration is still required. The
draft is intentionally uncommitted and does not meet the requested acceptance
criterion.

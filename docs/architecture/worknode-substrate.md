# WorkNode / Run substrate (first slice)

This is an opt-in execution control repository, not yet connected to the legacy
controller. `substrate.sqlite3` under `.opencode-gear/orchestration/` is the
only authority for its missions, work nodes, runs, dependencies and events.
Legacy Mission writes remain authoritative in `replay/state.json`; their
`missions/<id>.json` files are compatibility projections. Do not populate both
stores for the same live workflow without an explicit migration and cutover.

## Current to canonical mapping

| Current concept | Canonical concept | Action |
| --- | --- | --- |
| `mission::Mission` (stable task id, generation, status, budget) | Mission, root WorkNode | Reuse identity; adapt on migration, do not duplicate its mutable fields |
| `state::SessionState` / `context::TaskCapsule` | execution view / WorkNode payload | Adapt; session is disposable, capsule is handoff data |
| `state::Attempts`, `reconcile::ReconcileRun`, `dispatch::DispatchRecord` | Run-adjacent counters, recovery, chargeable network attempt | Keep distinct; adapt on migration, not Run identity |
| `runtime::lifecycle::RuntimeExecution{Id}` | external executor binding inside a RunContract | Adapt; an opaque runtime object is not a RunId |
| `mission::MissionEvent`, `replay::DomainEvent`, telemetry `Event` | domain Event / diagnostic metric | Keep existing event streams for legacy workflows; substrate events belong to SQLite |
| legacy phase and retry control | dependency readiness and Run replacement | Deprecate later after scheduler migration |

Other `Execution` uses include runtime lineage, recovery keys and policy's
current execution ID; these continue to refer to runtime objects. Legacy
rollover/reconcile and lead authority checks are unchanged in this slice.

## Invariants and storage

`MissionId` is stable; `WorkNodeId` and `RunId` are distinct mission-local
typed indexes. IDs start at zero, append densely, and are never reused in the
ordinary lifecycle. Root WorkNode is index zero. Ownership uses one parent
column; dependency edges are separate and acyclic. Child provenance is the
spawning Run, but ownership belongs to the Mission. Work readiness is derived
from persisted state, `not_before`, and completed dependencies; memory queues
may only cache that result. Run contracts are private and readable but not
modifiable via lifecycle operations.

`SubstrateRepository` owns a single SQLite connection with WAL, foreign keys,
and transaction-scoped writes. Tables are `missions`, `work_nodes`, `runs`,
`dependencies`, `domain_events`, each keyed by mission and a dense local ID or
sequence. Reconstruction reads ordered rows and checks contiguity and
authority before returning TiVec arenas. No TiVec binary snapshot is stored.
Replacement atomically fences the old Run, appends a new immutable contract,
advances the WorkNode generation and active Run, and appends an event. A stale
Run cannot perform the repository's authoritative child or terminal mutations.
Reopen and reload after writes to reconcile an in-memory projection. This is
not yet a tool gateway, distributed lease protocol, or scheduler integration.

Large immutable artifacts belong in a future filesystem BLAKE3-addressed CAS.
Diagnostic tracing and rotating logs are not database truth. L1 memory and L2
disk caches, search/graph projections, and statistics rollups are rebuildable;
raw accounting facts remain durable. Retention/compaction is deferred.

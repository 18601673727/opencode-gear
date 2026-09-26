# OCG 0.4 Execution Mission

This document is the execution contract for the first long-horizon OCG 0.4
Mission. It is written against the pushed preparation baseline below. The
executor must verify the repository identity and exact `HEAD` before changing
anything.

OCG 0.4 is the self-hosting threshold: the canonical WorkNode/Run execution
spine must own enough of the real production lifecycle to execute one
meaningful engineering Mission end to end, with durable evidence and no
ambiguous result correlation.

## 1. Exact Baseline

| Item | Value |
| --- | --- |
| Canonical repository | `https://github.com/18601673727/opencode-gear.git` |
| Canonical local root | repository root containing `Cargo.toml`, `src/`, `tests/`, and `frontend/` |
| Canonical branch | `main` |
| Pushed baseline SHA | `bf8a79e0b355a0ed21c5c0dd08a1f0720eef713a` |
| Current OCG version | `0.3.7` in root `Cargo.toml` |
| Backend/core path | `src/`, `tests/`, `config/`, `docs/` |
| Frontend path | `frontend/` |

Before starting, the executor must run:

```bash
git rev-parse --show-toplevel
git branch --show-current
git rev-parse HEAD
git worktree list --porcelain
git status --short --branch
```

The branch must be `main`, the worktree must be the canonical repository, and
`HEAD` must equal the baseline SHA above unless the operator has explicitly
published a newer mission baseline. Do not silently execute in a sibling
checkout or a different worktree.

Backend verification commands:

```bash
cargo fmt --all -- --check
cargo check
cargo build
cargo test --all-targets
cargo test --test substrate_tests --test orchestration_tests
git diff --check
```

Frontend verification commands, from `frontend/`:

```bash
pnpm install --frozen-lockfile
pnpm exec tsc --noEmit
pnpm lint
pnpm test
pnpm build
```

The frontend `lint` command currently emits one warning for the existing
unused `onOpenChat` prop in `components/ocg/topbar/ocg-topbar.tsx`; it has no
lint errors. The canonical frozen install permits `esbuild` in
`frontend/pnpm-workspace.yaml` and denies the other packages currently listed
there.

## 2. Current Implementation Inventory

Paths below are authoritative at the baseline. Preserve their behavior and
history while extending the canonical execution spine.

### KEEP

- `src/orchestration/substrate.rs`: tested SQLite WorkNode/Run substrate. It
  defines `MissionId`, `WorkNodeId`, `RunId`, `WorkState`, `RunState`,
  `RunContract`, `WorkNode`, `Run`, `Dependency`, `MissionState`, durable
  domain events, readiness, dense recovery, authority checks, and fenced
  result recording.
- `tests/substrate_tests.rs`: tests ownership-tree creation, dependency
  readiness, concurrent independent children, replacement fencing, rollback,
  terminal history, and restart reconstruction.
- `src/runtime/lifecycle.rs`: runtime-neutral `RuntimeExecutionId`, lineage,
  recovery, capability, and adapter vocabulary. Keep runtime identity distinct
  from Mission, WorkNode, and Run identity.
- `src/project.rs`: nearest `.opencode-gear.yaml` project-boundary resolution.
  This is the starting point for machine-enforced workspace identity.
- `src/verification/`: structured trusted verification, bounded logs, result
  distillation, and test selection. Verification evidence must remain separate
  from model text and inference.
- `src/orchestration/replay.rs` and `src/orchestration/mission.rs`: preserve
  existing durable legacy state and migration compatibility until the cutover
  has proved that it no longer owns production authority.
- `frontend/components/ocg/runtime/`: existing `RuntimeStore`, reconciler,
  snapshot/event envelopes, cursor generation and sequence handling,
  deduplication, stale-event rejection, project isolation, transport boundary,
  and command correlation.
- `frontend/components/ocg/mission/`,
  `frontend/components/ocg/mission-control/`,
  `frontend/components/ocg/execution/`,
  `frontend/components/ocg/logs/`,
  `frontend/components/ocg/resource-ledger/`, and
  `frontend/components/ocg/attention/`: existing Mission and runtime
  projections. Preserve their domain contracts and tests.
- `AGENTS.md` and `frontend/AGENTS.md`: repository and scoped operating rules.

### EXTEND

- `src/orchestration/substrate.rs`: extend the durable Run record and event
  contract with the stable dispatch witness required below. Keep `RunId` and
  Run generation authoritative; do not add a parallel task identity.
- `src/orchestration/controller.rs`: extend canonical Controller operations
  around `create_work_mission`, `create_child_work`, `dispatch_work`,
  `finish_work`, `replace_work_run`, and `record_late_work_result`. These are
  the intended seam for production WorkNode/Run transitions.
- `src/orchestration/bridge.rs`: make canonical bridge operations carry and
  validate the exact dispatch witness through dispatch, runtime binding,
  result, completion, replacement, and late-result paths. Keep malformed or
  stale deliveries fail-closed.
- `src/orchestration/plugin.rs`: extend the generated V2 `subagent`
  `execute.before` and `execute.after` hooks so the exact witness returned by
  dispatch is delivered back with the same invocation. Prove the host hook
  behavior in fixtures before enabling it for production.
- `src/runtime/lifecycle.rs` and the concrete compatibility adapters: add only
  the runtime-neutral operation needed to bind a dispatched Run to the exact
  runtime execution and recover that binding after restart.
- `src/orchestration/control.rs` and the existing control/replay projections:
  expose enough canonical state and evidence for inspection and the frontend
  without making a presentation projection authoritative.
- `frontend/components/ocg/runtime/` and execution projections: consume a
  stable backend event/snapshot contract only after the backend contract is
  durable and versioned. Keep reconciliation as the frontend authority for its
  presentation state.

### MIGRATE

- `src/orchestration/bridge.rs` legacy event routing:
  `session.prompt` currently calls `Controller::admit_user_task`;
  `tool.execute.before` calls `Controller::prepare_handoff`; Explore
  completion calls `Controller::consume_explore_result`; Build completion calls
  `Controller::after_build`.
- `src/orchestration/controller.rs` legacy lifecycle:
  `ensure_mission`, `admit_or_resume`, `persist`, `admit_user_task`,
  `prepare_handoff`, `consume_explore_result`, `after_build`, and terminal
  methods currently mutate JSON Mission/SessionState/replay authority.
- `src/orchestration/plugin.rs` generated hook payloads currently contain
  session, role/agent, prompt/args, and result but no stable canonical Run
  witness. The current V2 after hook handles only a completed foreground
  subagent and intentionally has no asynchronous background completion path.
- Legacy `Mission`, `SessionState`, Attempts, phase, destination, checkpoints,
  and rollover state must become projections or compatibility inputs only after
  canonical Controller transitions own the same decision.

### REPLACE

- Replace result correlation based on role, prompt text, runtime session alone,
  or the next ready child with a durable dispatch witness bound to one Run.
- Replace production child selection based on legacy phase and retry counters
  with `MissionState::ready(now)` plus explicit dependency and authority
  checks.
- Replace separate root failover and child retry semantics with the existing
  shared Run Replacement primitive (`replace_work_run` /
  `SubstrateRepository::replace_bound_run`) once it is live on the production
  path.
- Replace terminal completion that trusts model output or an uncorrelated
  hook with canonical Run completion plus independent verification evidence.

### DELETE AFTER CUTOVER

Only after a compatibility period, migration evidence, and restart tests prove
that no production path reads them as authority:

- legacy lifecycle writes that duplicate canonical WorkNode/Run state;
- fallback correlation by prompt, role, session, or ready-queue position;
- duplicate retry/failover state that can disagree with Run generation;
- compatibility-only JSON control fields that are no longer read by the
  canonical Controller.

Do not delete legacy state, replay records, or migration readers merely because
the canonical tests pass.

### DO NOT TOUCH

- Do not redesign the Mission/root WorkNode model, ownership tree, dependency
  DAG, Run entity, frozen Run contract, authority roles, fencing, replacement,
  durable execution state, workspace boundary, or evidence-based completion.
- Do not replace the existing frontend with a scaffold, move it out of
  `frontend/`, or create another RuntimeStore/Mission model.
- Do not begin a broad PWA redesign, UI polish pass, or unrelated provider,
  cost, naming, or directory refactor to satisfy this Mission.

## 3. Explicit Current Status

| Capability | Status at baseline | Evidence and location |
| --- | --- | --- |
| Mission -> root WorkNode | Implemented but not live | `SubstrateRepository::create_live_mission` creates root node and root Run atomically; legacy admission still owns live creation. |
| Recursive WorkNode ownership tree | Implemented but not live | `create_child_work` stores `parent_node_id` and `spawned_by_run_id`; `MissionState::children` and load validation recover it. |
| Separate dependency DAG | Implemented but not live | `dependencies`, `add_dependency`, dependency-aware `create_child_work`, and `MissionState::ready`. |
| Deterministic readiness | Implemented but not live | `MissionState::ready` requires `Ready`, no active Run, `not_before`, and completed prerequisites. |
| Explicit Run identity/entity | Implemented but not live | Dense `RunId`, generation, immutable `RunContract`, state, result, and runtime binding in `substrate.rs`. |
| Frozen Run executor contract | Implemented in substrate, not live | `RunContract` is persisted and not exposed as a mutable lifecycle update; production handoff still uses legacy capsules. |
| Stable dispatch/completion correlation | Missing in production; partial substrate support | `Run.runtime_execution_id` exists, but generated hook payloads do not carry a durable Run witness from before to after. |
| Machine-enforced workspace/repository boundary | Partially implemented | `src/project.rs` resolves the nearest marker and tests the boundary; bridge/runtime identity and agent task execution still need explicit canonical witness checks. |
| Recursive child work through canonical machinery | Implemented in direct bridge API, not live | `work.child.create` requires parent Run authority; normal `tool.execute.before` still uses `prepare_handoff`. |
| Run fencing | Implemented in substrate, not live | `replace_run_with_binding` fences old generations and `record_fenced_result` preserves late evidence without authority. |
| Unified Run Replacement | Implemented in substrate, not live | Root and child replacement share `replace_bound_run`; legacy rollover/retry paths remain separate. |
| Durable canonical WorkNode/Run state | Implemented as opt-in SQLite | `.opencode-gear/orchestration/substrate.sqlite3` is validated and recoverable; `mission.rs`/`replay.rs` JSON remains legacy authority. |
| Restart/recovery/resume | Partial | SQLite reload validates dense IDs, ownership, generations, authority, dependencies, and events; production restart still follows legacy replay/session recovery. |
| Verification separate from model completion | Implemented in legacy lifecycle; missing in canonical lifecycle | `Controller::after_build` runs `src/verification/` and records `VerificationReport`; canonical `finish_work` accepts result text but is not yet gated by production verification. |
| Evidence-based WorkNode/Mission completion | Partial and conflicting | Legacy Mission completion records verification evidence; canonical WorkNode completion is available but not connected to that evidence contract. |
| Production authority cutover | Missing | `docs/architecture/live-worknode-cutover-draft.md` explicitly records that legacy Mission/SessionState still owns live decisions. |
| Frontend runtime reconciliation | Implemented for frontend mock/runtime contract | `frontend/components/ocg/runtime/` tests generation, sequence, deduplication, stale events, project scope, command acknowledgements, and recovery. Backend transport integration is not the 0.4 authority yet. |
| One meaningful self-dogfood Mission | Missing | No baseline evidence shows a real engineering Mission completed through canonical production WorkNode/Run execution. |

## 4. Concrete OCG 0.4 Delta

The executor must implement and prove all of the following, in this order:

1. **Root identity and boundary.** A real admitted Mission creates exactly one
   canonical root WorkNode and initial Lead Run atomically, under the resolved
   repository boundary. The root Run carries the resolved Lead contract and
   runtime binding.
2. **Recursive ownership and dependencies.** Lead/Worker code creates child
   WorkNodes only through canonical Controller machinery. Ownership is a tree;
   dependencies are a separate acyclic DAG; readiness is deterministic and
   durable.
3. **Run entity and immutable contract.** Every execution attempt has one
   durable Run, one generation, one frozen executor/model/role contract, and
   one explicit runtime binding. A replacement creates a new Run rather than
   mutating the old contract.
4. **Stable dispatch witness.** The exact Run identity is carried from
   WorkNode -> Run -> dispatch -> runtime binding -> execution -> result ->
   canonical Controller. It must survive concurrent children, restart, and
   replacement.
5. **Authority and fencing.** Every mutating completion, child creation, and
   replacement validates Mission, WorkNode, Run, generation, and current
   authority. Fenced or late deliveries may be retained as evidence but may
   not mutate authoritative WorkNode/Mission state.
6. **Production cutover.** The live `session.prompt` admission, task before,
   task after, retry/failover, terminal, and recovery path converges on the
   canonical Controller. Legacy state can remain as a projection or migration
   input, but cannot make a conflicting execution decision.
7. **Durable recovery.** Restart loads canonical state, verifies identity and
   invariants, reconstructs readiness and active Runs, and resumes or replaces
   execution without guessing from session or prompt text.
8. **Verification and evidence.** Verification is an explicit transition and
   evidence record. Model output is not completion evidence by itself.
9. **Frontend contract.** The existing RuntimeStore receives versioned,
   project-scoped canonical snapshots/events with command correlation. UI
   rendering remains a projection and must not become execution authority.
10. **Self-hosting dogfood.** One real engineering Mission completes through
    the production path described in section 10.

## 5. Stable Dispatch Witness / Run Correlation

This is the most important unresolved contract. The current code has a
durable `RunId` and an optional `Run.runtime_execution_id`, but the generated
plugin calls the bridge with only `session_id`, role/agent, prompt/args, and
result. That is not enough to identify a concurrent child or reject a late
result after replacement.

The implementation must establish one durable witness for each dispatched Run.
The witness must bind at least:

```text
mission_id
work_node_id
run_id
run_generation
runtime_execution_id
dispatch_id
```

`run_id` plus generation must be checked against the canonical SQLite Run;
`dispatch_id` must identify the individual invocation and be persisted before
the external execution starts. Do not use a prompt hash as the authority, and
do not treat a runtime session id as a Run id.

Required flow:

1. Canonical Controller creates or selects the ready WorkNode and commits the
   active Run, frozen contract, runtime binding, and dispatch witness before
   invoking the runtime.
2. The dispatch adapter attaches the witness to the exact subagent execution
   using a runtime-supported structured field or an OCG-owned envelope that is
   returned unchanged by the host. It must not depend on model-visible prose.
3. The `execute.before` hook sends the witness to `ocg __bridge` and the
   bridge validates that it names the active Run before allowing dispatch.
4. The `execute.after` hook sends the same witness and result. The bridge
   answers: which Run produced this result, is it still authoritative, and may
   this result change WorkNode state?
5. A replacement fences the old Run before starting the new Run. A late old
   result is recorded against the old Run as late evidence and cannot complete,
   retry, create children for, or otherwise mutate the replacement's node.
6. Restart recovers the witness and pending dispatch state from durable state;
   no in-memory map or next-ready scan is required for correctness.

Acceptance tests must include two concurrent children with identical roles and
similar prompts, replacement before the old result arrives, a late result from
the old runtime binding, process restart between dispatch and completion, and
duplicate completion delivery. Tests must prove the exact Run receiving each
result and the absence of authoritative mutation from stale deliveries.

## 6. Production Authority Cutover

The currently live path is documented in
`docs/architecture/live-worknode-cutover-draft.md` and in the controller module
header:

```text
session.prompt -> Controller::admit_user_task -> legacy Mission/SessionState
tool before   -> Controller::prepare_handoff
Explore after -> Controller::consume_explore_result
Build after   -> Controller::after_build
terminal      -> legacy Mission terminal transition
restart       -> JSON replay/state and legacy Mission reads
```

The generated V2 path is in `src/orchestration/plugin.rs`: its `prompt`,
`session.context`, `tool.execute.before`, and `tool.execute.after` hooks call
the bridge. `BridgeContext::tool_before`, `after_explore`, and `after_build` in
`src/orchestration/bridge.rs` currently call the legacy controller methods.

The cutover seam is the bridge dispatch boundary. Preserve ordinary chat and
context behavior, but route an admitted canonical Mission and all delegated
execution through canonical Controller operations. The cutover must not create
two sources of truth by mirroring every legacy mutation into SQLite. Choose a
single authority per transition, publish a projection for compatibility, and
make the choice explicit in tests and documentation.

Required cutover evidence:

- a trace from one real prompt through root creation, child creation, dispatch,
  runtime binding, result, verification, completion, and terminal Mission;
- no legacy `SessionState` phase or `next ready` lookup decides which Run a
  result belongs to;
- replacement and recovery use the canonical Run generation;
- legacy JSON/replay remains readable during migration and cannot overwrite a
  newer canonical decision;
- the bridge fails closed when the canonical database is missing, corrupted,
  or outside the resolved repository boundary.

## 7. Concrete WorkNode Hierarchy

The executor should implement these WorkNodes in dependency order. Each node
has a bounded objective and acceptance evidence. Do not maximize parallelism;
the witness and authority path are the critical path.

### W0: Establish the canonical execution contract

- Parent: none; root Mission WorkNode.
- Prerequisites: baseline verification in section 1.
- Objective: write the durable schema/event and API contract for Run witness,
  authority, completion, replacement, verification evidence, and recovery.
- Relevant modules: `src/orchestration/substrate.rs`, `controller.rs`,
  `bridge.rs`, `runtime/lifecycle.rs`.
- Likely files: those modules, `docs/architecture/`, focused Rust tests.
- Invariants: one root; tree ownership; separate acyclic DAG; dense IDs;
  immutable contract; no stale authority.
- Tests: schema round-trip, migration/open behavior, invalid witness rejection.
- Acceptance: a written contract and tests state the exact witness fields and
  which transaction owns every transition.
- Non-goals: no broad schema rewrite or frontend redesign.

### W1: Implement durable dispatch witness and runtime binding

- Parent: W0.
- Dependencies: W0.
- Objective: persist and validate a witness for every canonical dispatch and
  carry it through runtime-neutral lifecycle APIs.
- Relevant code: `Run`, `RunContract`, `SubstrateRepository::dispatch_run`,
  `replace_bound_run`, `RuntimeExecutionId`, runtime adapters.
- Likely files: `substrate.rs`, `runtime/lifecycle.rs`, compatibility adapter,
  `tests/substrate_tests.rs` and new focused tests.
- Invariants: witness is committed before external execution; replacement
  increments generation; late result cannot become active.
- Tests: concurrent same-role children, duplicate delivery, restart,
  replacement, late result, invalid/mismatched witness.
- Acceptance: the test can identify the exact Run for every result without
  consulting prompt, role, session ordering, or ready ordering.
- Non-goals: do not change provider selection policy.

### W2: Cut over canonical bridge and plugin dispatch

- Parent: W1.
- Dependencies: W1.
- Objective: make the actual `tool.execute.before` and `tool.execute.after`
  path create/dispatch/complete canonical Runs with the witness attached.
- Relevant code: `BridgeContext::canonical_work`, `tool_before`,
  `tool_after`, `after_explore`, `after_build`; generated V2 hooks in
  `src/orchestration/plugin.rs`.
- Likely files: `bridge.rs`, `plugin.rs`, `controller.rs`, hook fixtures and
  orchestration integration tests.
- Invariants: no result is applied until witness validation succeeds; every
  child is spawned by an authoritative parent Run; background or missing
  delivery is represented explicitly rather than guessed complete.
- Tests: real generated-plugin fixture with two child calls, same-role
  concurrency, replacement, late result, and duplicate after hook.
- Acceptance: a trace proves before and after carry the same durable witness.
- Non-goals: no new transport ecosystem or provider abstraction.

### W3: Migrate lifecycle decisions and terminal evidence

- Parent: W2.
- Dependencies: W2.
- Objective: route admission, readiness, retry/replacement, verification,
  terminal completion, and recovery through the canonical Controller.
- Relevant code: `admit_user_task`, `admit_or_resume`, `prepare_handoff`,
  `consume_explore_result`, `after_build`, `terminate_mission`,
  `src/verification/`, `src/orchestration/mission.rs`.
- Likely files: `controller.rs`, `bridge.rs`, `mission.rs`, `replay.rs`,
  verification and integration tests, migration documentation.
- Invariants: legacy state cannot win a conflict against a newer canonical
  decision; verification evidence is durable and distinct from model output;
  terminal state freezes the generation.
- Tests: successful chain, failed verification and replacement, restart,
  recovery, terminal idempotency, legacy compatibility read.
- Acceptance: no live execution transition depends on legacy phase,
  Attempts, or uncorrelated session completion.
- Non-goals: do not delete compatibility state during the first cutover.

### W4: Enforce repository boundary and recovery/resume

- Parent: W3.
- Dependencies: W3.
- Objective: machine-enforce repository/worktree identity for every canonical
  bridge/runtime call and resume an interrupted Mission from durable state.
- Relevant code: `src/project.rs`, CLI project resolution, Controller root,
  `SubstrateRepository::open/load`, runtime recovery adapters.
- Likely files: `project.rs`, `cli.rs`, `controller.rs`, bridge and recovery
  tests, `AGENTS.md` only if the actual contract changes.
- Invariants: one task cannot write another repository; missing initialized
  database fails closed; restart never invents a Run or silently resets state.
- Tests: sibling/nested project roots, wrong worktree, missing DB marker,
  restart between each execution boundary, stale process result.
- Acceptance: boundary violations are machine errors and recovery resumes from
  canonical evidence without human prompt reconstruction.
- Non-goals: no container sandbox or Docker dependency.

### W5: Publish canonical runtime projection to the existing frontend

- Parent: W4.
- Dependencies: W4 and the backend event/snapshot contract.
- Objective: connect the existing UI runtime transport to canonical snapshots,
  events, command acknowledgements, logs, ledger, attention, and execution
  projections.
- Relevant code: `frontend/components/ocg/runtime/runtime-envelope.ts`,
  `runtime-store.ts`, `reconciler.ts`, `runtime-snapshot.ts`, execution and
  Mission projections.
- Invariants: generation/sequence and project isolation remain enforced;
  frontend state remains a projection; commands carry stable identities.
- Tests: existing 202 frontend tests plus backend/frontend contract fixtures
  for snapshot, event, acknowledgement, reconnect, stale event, and late Run.
- Acceptance: UI can inspect the same canonical Run/WorkNode identity as the
  backend without deriving authority from presentation state.
- Non-goals: no broad UI polish, PWA redesign, or mock fixture rewrite.

### W6: Execute and preserve dogfood evidence

- Parent: W5.
- Dependencies: W5 and all critical-path acceptance tests.
- Objective: execute the Mission in section 10 through the real production
  OCG path and retain inspectable evidence.
- Evidence: canonical event stream, Run/WorkNode snapshot before and after,
  verification report/log references, replacement or recovery trace where
  exercised, final commit, and frontend projection trace.
- Acceptance: all criteria in section 10 pass against durable state.
- Non-goals: do not claim 0.4 from tests alone.

## 8. Frozen Decisions

Bunny and Pixel must not redesign these decisions during execution:

- Mission is the root WorkNode.
- WorkNode ownership is recursive and forms a tree.
- Dependencies are a separate DAG.
- WorkNode -> Run is the single execution model.
- Run contracts are immutable after dispatch.
- Lead, Worker, and Sub-agent differ by authority and role, not architecture.
- Workers may recursively create children for their subtree.
- Run fencing prevents late or replaced Runs from regaining authority.
- Run Replacement is the unified root failover, child replacement, provider
  loss, failure, and preemption mechanism.
- Execution state is durable and authoritative below the inference boundary.
- Repository/workspace identity is machine-enforced.
- Completion requires verification and evidence, not model assertion.

## 9. Out of Scope

Unless a narrowly scoped change is required to unblock the critical path,
exclude:

- broad PWA redesign or UI polish;
- full World Graph;
- full Context and Attention Plane;
- full Robust Editing roadmap beyond already preserved `src/edit.rs`;
- complete Runtime Capability Layer;
- provider ecosystem expansion;
- complete metric-semantics overhaul;
- broad cost optimization;
- naming, branding, and version-marketing work;
- unrelated refactors, crate renames, package-manager changes, or workspace
  rewrites;
- Docker/container assumptions in Tart development VMs.

## 10. Dogfood Gate

OCG 0.4 must execute this concrete engineering Mission through the real
production execution path after W0-W5:

> **Canonical execution inspector:** extend OCG so a read-only inspection of a
> live Mission exposes the canonical WorkNode tree, dependency DAG, active and
> historical Run identities, frozen executor contracts, verification evidence,
> replacement/fencing events, and restart-safe runtime state to the existing
> frontend Execution Graph and Mission Control surfaces.

This is meaningful because it requires the same backend authority and frontend
projection boundary that OCG must use to operate itself. It is bounded: the
inspector is read-only, uses existing `work.inspect`/control projection seams,
and does not require a full World Graph or a new provider ecosystem.

The dogfood Mission must have at least this WorkNode shape:

```text
root: define and implement the canonical execution inspector
  child A: trace current substrate/controller and define the projection
    child A1: inspect WorkNode/Run/dependency/event invariants
  child B: implement backend canonical inspection and event projection
    depends on A
    child B1: implement witness-aware Run history and late-result display
  child C: connect the existing RuntimeStore and Execution Graph projection
    depends on B
  child D: verify, recover, and document the result
    depends on B and C
```

The Mission must exercise:

- at least five WorkNodes, including recursive depth of two or more;
- at least one real dependency chain and at least two independent children;
- a root Run and child Runs with frozen contracts;
- canonical readiness and dispatch;
- independent verification commands and durable evidence;
- terminal WorkNode and Mission completion;
- frontend snapshot/event projection with command correlation.

Where practical, the same run must also exercise a Worker Run Replacement,
restart/resume between dispatch and completion, and a stale late result from a
fenced Run. When an environment cannot force a provider/runtime failure, the
executor must use the deterministic runtime fixture and record why the
replacement scenario was fixture-backed.

Objective acceptance criteria:

1. `work.inspect` or its versioned control successor shows one root, the full
   ownership tree, dependency edges, every Run generation, contracts, bindings,
   and event sequence from the canonical durable store.
2. Every result and completion is attributed to the exact Run witness. A test
   with same-role concurrent children proves that no result is assigned by
   prompt, role, session, or ready order.
3. A replaced Run is `Fenced`; its late result is retained as non-authoritative
   evidence and cannot create a child, complete the node, or alter the Mission.
4. Stopping and restarting the executor preserves the canonical state and
   allows the Mission to resume without reconstructing intent from chat text.
5. Verification output is stored/referenced separately from model output and
   is required for the terminal success transition.
6. The existing frontend RuntimeStore rejects stale generation/sequence or
   wrong-project events and displays the canonical identities without inventing
   a second authority.
7. The final repository contains the implementation, focused tests, durable
   event/evidence artifacts, and a concise dogfood report that another
   executor can inspect from the canonical repository.

## 11. Final 0.4 Definition

OCG 0.4 is **not** reached because version metadata says `0.4`, code compiles,
unit tests pass, or an agent says "done".

OCG 0.4 is reached only when:

1. the canonical WorkNode/Run execution spine is implemented;
2. stable dispatch/completion correlation and Run fencing are proven;
3. production authority has converged sufficiently away from the legacy
   Mission/SessionState lifecycle;
4. durable restart/recovery and evidence-based verification work on the real
   path; and
5. the meaningful dogfood Mission above completes end to end through that path
   with inspectable evidence.

The milestone is the **SELF-HOSTING THRESHOLD**.

The executor must update this document or linked architecture records when
implementation changes the actual seams, but must not weaken the acceptance
criteria to match an incomplete implementation.

# Architecture decision: managed tool-execution authority

**Status:** boundary recorded; managed edit adoption deferred. **Baseline:**
`9fa16c5b16618fa2e6f2e3b113d32389a5225932`.

## Decision and scope

For future **OCG-managed execution**, OCG must own admission and execution of
side-effecting tools. A runtime adapter must not mutate authoritative external
state behind OCG's back. An ordinary OpenCode-native session is outside this
contract and retains its native tools. Launching a private OpenCode server and
selecting an OCG Lead do **not** transfer ownership of its tool executors to OCG.

The existing Rust Robust Edit Gateway (`src/edit.rs`) is deliberately usable
without OpenCode. It is not on the current live OpenCode edit path. The
invocation-scoped **provider** gateway (`src/provider_gateway.rs`) is also not
a tool executor: it relays model responses, including tool-call name/argument
deltas, to OpenCode; OpenCode assembles, validates, and executes those calls.

## Current production flow

```text
model/provider ── tool-call stream ──> OpenCode session/tool registry
                                          │
                     optional OCG generated plugin tool before/after hooks
                     (V1 task, V2 subagent: hand-off/feedback only)
                                          │
                                          ├─ edit/write/other native tool → OpenCode writer → file
                                          ├─ bash/shell → OpenCode executor → external process
                                          ├─ read → OpenCode reader → tool result
                                          └─ task/subagent → OpenCode child session → tool result

separately, when explicitly registered:
model → OpenCode MCP client → ocg mcp → ControlService → domain mutation/result
```

`src/build.rs` generates agents and permission profiles and enables the plugin
only when orchestration is enabled. `src/orchestration/plugin.rs` materializes
the V1/V2 adapters. Its tool-before and tool-after hooks ignore tools other
than `task`/`subagent`; `src/orchestration/bridge.rs` handles delegation
context/feedback, not edit requests. V1 passes through `output.args`; V2
mutates `event.input.prompt`. The bridge is fail-soft, so it cannot be treated
as a mandatory tool-admission gate. OpenCode owns the native tool result and
the actual file/process operation. An OCG worker still executes *inside*
OpenCode, not inside an OCG tool executor.

The shipped worker permissions (`config/permissions.yaml`) deny `edit` and
`bash` in read-only/review profiles and permit writing in the write profile.
Those are runtime-enforced permissions, not proof that an allowed edit flows
through OCG. Overrides and runtime-specific tools require their own audit.

### Ownership and interception matrix

Legend: **A** OCG-authoritative; **B** observable but runtime-authoritative;
**C** runtime-only; **D** safely interceptable per managed session; **E** only
globally/location interceptable with the current tool-definition surface;
**F** unproven or requires a stronger protocol. "Hooks" below means the
generated plugin is active; it does not imply a successful OCG bridge call.

| Tool/action | Current owner / class | OCG visibility | Can block? | Can rewrite? | Can execute itself? | Session-scoped? | Native-session impact of replacement | Future desired owner |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Native `edit` (exact replacement) | OpenCode / B, E | Generic tool hook can see tool/input; current handler ignores it; provider stream may expose deltas on migrated routes | A throwing hook may reject a seen invocation, **not** substitute a complete writer; early schema failures may not reach it | Hook input may be mutable, but no guaranteed `oldString` repair before runtime validation | No | Hook receives session ID; tool definition transform is location-scoped | Replacing native definition may affect ordinary sessions at that location | OCG managed gateway + transactional edit engine |
| Native `write` / other file-writing or patch tools, where exposed | OpenCode / B, E, F | Generic hook when invoked; exact catalog varies with runtime/config | No demonstrated complete managed-only admission | No demonstrated complete rewrite | No | No proven exclusive managed-session writer | Location-level replacement risks native sessions | OCG executor or excluded from managed tool snapshot |
| `bash`/shell or command-capable tool | OpenCode and spawned process / B, F | Generic hook sees tool invocation, not all effects of the child process | Runtime permission or hook can reject an observed call; not a semantic filesystem gate | Arguments may be hook-visible; shell side effects cannot be normalized into edits | No | Not a proven managed-only execution boundary | Global restrictions would affect native sessions | OCG-admitted execution with explicit effects policy, or not exposed |
| `read`, `glob`, `grep`, `list`, `lsp` | OpenCode / B (read-only by advertised purpose) | Generic hook sees invocation when active; OCG context indexing is a separate reader | Runtime permissions/hooks, not OCG tool admission | Hook input potentially mutable | No | Not an OCG-owned tool | Global replacement may affect native sessions | OCG-managed reader where tool results require OCG ownership |
| V1 `task` / V2 `subagent` | OpenCode / B | OCG bridge receives selected Lead-to-worker arguments and foreground completion feedback | Existing handler does not authorize or execute delegation; bridge failure is swallowed | Yes, appends context to selected worker prompt/result | No; OpenCode creates/executes child | Filtered by agent in the hook, but not an exclusive execution gate | Global substitution would affect other sessions | OCG admits delegation; runtime may host the child under an explicit contract |
| Explicit `ocg mcp` control reads and approval/budget writes | OCG `ControlService` / A for **these calls only** | Full validated MCP request, typed domain result | Yes | Validates at MCP/domain boundary | Yes | Project-scoped child; registration is opt-in | Does not replace native edit/shell; native sessions can separately opt in | Keep OCG domain authority; integrate into managed catalog when available |
| Other runtime/plugin/MCP-provided mutators | Their registered executors / C or F | Unknown unless OCG hook is active and delivered | Not proven | Not proven | Not generally | Not proven | Disabling globally may change native sessions | Explicit managed catalog and fail-closed admission |

No row for native edit currently qualifies as **D**. A per-request V2
`session.context` hook can alter the *model-visible* tool set, but that is not
evidence of exclusive execution control over every callable file mutator,
earlier snapshots, or non-model-originated calls. V2 `ctx.tool.transform` can
replace a definition, but registrations are location-scoped, and each request
captures a stable executable tool snapshot. V1 and V2 expose different plugin
contracts; neither generated adapter currently registers an edit executor.

## Why there is no live edit workaround

The missing primitive is an **exclusive, managed-execution-scoped tool
admission/delegation point** before any native writer runs, with a way to
return a typed result instead of invoking that writer. Hook observation or
post-execution reporting alone cannot establish this guarantee. A location-wide
replacement would also change ordinary native sessions; routing only the
`edit` name leaves `write`, shell commands, other mutators, and runtime/plugin
tools as alternate mutation paths. Assuming an edit hook always receives a
schema-invalid request would additionally be unsafe. Making every OpenCode
mutator use an OCG replacement would be a deeper runtime-specific integration,
not a thin gateway adapter. No global monkey-patch, fail-soft bridge call, or
test-only route was added.

The present OpenCode integration therefore **does not satisfy** the complete
managed-tool contract. This is a statement about this repository's adapter and
verified guarantees, not a claim that OpenCode has no future extension API.

## Target flow and minimum runtime contract

```text
model → OCG-managed tool request (bound to execution/session)
      → OCG Tool Call Gateway (admit, canonicalize or reject)
      → typed OCG tool executor
      → for edits: construct_call() → apply_call() → atomic replacement
      → typed success / construction rejection / execution conflict
      → runtime delivers result to model

OpenCode-native session → native OpenCode tools (outside OCG authority)
```

A future adapter (including an OCG-owned communication/tool path) must provide:

1. Every managed tool request, including name, raw arguments and invocation
   identity, reaches OCG **before** its side effect. Malformed construction
   requests must be deliverable for deterministic repair or structured refusal.
2. OCG can admit or reject the request, replace/delegate the *executor* (not
   merely rewrite the model-visible schema), and return a typed result or
   error to the same model turn. A rejected call cannot fall back to native
   execution.
3. Session/execution correlation identifies managed vs native calls and
   supports cancellation and stale-execution rejection. Cancellation must not
   claim to undo a committed external side effect.
4. The managed catalog is closed over effectful tools: disabled or unknown
   mutators cannot still execute through a hidden native tool, plugin, MCP,
   shell, or earlier captured snapshot. Read-only tools must be classified
   separately from tools that can spawn a process or write files.
5. An edit result preserves operation kind, direct/repaired/rebased outcome,
   retry count, revision correlation and construction-vs-execution conflict.
   One bounded retry policy lives in `apply_call()`; the adapter adds none.

For edits, the adoption point is immediately after the OCG-owned request
boundary: pass the raw edit arguments to `edit::construct_call`, then the
canonical call and project root to `edit::apply_call`, and translate the typed
outcome once into the runtime's result envelope. Missing `oldString` is not
invented. The engine's revision check remains authoritative even while its
cooperative per-file lock is held; an external writer need not use that lock.
This contract does not require WorkNode/Run or any persistence schema.

## Deferred work

- Establish and verify an exclusive managed tool-execution boundary in a
  future OCG-owned runtime/communication path; keep OpenCode as a transitional
  compatibility runtime unless an adapter can prove the same contract.
- Inventory all native and extension-provided effectful tools for a specific
  runtime version; prove managed-only suppression or OCG execution, including
  shell, patch/write, MCP, and previously captured tool snapshots.
- Wire a thin edit adapter to the existing gateway and test real managed
  success, mechanical repair, stale/ambiguous conflicts, cancellation and
  alternate-path denial. Do not duplicate edit rules in the adapter.

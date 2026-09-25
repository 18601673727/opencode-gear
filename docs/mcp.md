# Thin MCP adapter

## Purpose and authority

`ocg mcp` is the model-facing adapter over the existing `ControlService`. It
runs in the foreground as one local child process and owns no application state:

```text
OpenCode/model -> MCP STDIO -> ControlService -> SnapshotService
```

`SnapshotService` remains authoritative. MCP never reads or writes Mission,
approval, Resource Registry, budget, snapshot, journal, or compatibility
projection files directly. Mutations use the same domain transitions and
post-commit cursor semantics as the CLI and loopback control API.

## Transport and project scope

Version 1 supports **STDIO only**:

```sh
ocg mcp
```

Protocol JSON is written only to stdout. Startup and human-readable output are
absent; process failures and diagnostics go only to stderr. Requests are handled
sequentially and each input frame is capped at 1 MiB.

The child resolves exactly one project boundary using the ordinary OCG rules:
the explicit CLI `--project` boundary, or the nearest ancestor containing
`.opencode-gear.yaml`. Tool schemas accept no root, project, path, command, or
filesystem argument, so a model cannot switch scope per call. The HTTP/SSE
server does not need to be running.

## OpenCode registration

OpenCode V2 2.0.15's installed CLI and current V2 documentation both use a
local command under `mcp.servers`. From the project root, either run:

```sh
opencode mcp add ocg -- ocg mcp
```

or add the equivalent project `opencode.jsonc` entry:

```jsonc
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "servers": {
      "ocg": {
        "type": "local",
        "command": ["ocg", "mcp"],
        "cwd": ".",
        "protocol": "legacy"
      }
    }
  }
}
```

`cwd` defaults to the OpenCode workspace; spelling it out documents the OCG
project boundary. `legacy` is OpenCode's default classic MCP initialize
handshake (through protocol revision 2025-11-25). No global configuration or
credentials are required. OpenCode prefixes model-visible tool names with the
server name, so server `ocg` exposes `ocg_ocg_state_summary` outside Code Mode;
the MCP tool's stable protocol name remains `ocg_state_summary`.

## Tools

Every successful read carries an authoritative cursor. List responses include
`total` and `truncated`; defaults and maxima are enforced by the adapter.

| Tool | Mode | Input | Output | Authority path and safety |
| --- | --- | --- | --- | --- |
| `ocg_state_summary` | read | `{}` | compact Mission/approval/resource/budget-block counts + cursor | one atomic `ControlService::state_summary` snapshot; no world dump |
| `ocg_mission_list` | read | optional `limit` (1–100, default 50) | newest-first compact summaries, total/truncated + cursor | `ControlService::missions`; runtime execution remains distinct from Mission identity |
| `ocg_mission_get` | read | `mission_id` | authoritative Mission detail + cursor | `ControlService::mission`; safe-id validated, no projection read |
| `ocg_events_replay` | read | `epoch`, `after`, optional `limit` (1–25, default 10) | strict-after events, request/current/last cursor and `has_more` | `ControlService::replay`; wrong epoch, expired and future are errors, never reset |
| `ocg_approvals_list` | read | optional `limit` (1–100, default 50) | durable approvals, total/truncated + cursor | one authoritative snapshot; bounded deterministic ordering |
| `ocg_resources_list` | read | optional `limit` (1–100, default 50) | descriptive durable observations, total/truncated + cursor | one authoritative snapshot; Unknown stays Unknown; no rank or selection |
| `ocg_budget_get` | read | `mission_id` | durable accounting + cursor | `ControlService::budget`; safe-id validated |
| `ocg_approval_approve` | write | `approval_id`, optional note (≤240 bytes) | outcome, updated record, post-commit cursor | existing `ControlService::resolve_approval`; cannot manufacture or rebind an approval |
| `ocg_approval_reject` | write | same | outcome, updated record, post-commit cursor | same generation/action-bound transition |
| `ocg_budget_set` | write | `mission_id`, positive integer `limit_micros`, validated `currency` | updated budget/revision + post-commit cursor | existing `ControlService::set_budget`, Mission CAS and currency invariants; historical spend is retained |

Approval and budget tools are correctly annotated as mutating and non-idempotent:
the existing domain transitions record each explicit operator assertion. An
approval never overrides a hard budget. There are no `force`, policy bypass, or
unsafe arguments.

## Replay and errors

Replay uses the durable `{epoch, seq}` journal, not an MCP buffer. It returns at
most the requested limit and says whether more retained events remain. Stable
tool error codes include `invalid_argument`, `not_found`, `conflict`,
`wrong_epoch`, `replay_expired`, `future_cursor`, `persistence_unavailable`,
`unsupported`, and `internal`. Error text is secret-redacted and capped at 400
bytes. Malformed JSON-RPC uses standard protocol errors and cannot invoke a
tool.

## Non-goals

MCP v1 provides no shell, arbitrary filesystem access, reconciliation,
placement/ranking, Resource mutation, runtime creation/replacement, provider or
model switching, failover, account rotation, raw authority mutation, live
subscription, HTTP MCP, or remote transport. It is not provider-dispatch
authority and does not add a second application layer.

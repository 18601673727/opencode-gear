# Loopback control plane

Phase 2B-2 adds a thin, loopback-only HTTP/1.1 + SSE surface over the durable
orchestration authority described in [replay.md](replay.md). The transport and
the service are deliberately separate:

```text
src/orchestration/control.rs   ControlService  transport-neutral, authority-backed
src/control_server.rs          HTTP/SSE server thin transport over ControlService
```

`ControlService` owns no durable state. Every read is an atomic
`snapshot_with_cursor`/`replay_after` against the Phase 2B-1 authority; every
mutation goes through the existing domain path (`policy::resolve_approval`,
`Mission::set_hard_budget` + `mission::save_if_revision`) and returns the
**post-commit** authoritative cursor. A caller can therefore resume an event
tail from any mutation response without a gap.

## Starting the server

```text
ocg serve [--addr 127.0.0.1:PORT]
```

- The default address is `127.0.0.1:0`, so the OS assigns a free port.
- On success the server flushes either `listening on http://127.0.0.1:PORT`
  or, with `--pretty`, the JSON object `{"listening":"http://127.0.0.1:PORT"}`,
  then runs until the process is terminated.
- Only numeric loopback addresses are accepted (`127.0.0.0/8` or `::1`). A
  wildcard (`0.0.0.0`, `[::]`), a routable address, a hostname or a malformed
  address is refused **before** the socket is created, and the bound address is
  re-checked to be loopback afterward.

There is no authentication, CORS, frontend, WebSocket support or reconciliation
endpoint. This is local operator tooling, not a network service.

## Client lifecycle

### Bootstrap

Fetch `GET /api/v1/snapshot`, apply the returned authoritative state, then open
`GET /api/v1/events?epoch=E&after=S` using that response's exact `{E,S}` cursor.
The atomic snapshot read followed by strict replay closes the bootstrap race.

### Reconnect

Keep the id of the last event successfully applied. Reconnect with the original
query cursor and `Last-Event-ID: E:S`; the header may advance, but never rewind,
the same-epoch query cursor. Replay is followed by live tailing.

### Reset

On `wrong_epoch`, `replay_expired`, `future_cursor`,
`persistence_unavailable`, or a live `reset_required` event, discard the resume
cursor and fetch a fresh atomic snapshot. The server never silently crosses an
epoch or returns a partial expired suffix.

## Request and response rules

| Rule | Bound |
| --- | --- |
| HTTP versions | `HTTP/1.1`, `HTTP/1.0` |
| Methods | `GET`, `POST`, `PUT` |
| Request head | 16 KiB (`MAX_HEADER_BYTES`) |
| Request headers | 64 (`MAX_HEADERS`) |
| Request body | 256 KiB (`MAX_BODY_BYTES`) |
| Serialized JSON response | 8 MiB (`MAX_RESPONSE_BYTES`) |
| Chunked bodies | rejected (`unsupported_transfer_encoding`) |
| Folded, duplicate or invalid-name headers | rejected (`malformed_request`) |
| Concurrent clients | 16 by default; excess receives `503 overloaded` |
| Per-socket read / write timeout | 10 s / 15 s; a slow consumer is dropped |
| Body framing | bytes beyond `Content-Length` are rejected |
| Keep-alive | not used; every response sends `Connection: close` |

All query and path values are percent-decoded; path identifiers must pass the
shared safe-id check (`is_safe_id`), otherwise the request is a typed
`invalid_request`/`invalid_id` failure.

## Error envelope

Every non-2xx response (and an SSE live failure, below) is:

```json
{ "error": { "code": "not_found", "message": "unknown Mission 'task-x'" } }
```

Messages are bounded to 400 bytes on a character boundary and serialized
through `telemetry::task::redact`, so a hostile filesystem path or payload echo
cannot be reflected unbounded and a credential-shaped value cannot be echoed
back. Some codes add structured detail fields that are not part of the message.

| `code` | Status | Extra fields | Meaning |
| --- | --- | --- | --- |
| `not_found` | 404 | | unknown route or unknown entity |
| `invalid_request` | 400 | | malformed or invalid input |
| `invalid_id` | 400 | | path identifier is not a safe id |
| `conflict` | 409 | | lost compare-and-swap / conflicting state |
| `wrong_epoch` | 409 | `expected_epoch`, `got_epoch` | cursor from a different epoch |
| `replay_expired` | 410 | `floor_seq`, `requested_seq` | cursor older than retained window |
| `future_cursor` | 409 | `head_seq`, `requested_seq` | cursor ahead of the head |
| `persistence_unavailable` | 503 | | authority missing, unreadable or corrupt (fails closed) |
| `method_not_allowed` | 405 | `Allow` header | route exists for another method |
| `malformed_request` | 400 | | unparseable request line, head or body |
| `malformed_json` | 400 | | a JSON body that does not parse |
| `payload_too_large` | 413 | | body over `MAX_BODY_BYTES` |
| `headers_too_large` | 431 | | head over `MAX_HEADER_BYTES` or too many headers |
| `unsupported_transfer_encoding` | 400 | | chunked request body |
| `http_version_not_supported` | 505 | | neither HTTP/1.0 nor HTTP/1.1 |
| `invalid_last_event_id` | 400 | | `Last-Event-ID` is not `epoch:seq` |
| `overloaded` | 503 | | concurrent client limit reached |
| `serialization_error` | 500 | | response could not be serialized |
| `response_too_large` | 503 | | serialized JSON exceeds 8 MiB |

`replay_expired`, `future_cursor` and `wrong_epoch` are **explicit failures**.
No partial replay is ever returned for an expired or out-of-range cursor.

## Routes

### `GET /api/v1/snapshot`

Reads the authoritative snapshot and its cursor under one lock, so the two can
never come from different writes.

```json
{
  "api_version": "v1",
  "schema_version": 1,
  "cursor": { "epoch": 1, "seq": 4 },
  "snapshot": {
    "missions": { "<mission-id>": { "…": "Mission" } },
    "approvals": { "<approval-id>": { "…": "ApprovalRecord" } },
    "resources": { "<resource-id>": { "…": "ResourceObservation" } }
  }
}
```

### `GET /api/v1/events?epoch=<u64>&after=<u64>`

Server-Sent Events stream: it first replays the retained journal strictly after
the cursor, then tails new events by polling the same authoritative reader.

- `epoch` and `after` are required. They establish the initial subscription
  cursor obtained from the atomic snapshot response.
- `Last-Event-ID: epoch:seq` may only **advance** the same-epoch query cursor:
  the effective cursor is `max(after, seq)`. A different epoch is a `409
  wrong_epoch`; a malformed value is a `400 invalid_last_event_id`.
- A bad initial cursor (wrong epoch, expired, future, storage) is a plain JSON
  error response with the status from the table above — the stream is never
  opened in a broken state.
- Once the stream is open, a later failure (retention pruning past the consumer,
  epoch change, authority lost) emits a final `event: reset_required` frame
  carrying the same envelope and `current_cursor` when it can still be read,
  then closes the connection. The client must fetch a fresh snapshot.

Each event frame is:

```text
id: 1:5
event: mission_upsert
data: {"cursor":{"epoch":1,"seq":5},"prev_hash":"…","hash":"…","event":{"kind":"mission_upsert","mission":{"…":"Mission"}}}

```

The id is `epoch:seq`; event kinds are `mission_upsert`, `approval_upsert`,
`resource_upsert` and `resource_replace`. Heartbeats are SSE comments with **no** `id:` field, so they
can never advance a client's resume position:

```text
: heartbeat 1758768000

```

The default poll interval is 200 ms and the default heartbeat is 10 s; a
heartbeat is only emitted after that much idle time.

### `GET /api/v1/approvals`

```json
{ "approvals": [ { "…": "ApprovalRecord" } ], "issues": [ { "…": "ApprovalIssue" } ] }
```

Read-only. Corruption is surfaced as an issue, never hidden.

### `POST /api/v1/approvals/{id}/approve` and `…/reject`

Optional body:

```json
{ "note": "why" }
```

`note` must be a string when present. The mutation runs through the
authority-backed approval path and returns the post-commit cursor plus the
resolved record:

```json
{ "cursor": { "epoch": 1, "seq": 6 }, "approval": { "…": "ApprovalRecord" } }
```

An unknown approval is a typed `404 not_found`.

### `GET /api/v1/resources`

```json
{
  "resources": [ { "…": "ResourceRecord" } ],
  "issues": [ { "target": "<resource>", "detail": "…" } ],
  "corrupt": false
}
```

Read-only. `corrupt` reports an authority or registry corruption without making
the whole route fail.

### `GET` / `PUT /api/v1/budgets/{mission}`

`GET` reads the durable budget; `PUT` explicitly sets the hard cap and is the
only API path past it. `PUT` requires both fields:

```json
{ "limit_micros": 500000, "currency": "USD" }
```

Both responses are the same `BudgetView`:

```json
{
  "cursor": { "epoch": 1, "seq": 7 },
  "mission_id": "task-x",
  "revision": 3,
  "changed": true,
  "budget": { "…": "MissionBudgetReceipt" }
}
```

`changed` is `false` on `GET` and reports whether the `PUT` committed a new
budget. `limit_micros` must be an integer and `currency` a string that passes
`normalize_currency`; anything else is a `400 invalid_request`. An unknown
Mission is a `404 not_found`. A lost revision compare-and-swap is a `409
conflict`, never a silent overwrite.

## Mutation guarantees

- A mutation is committed by the existing domain authority, then refreshed into
  the compatibility projections. The returned cursor is read **after** the
  commit, so a client can tail `/api/v1/events` from it without missing the
  change it just made.
- Mutations preserve the current domain behavior. Approval resolution records
  an explicit operator transition on each call; budget updates use Mission CAS.
- `PUT` on a budget always produces a durable revision/event, including when the
  value is unchanged, because the domain setter advances the revision.

## Non-goals

- No remote exposure: loopback only, and no auth/CORS/TLS layer.
- No frontend, no WebSocket, no MCP.
- No reconciliation route; `ocg reconcile` stays an explicit CLI action.

## Tests

- `tests/control_server_tests.rs` covers atomic bootstrap, replay-then-tail
  ordering and gap-freedom, reconnect via `Last-Event-ID`, invalid/expired/future
  cursors, replay/live race, heartbeats without ids, typed route/method/body
  errors, loopback-only binding, and that a mutation is visible to an independent
  authority reader.
- `tests/control_cli_tests.rs` spawns the real `ocg serve` binary and performs a
  cross-process budget mutation through HTTP.
- `src/orchestration/control.rs` unit-tests the service and the bounded,
  redacted error envelope.

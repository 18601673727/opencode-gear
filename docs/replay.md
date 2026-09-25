# Durable replay authority

Phase 2B-1 introduces one durable authority for the domains OCG already treats
as product state: Missions, Policy approvals and normalized Resource
observations.

## Authority

```text
.opencode-gear/orchestration/replay/
  state.json      authoritative snapshot + hash-chained event journal
  state.lock      stable advisory lock file (never deleted)
.opencode-gear/orchestration/replay.initialized
                  initialization/epoch witness outside the replay directory
```

`state.json` is the authority. The existing per-domain files remain
**compatibility projections**:

```text
missions/<id>.json            approvals/<id>.json         resources/registry.json
             ▲                            ▲                            ▲
             └────────── written after the authority is committed ────┘
```

A projection write happens *after* the matching event is durably committed to
the authority. If a projection write fails, the authority already holds the
update, so re-running the same save is idempotent.

## Cursor and atomicity

A cursor is `{ epoch, seq }`. `seq` is assigned under a project-wide
cross-process advisory file lock (`fs2`, `flock`-based) and strictly increments
within an epoch. `SnapshotService::snapshot_with_cursor()` takes the same lock as
`append()`, so a caller can never observe a snapshot and cursor from two
different writes. `append()` applies the event, advances the cursor and writes
the document in one atomic durable write (temp sibling fsynced, renamed, parent
directory fsynced).

A no-op upsert — the incoming value already equals the authoritative value —
does not create an event and does not advance the cursor.

## Authority-backed reads

Once the authority is initialized (its document **or** its marker exists), the
public domain readers are served by the authority:

```text
mission::load / mission::list / mission::find_by_session
policy::load_approval / policy::list_approvals
resources::load
```

Only before the first initialization do they consult the legacy projections, so
an existing install is migrated by the first `SnapshotService::open` bootstrap.
A read never initializes or rewrites the store. An initialized-but-unreadable
authority fails closed using each API's existing error/corrupt representation
(`Err`, a counted corrupt Mission, or a surfaced issue) — it is never treated as
"absent". Because the authority is the read source, a Mission that remains in the
authority is still listed and loaded even when its projection file was removed or
failed to write.

Public writers (`mission::save`, `policy::save_approval`, `resources::save`) and
the read-modify-write seams commit through the authority first and then update
the compatibility projection.

## Domains and events

Only durable domains enter the snapshot:

```text
MissionUpsert    full Mission (lifecycle, receipts, budget, history)
ApprovalUpsert   ApprovalRecord, pruned to MAX_APPROVALS
ResourceUpsert   one normalized durable ResourceObservation
ResourceReplace  the complete normalized resource set, atomically
```

`ResourceReplace` is the low-level `resources::save` transition. It replaces the
whole authoritative resource map in a single document write, so a batch of
observations (including removals and an emptied registry) can never be observed
half-applied. The authoritative resource map is bounded by the domain's own
`MAX_RESOURCES`, and approvals by `MAX_APPROVALS` with the domain's pruning rule
(oldest resolved first; a pending record is never dropped while a resolved record
could be removed instead).

Raw OpenCode events, UI, messages, token/runtime-only data and HTTP/SSE traffic
are explicitly **not** authority and are never stored.

## Monotonicity

Within a generation a Mission's `revision` is a strict mutation witness: an
upsert that lowers the revision, or that changes content at the same
generation/revision, is rejected rather than committed. An exact no-op is
accepted and records nothing. A generation may only advance; a new generation may
reset the revision. Callers that legitimately re-persist a stale snapshot merge
the authoritative fields first and advance the revision for the resulting content
change.

## Validation and fail-closed

Loading validates, in order: the schema version, the epoch (nonzero; a
continuity-loss reason is required once the epoch is greater than one), the
retained journal length against `MAX_RETENTION`, epoch/head/floor consistency,
contiguous retained sequences, the chained envelope hashes, the snapshot state
digest, and every domain identity (map key vs. value identity) using the domain's
own validator. Integrity hashing is fallible: a serialization error fails closed
instead of hashing a defaulted value. Any corrupt or ambiguous state fails closed;
nothing is silently reset.

The initialization marker and the document must agree on the epoch. A missing
`state.json` while `initialized` is present is a persistence failure, not a
bootstrap.

### Strict bootstrap

The first initialization reads the legacy projections read-only and strictly: a
Mission directory or entry **read** error propagates, an unreadable, malformed or
unsupported record aborts the bootstrap, and a quarantined
`<id>.corrupt.json` artifact blocks it as unresolved corruption. Corruption is
never silently bootstrapped over or partially incorporated.

## Retention

The journal is bounded (`DEFAULT_RETENTION = 256`, test-configurable via
`SnapshotConfig`). Pruning advances the retention floor and keeps the hash of the
last pruned envelope as an anchor, so the retained suffix is still hash-chained.

`replay_after(cursor)` reports one of:

```text
Success(events)  events strictly after the cursor
Empty            the cursor is already at the head
WrongEpoch       the cursor belongs to another epoch
Expired          the retained prefix no longer covers the cursor
Future           the cursor is ahead of the head
PersistenceFailure  the store is missing, unreadable or invalid
```

`Expired` never returns a partial suffix.

## Epochs

Ordinary operation never changes the epoch. A continuity loss is resolved only
by the explicit operator assertion:

```rust
SnapshotService::begin_new_epoch_after_continuity_loss(root, snapshot, reason)
```

The reason must be nonempty and bounded. The next epoch is derived from the
prior state/marker epoch with a checked increment (an exhausted epoch is an
error, never a saturating reuse). The new epoch starts empty with the
caller-supplied snapshot; every cursor from the previous epoch then reports
`WrongEpoch`.

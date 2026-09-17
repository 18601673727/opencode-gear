# Token efficiency and local measurements

OpenCode Gear prepares a bounded, deterministic local context and distils
noisy command output before any model sees it. This document records one
representative, reproducible measurement of those two reductions on a specific
machine. It is **not** a benchmark, not a universal claim and not a promise of
speed or savings on any other machine.

The measurement is a deterministic offline test, so the byte counts below are
exact and reproducible. Timings use `std::time::Instant` and are only recorded;
the test never asserts them.

## What "reduction" means here

| Quantity | Unit | Meaning |
| --- | --- | --- |
| candidate bytes | bytes | the summed size of every ranked repository file the context engine considered |
| selected bytes | bytes | the summed byte size of the bounded context slices actually selected |
| capsule bytes | bytes | the JSON size of the structured task capsule in the plan |
| context reduction | bytes / % | `candidate - selected`, over candidate |
| raw log bytes | bytes | the captured output of a verification command |
| distilled bytes | bytes | the deterministic JSON size of the distilled summary |
| log reduction | bytes / % | `raw - distilled`, over raw |

Tokens are **never** reported as exact unless a real source supplied them.
The only token number in these flows is an estimate: the context plan estimates
`selected_bytes / 4` and labels it estimate-only (`source: "estimated"`).
Provider-reported and OpenCode-reported token counts require a session hook that
OpenCode Gear deliberately does not ship yet; those fields stay `unknown`.

## Fixture

`tests/measurement_tests.rs` builds a uniform temporary repository:

- 120 Rust modules (`src/module_<n>.rs`), each with two functions;
- a `README.md` and a `Cargo.toml`;
- a fake Git host that reports "not a repository", a fixed clock, and the
  repository's own default context policy.

The flow is:

1. **cold** — first plan: repo map, full symbol index, ranking, slices, cache
   write;
2. **warm** — identical plan served from the dependency-checked context cache;
3. **incremental** — `src/module_1.rs` changes: one index entry updates, the
   rest are reused, and the plan misses the cache for exactly that dependency;
4. **log distillation** — a noisy 400-progress-line build log with duplicate
   warnings and errors is distilled.

## Recorded measurement — 2026-09-17

One run, first observation, debug test profile.

### Environment

| Item | Value |
| --- | --- |
| OS | Debian GNU/Linux 13 (trixie) |
| Kernel | Linux 6.12.107+deb13-arm64 |
| Architecture | aarch64 (ARM64) |
| Virtualization | Apple Virtualization guest |
| vCPU / RAM | 4 / ~5.8 GiB |
| Rust | rustc 1.98.1 (48a229cea 2026-09-01), LLVM 22.1.8 |
| Cargo | cargo 1.98.1 |
| Build | `cargo test`, debug profile |

### Deterministic byte results

| Metric | Value |
| --- | ---: |
| fixture modules | 120 |
| candidate bytes | 12,729 |
| selected bytes | 2,044 |
| capsule bytes | 13,255 |
| context reduction | 10,685 bytes (83.9%) |
| raw log bytes | 14,399 |
| distilled bytes | 666 |
| log reduction | 13,733 bytes (95.4%) |

Orchestration hand-offs, from the same fixture (task
`update module_1 parse_1 parser`, Explore → Build → Verify → Debug):

| Metric | Value |
| --- | ---: |
| candidate context bytes | 12,729 |
| selected source bytes | 1,752 |
| rich capsule bytes | 21,642 |
| Explore → Build hand-off | 2,526 |
| Build → Verify hand-off | 2,620 |
| Verify → Debug hand-off | 1,079 |
| model dynamic context bytes | 4,143 |

This fixture deliberately configures the tight `4096`-byte / `40%` regression
gate (not the runtime envelope of `16384` / `60%`). Every hand-off is below
4,096 bytes and below 40% of the 21,642-byte rich task context (8,656 bytes).
The Build → Verify figure is an actual typed `ModelHandoffCapsule` produced
after automatic verification from a refreshed context plan (current changed
files, symbols and a real bounded diff), not just the serialized verification
block. The fixture also asserts that the goal, the hard constraint, the relevant
symbol `parse_1`, a critical finding and the failing location
`src/module_1.rs:4:5` survive the projection — evidence is not dropped to hit a
byte target.

Read honestly:

- **Selected is smaller than candidate by design.** Candidate sums every ranked
  file; selected is bounded by `context.maxFiles` / `context.maxSlices` /
  `context.maxBytes`.
- **The capsule is not context slices.** It is the structured task capsule
  (task fingerprint, selected paths, symbols, verification state) and is larger
  than the small fixture's selected bytes.
- **A hand-off is smaller still.** It is the compact projection of the rich
  capsule *and* the selected source: source slices travel separately in the
  dynamic context and never enter the compact capsule.
- **The estimated token count for this plan is `2044 / 4 = 511`** and is labelled
  `estimated` in telemetry. It is not a provider measurement.

### Timings

| Stage | First run (ms) | Repeated runs (ms) |
| --- | ---: | ---: |
| cold (map + index + plan) | 12.540 | 12.54 – 12.86 |
| warm unchanged (cache hit) | 8.427 | 8.23 – 8.45 |
| incremental (one file changed) | 11.472 | 11.32 – 11.75 |

The spread across repeated runs reflects ordinary machine load, not a change in
the fixture or the engine. The capsule byte count grew by 5 bytes versus the
0.1.0 record because the engine-version string (`0.2.0-rc.3`) is embedded in
provenance; the content is otherwise unchanged.

Warm still rebuilds the repo map and revalidates the index; it only skips
ranking, slicing and file reads. That is why warm is faster but not free.

## Reproduce

```bash
cargo test --test measurement_tests -- --nocapture
```

The test prints two blocks:

```text
OCG_MEASUREMENT version=1 fixture_modules=120
context candidates=12729 selected=2044 capsule=13255 reduction=10685 reduction_percent=83.9
log raw=14399 distilled=666 reduction=13733 reduction_percent=95.4
timing cold_ms=12.540 warm_ms=8.427 incremental_ms=11.472
OCG_MEASUREMENT_END
OCG_ORCHESTRATION_MEASUREMENT version=1 fixture_modules=120
candidate_context_bytes=12729 selected_source_bytes=1752 rich_capsule_bytes=21642
explore_to_build_handoff_bytes=2526 build_to_verify_handoff_bytes=2620 verify_to_debug_handoff_bytes=1079 model_dynamic_context_bytes=4143
OCG_ORCHESTRATION_MEASUREMENT_END
```

## Limitations

- **One run, one fixture, one machine.** The byte counts are exact for this
  fixture; the timings are a single debug-profile sample and are not a
  benchmark.
- **No extrapolation.** Do not project these numbers onto macOS, x86-64,
  release builds, different CPUs, different repositories or real-world mixed
  codebases.
- **Estimates stay estimates.** The only token figure is `bytes / 4`; it is
  labelled `estimated` everywhere it appears.
- The timings include filesystem and process overhead outside Gear's control.

For the telemetry schema, privacy model, `ocg stats` output and `ocg doctor`
checks, see [telemetry.md](telemetry.md).

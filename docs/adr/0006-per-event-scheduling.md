# ADR-0006: Per-event scheduling

- Status: Accepted
- Date: 2026-05-17
- Supersedes the batch-advance model implicit in ADR-0002.

## Context

ADR-0002 established that `advance` is a reporter, not a trigger.
Implementation-wise, however, we kept the schedule running *at advance
time*: ingest queued up events (and `Unprocessed` markers), and a
single batch run of the schedule fired when the user called
`advance()` or `materialize()`.

That batch model has a real cost. During replay, all events apply
first (spawning entities, flipping status flags), and *then* the
schedule runs once over the union. Any system whose logic depends on
"what state was the world in when this specific event arrived" has to
encode that temporally — by hand, per system.

We already hit one instance: archiving a project after a signal
matched was erasing the signal's effect on replay, because the
matching system saw the archived flag (set by the later-in-log archive
event) before it could process the earlier signal. We patched it with
a `closed_at` field on Project and an `accepts_signal_at` check in the
matching system — a temporal gate, hand-coded.

The patch worked, but the underlying pattern was clear: every future
system that depends on "world state at event time" would need its own
gate, with its own bugs.

## Decision

Run the schedule **after every event**, not at advance time.

`apply_event` becomes a method (`apply_and_derive`) that applies the
event and then immediately runs the schedule against the resulting
world. Both `ingest()` and the per-event step of `replay()` use it.

`advance()` becomes a pure cursor-arithmetic reader: filter records in
the in-memory `ChangeLog` whose triggering event id is past the
cursor, update the cursor, persist, return.

`materialize()` is removed from the public API. State is always
derived; reads never need to ask for derivation.

## Why this is right

Each event's schedule run sees only the world as of that event. The
matching system processing a signal sees the project as Active because
the archive event hasn't happened yet (from matching's perspective).
The temporal gate disappears — and so does the pattern that produced
it.

Concretely:

```
Log:        ProjectCreated → SignalObserved → ProjectArchived
            t=0              t=1             t=2

Replay under per-event:
  t=0: spawn project (Active), schedule runs (no signals → no-op)
  t=1: spawn signal (Unprocessed), schedule runs
         → matching: project is Active, bump relevance, despawn signal
         → decay: 1-second elapsed, negligible
  t=2: flip status to Archived, schedule runs
         → matching: no unprocessed signals
         → decay: project Archived, skip
```

State at end of replay: project is Archived, relevance bumped from the
t=1 signal. No `closed_at`, no `accepts_signal_at`, no per-system
temporal gates.

## Trade-offs

- **Replay cost grows.** Schedule runs once per event instead of once
  per advance. With today's two systems and modest event counts the
  cost is negligible (microseconds × N). At ~10k events the cost
  becomes noticeable; the mitigation is the snapshot mechanism already
  on the v2 roadmap (ADR-0005).
- **ChangeLog accumulates.** Records from every per-event schedule
  invocation pile up in the in-memory `ChangeLog`. `advance()` filters
  by cursor for output; memory usage is the cost. For long-lived
  processes this would matter; for the CLI (start, do thing, exit) it
  does not.
- **No more `materialize()`.** API surface shrinks. Anything that
  needs derived state just queries.

## What ADR-0006 unlocks

- Every future system gets "I see only the past" for free.
- Adding systems with timing-sensitive logic (alignment, decay,
  recommendation thresholds) no longer requires hand-coded temporal
  gates.
- `advance()` is finally what ADR-0002 promised: a pure reporter.

## What ADR-0006 rules out

- "Lazy" systems that defer work until advance. If a system is
  expensive, the right answer is to optimize it or split it, not to
  batch it. Lazy systems would reintroduce the batch-time coupling.
- Per-advance summarization. If we want compressed per-advance output
  (e.g., "30 days of decay" instead of 30 individual records), that
  becomes a formatting concern at the reporter level, not a scheduling
  decision.

## Consequences

- `closed_at` field on Project removed.
- `accepts_signal_at` method on Project removed.
- Matching system reverts to a simple `status == Active` check.
- `materialize()` method removed from `WorldRuntime`.
- CLI `inspect` no longer calls `materialize()`.
- `advance()` no longer runs the schedule.
- All existing tests pass under the new model with minor edits.

## Cross-references

- ADR-0002 — Advance is a reporter, not a trigger (this ADR fully
  realizes it).
- ADR-0003 — Event log is inputs-only (per-event scheduling reinforces
  this: each event triggers its own derivations, regenerated on
  replay).
- ADR-0005 — Storage roadmap (snapshots become more important as
  per-event scheduling makes replay cost more visible).
- `crates/world/src/runtime.rs` — `apply_and_derive`, `advance`.
- `crates/world/src/systems.rs` — matching/decay with simple status
  check.

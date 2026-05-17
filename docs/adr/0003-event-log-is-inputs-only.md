# ADR-0003: Event log is inputs-only

- Status: Accepted
- Date: 2026-05-16

## Context

The kickoff doc listed `WorldEvent { ProjectCreated, GoalCreated,
SignalObserved, ProjectUpdated, RecommendationCreated }`. Read naively,
that mixes user-driven events (Project/Goal/SignalObserved) with
system-emitted events (ProjectUpdated from the matching system,
RecommendationCreated from a future recommendation system).

We have to decide whether the event log holds only inputs or also
derived outputs.

## Options considered

- **A. Inputs-only.** Log holds events the user / outside world ingest.
  System outputs are Changes (in-memory + `last_advance.json`), never
  appended to the log. Replay re-derives outputs every time.
- **B. Mixed.** Log holds both. Systems append their derived events
  back to the log after each Advance. Replay must skip system-emitted
  events. Old explanations stay byte-frozen.
- **C. Inputs-only + snapshots.** Log holds inputs. A separate
  snapshots directory periodically freezes derived state for audit.

## Decision

A. Events are inputs-only. System outputs are Changes, not Events.

## Why not B

Determinism. "Same events → same world state" only holds if events are
the cause, not also the effect. With mixed logs:

- A future change to the matching algorithm cannot retroactively
  improve old derivations — they are baked in. We would have to
  introduce migration tooling for replay correctness.
- Replay logic has to thread "skip derived" filters everywhere.
- Scenario branching (future feature) gets ambiguous: do hypothetical
  branches re-derive or inherit baked-in outputs?
- Deletion test: if we removed all system-emitted variants from the
  enum, every direct consumer would still work — they read Changes,
  not log entries. That marks them as pass-throughs in the log.

The one thing mixed buys is permanent audit history of "what the
system said at the time". That is valuable but better served by
periodic snapshots (option C) when we actually need replay-history.
For v1, replay always reflects the current algorithm.

## Why not C yet

We do not have a use case for replay-history yet. The cost of building
snapshots and a snapshot-aware reader is real (file format, retention
policy, replay-from-snapshot path). One adapter = hypothetical seam.
Defer until evidence demands it.

## Consequences

- `WorldEvent` enum holds only user / outside-world events. v1 today:
  `ProjectCreated`, `GoalCreated`, `SignalObserved`. (ADR-0004 will
  cover the v1 addition of edit events.)
- System outputs are `ChangeRecord`s, materialized into the in-memory
  `ChangeLog` resource and the per-advance `last_advance.json` file.
- If we change the matching algorithm in a way that meaningfully shifts
  derivations, last week's `explain` output is no longer reproducible
  byte-for-byte. We accept this; mitigate later with snapshots if it
  bites.
- `RecommendationCreated` is not a log variant. When we build
  Recommendations, they will be ChangeRecords (or a sibling derived
  type), not Events.

## Cross-references

- `docs/CONTEXT.md` § Event, § Change.
- `crates/world/src/events.rs` — `WorldEvent` enum.
- `crates/world/src/explanation.rs` — `ChangeRecord`, `ChangeLog`.

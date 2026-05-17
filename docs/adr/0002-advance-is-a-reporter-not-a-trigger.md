# ADR-0002: Advance is a reporter, not a trigger

- Status: Accepted
- Date: 2026-05-16

## Context

The kickoff phrasing "Runtime advances the World → system updates strategic
relevance → user inspects the project" reads as though `advance` is the
mutator: state does not change until the user calls `advance`.

The deterministic-replay model makes this distinction observably hollow.
The event log is the canonical history; replaying the same events into
the same systems always produces the same derived state. So "when does
derivation happen?" is an implementation detail, not a user-facing
property.

We have to choose how that detail surfaces in the CLI and the public
API.

## Options considered

- **A. Auto-derive on read; advance reports the delta.** Inspect, explain,
  and any future query auto-derive state. `advance` runs the schedule,
  filters Changes by an event-id cursor, persists the cursor, and writes
  `last_advance.json` for `explain latest`.
- **B. Strict tick.** Advance is the only moment derived state changes.
  Inspect must show post-last-advance state, so we either persist a
  derived snapshot or refuse to inspect when unprocessed signals exist.
- **C. Hybrid.** Auto-derive on read (like A), but `explain latest` reads
  the last persisted advance rather than live-deriving. (Effectively
  what A already does.)

## Decision

A. Advance is a reporter, not a trigger.

`advance` runs the schedule, emits the Changes whose triggering Events
fall past the cursor, and moves the cursor forward. State derivation is
not gated by advance — `inspect` and `explain` always reflect the full
event log. The cursor file (`.liferuntime/cursor.json`) records the
last-advanced event id so delta reporting is consistent across CLI
invocations.

## Why not B

The simulation metaphor is appealing — "the world ticks" is a clean
mental model. But it forces ceremony: every read becomes "have I
advanced?". The user's actual workflow is:

> "I add a signal. I look. I see what changed."

In B, that becomes: "I add a signal. I advance. I look." The extra step
buys nothing observable — the state shown to the user is identical.
Worse, it punishes incidental reads (autocomplete, scripted queries,
ad-hoc inspection) by either rejecting them or showing stale data.

We may revisit if the runtime grows: when systems take real time to
execute (LLM calls, heavy projections), gating derivation behind an
explicit user action becomes valuable. Today, all derivation is
pure-CPU and effectively free, so the ceremony has no payoff.

## Consequences

- The `materialize()` method on `WorldRuntime` is load-bearing: reads
  rely on it. It is currently public for CLI use; should stay there
  until we have a cleaner read API.
- `last_advance.json` is the source of truth for `explain latest`. It
  ages out the moment a new `advance` runs.
- The cursor mechanism is now part of the contract, not just an
  implementation detail. Document it in CONTEXT.md (done).
- If we later introduce expensive systems (LLM-backed), revisit this
  ADR: explicit-tick semantics become worth the ceremony.

## Cross-references

- `docs/CONTEXT.md` § Advance, § Cursor — domain vocabulary.
- `crates/world/src/runtime.rs` — `advance`, `materialize`, cursor
  load/save.

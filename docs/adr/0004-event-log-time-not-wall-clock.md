# ADR-0004: Time-dependent logic uses event-log time, not wall clock

- Status: Accepted
- Date: 2026-05-16

## Context

We are about to introduce Decay (ADR-0002 establishes that systems can
silently derive state on read; the Decay system will be one of the
systems that runs). Decay needs a notion of "how much time has passed
since the last relevant Signal" to know how far a Project's
strategic_relevance should drift back toward baseline.

We have two sources of time available:

- **Wall-clock time** via `chrono::Utc::now()`. Easy.
- **Event-log time** — the timestamp of the most recent Event in the
  log. Stable across replays.

If Decay reads wall-clock time, replaying the same event log tomorrow
yields different derived state than today. That breaks the
deterministic-core property (ADR-0001, ADR-0003).

## Decision

All time-dependent logic in the runtime reads time from the event log,
never from the wall clock.

Concretely: a system that needs "now" calls a `Now` resource backed by
the last event's `occurred_at`. Decay, future trajectory-over-time
queries, and any time-windowed Recommendation logic all use this
resource.

Wall-clock time appears in exactly one place: ingest, where a fresh
`StoredEvent` is timestamped with `Utc::now()` at append time. Once
written to the log, that timestamp is the only legal source of "when".

## Consequences

- The runtime is fully replayable. Running `replay` tomorrow produces
  the same derived state as running it today, given the same log.
- The CLI cannot show "TNT has cooled in the last 3 days of real time"
  if the event log has no events from the last 3 days. From the
  runtime's view, no time has passed. This is a feature: continuity of
  cognition only progresses when the log progresses.
- Tests can inject synthetic event timestamps to reproduce
  time-dependent behavior without `sleep` or `mock_time` hacks.

## What this rules out

- No `cron`-style passive ticks. The world does not "tick" on its own;
  only ingested events move it forward.
- No "time of day" or "day of week" features baked into systems. If we
  want those, they read from an explicit Event the user (or a cron job
  user-side) ingests, not from `Utc::now()` at advance time.
- No expiry / TTL using wall clock. Same reason.

## Open question deferred to a future ADR

For long stretches with no events (user goes on vacation, no signals
land), event-log time stops advancing — so Decay stops. We may want a
synthetic `TimePulseObserved` Event that the user (or an automated
cron user-side) can ingest to mark "time has passed without news".
That preserves the deterministic property: the pulse Event itself is
the source of time advancement, and replay sees the same pulses.

Defer until the absence is felt.

## Cross-references

- `docs/CONTEXT.md` § Event-log time, § Decay.
- ADR-0001 (CLI-first local runtime) — local-first means we control
  the clock surface.
- ADR-0003 (Event log is inputs-only) — replay determinism is the
  reason this ADR exists.

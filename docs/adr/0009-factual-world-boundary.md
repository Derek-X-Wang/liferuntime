# ADR-0009: Factual World boundary — copy-in, deferred build

- Status: Accepted
- Date: 2026-06-06

## Context

LifeRuntime's purpose (see `docs/CONTEXT.md` `#purpose`) is to surface
where a user's attention is being pulled across a *many-project* life.
The motivating insight: coding agents made execution ~10× cheaper, so
the user now runs far more concurrent projects than before, and the
bottleneck has shifted from building to *deciding where to point their
judgment*. Hand-feeding Signals to 20–30 projects does not scale —
external trend signals (AI news, market moves, ecosystem shifts) flowing
into the projects they touch is what would make the many-project vision
actually work.

That capability is the **Factual World**: a shared external source of
fact-signals that exist independent of any one user, which a Personal
World could subscribe to (the long-term "composability" frame — see
`docs/agents`-adjacent memory and CONTEXT.md `#personal-world`).

It is **not built**, and the user's world today has zero projects in it.
We faced a choice: design it now while the reasoning is fresh, or defer
entirely until dogfooding produces evidence. An independent review
(Codex, 2026-06-06) split the difference, and we adopt that split.

The risk that makes *part* of this worth settling now: the Factual World
constrains the **event-log schema** (`SignalObserved` would need
provenance fields) and the **replay-determinism guarantee** (a shared
upstream log is a determinism hazard). ADR-0005 lets us migrate the
schema but warns against changing event tags casually. Schema/replay
decisions are expensive to reverse and — crucially — decidable *without*
dogfood data, because they are pure architecture, not product design.

## Decision

**Bank the boundary invariants now. Defer all product/source design and
all implementation until a measured dogfood trigger fires.**

### Boundary invariants (settled now)

1. **Copy-in, never replay-time reference.** A subscribed Factual signal
   becomes an ordinary `SignalObserved` Event in the Personal World's own
   `events.jsonl`. The Personal World replays from its own inputs alone.

2. **Factual World is a signal *source*, not a second runtime kind.**
   There is no separate world to replay, no cross-world subscription
   resolved at replay time. It produces *proposed* `SignalObserved`
   inputs — conceptually a sibling of the [AgentBridge](../CONTEXT.md)
   seam (propose → runtime decides → ingest), not a new core.

3. **Provenance is metadata on the ingested signal:** source name,
   upstream event id or content hash, fetched/observed time, optional
   URL/title. This is the schema-shaping decision — when we add external
   ingestion, `SignalObserved` (or an envelope around it) carries these
   provenance fields.

4. **Dedup via `idempotency_key`.** Shape:
   `factual:<source_namespace>:<upstream_event_id_or_content_hash>`.
   Retries are no-ops (the mechanism already exists, see CONTEXT.md
   `#idempotency-key`).

5. **Replay never touches the network.** No upstream log, LLM, RSS, API,
   or wall-clock fetch happens during replay. Replay reads the local
   event log and nothing else.

The single load-bearing sentence:

> A Personal World is replayable from its own event log; Factual
> subscriptions copy accepted facts into that log as `SignalObserved`
> inputs with provenance, and replay never references upstream logs.

### Deferred until dogfood (explicitly NOT settled now)

Signal source types (RSS / news API / pasted newsletter / webhook),
polling cadence, ranking/filtering of incoming facts, subscription UX,
auth, trust scoring per source, and conflict semantics. These all need
real usage data about *what* signals the user reaches for and *how
often* — designing them before that data is the premature-abstraction
trap ADR-0001 warns against.

### Build trigger (so the deferral is not open-ended)

Build the Factual World only after manual dogfooding shows repeated
external-signal pressure. The bar:

- **Primary:** ≥20 manually-ingested external signals/week for 3
  consecutive weeks, touching ≥8 active Projects, where ≥30% of those
  signals change ranking or explanations enough to affect attention.
- **Secondary:** the user skips manual ingestion for a week, then
  identifies ≥5 missed external signals that would have changed project
  attention. This proves the bottleneck is *signal acquisition*, not too
  few projects or weak matching.

Either trigger flips the Factual World from "nice idea" to "build it,"
and at that point this ADR's invariants govern the design.

## Why copy-in, not reference-by-pointer

Reference-by-pointer (the Personal World stores a pointer into a shared
upstream Factual log, resolved at replay time) breaks the core promise
— "same events → same world" — because the upstream log is shared,
append-only, and can be reordered or extended differently across
consumers. Replay would depend on a mutable external source.

The only way to make a pointer safe is to make it content-addressed,
immutable, locally available, and carry enough data to replay without
fetching upstream — at which point it is functionally copy-in with extra
indirection. So copy-in is not a compromise; it is the only shape that
preserves determinism. Provenance metadata gives us the audit trail
("this came from source X, upstream id Y") without the replay
dependency.

## Why settle the schema-shaping bit now (invariant 3) under a deferral

It looks contradictory to defer the feature yet pin a `SignalObserved`
provenance shape. The reason: ADR-0005's compat contract makes event-tag
and field changes a migration-gated cost. Deciding *now* that external
signals are copy-in `SignalObserved`-with-provenance — rather than, say,
a new `FactualSignalObserved` variant or a reference event — means that
when we do build it, we extend an existing variant with optional
provenance fields (additive, no migration) instead of discovering mid-
build that we need a structurally different event. The decision is free
today and saves a migration later.

## Consequences

- No code today. `SignalObserved` is unchanged until the trigger fires.
- When built: external ingestion is an `AgentBridge`-shaped seam (a
  sync/poller adapter proposes candidate `SignalObserved`s; the runtime
  accepts/rejects/ingests). It reuses the existing propose→ingest
  boundary rather than inventing a new one.
- `SignalObserved` gains optional provenance fields when external
  ingestion lands — additive under ADR-0005, no migration.
- The "Personal vs Factual world boundary" deferred design thread (the
  composability vision) is now partially resolved: the *boundary
  mechanic* is settled (copy-in + provenance), even though the *product*
  is deferred. A future dedicated grill on composability inherits these
  invariants as fixed constraints.

## What this ADR rules out

- A second replayable world kind inside the Personal World's replay.
- Replay-time dependence on any external/shared log.
- A distinct `FactualSignalObserved` event variant (use `SignalObserved`
  + provenance).
- Open-ended deferral with no measurable build trigger.

## Cross-references

- ADR-0001 — Factual World feed deferred; `SignalObserved`
  forward-compatible with external signals. This ADR makes that
  forward-compatibility concrete.
- ADR-0003 — Event log is inputs-only. Copy-in keeps external facts as
  inputs; nothing derived enters the log.
- ADR-0005 — Storage roadmap + compat contract. Invariant 3 is shaped to
  stay additive under this contract.
- `docs/CONTEXT.md` — `#purpose`, `#factual-world`, `#personal-world`,
  `#agentbridge`, `#signal`, `#idempotency-key`.
- Independent review: Codex, 2026-06-06 (A-vs-B tie-break → B-narrow:
  bank the boundary, defer the product).

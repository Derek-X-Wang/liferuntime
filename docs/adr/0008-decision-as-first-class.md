# ADR-0008: Decision as a first-class entity

- Status: Accepted
- Date: 2026-05-19

## Context

The kickoff vision for LifeRuntime listed **Decision** as a first-class
concept alongside Project, Goal, and Signal — the recorded moment a
user weighs strategic options and picks one ("I'm putting weight on
TNT, not Side-X"). V1 shipped without it: there was no consumer yet
(no recommendation engine), so adding the concept would have violated
the project's "no abstraction without a second use case" discipline.

That changes once we look at the *Trust surface* the runtime is meant
to provide. Two soon-coming systems both need "the user already
decided this" context to behave correctly:

- The Attention Prompt engine (sketched in CONTEXT.md `#recommendation`)
  must not nag the user about a project they've explicitly de-prioritized.
- Explanations (`explain`) must be able to cite the *strategic act*
  that biased a resonance computation, not just the Signal.

A 2026-05-19 grill session walked the design tree and pinned every
branch. An independent review by a second model (Codex) surfaced six
findings, all of which the design absorbed — including reversing the
initial "persistent floor" mechanic in favor of a decaying boost.

The design lives in detail in `docs/CONTEXT.md`:

- `#decision`
- `#per-project-stance`
- `#chosen-decaying-boost-not-a-floor`
- `#dampened-x03-with-goal-amp-suppressed`
- `#lifecycle` (within `#decision`)

This ADR captures the *reasoning* — the alternatives weighed and why
this shape, not another.

## Decision

Decisions are first-class **Events** in the log:

```
DecisionRecorded {
    chose: ProjectId,
    over: Vec<ProjectId>,            // narrative rivals; NO mechanical effect
    dampen: Vec<ProjectId>,          // explicit mechanical suppression set
    reason: Option<String>,
    decided_at: Option<DateTime<Utc>>,
}
DecisionRevoked { decision_id: EventId }
```

Scope: **Projects only in v1.** Goals already carry status; Signals
are facts.

### Per-project stance, derived by replay

The runtime maintains, per Project, a stance:

- `Chosen { decision_id, boost_remaining }`
- `Dampened { decision_id }`
- absent

"Most-recent Decision wins per project" — by **replay order**, not
`decided_at`, not `EventId`. A single Decision can be partially
superseded.

### Chosen: decaying boost (not a floor)

On `DecisionRecorded` with `chose: P`:

- Initial boost: **+0.15** on P's `strategic_relevance`.
- Boost decays at **≈ 0.999 / event-log day** (slower than normal
  project decay ≈ 0.99 / day), toward zero.
- Displayed relevance = `raw_relevance + boost_remaining`, clamped to
  `[0.0, 1.0]`.

### Dampened: ×0.3, goal amp suppressed

For projects in `dampen`:

- Resonance deltas scaled by **0.3**.
- Goal amplification **does not apply** (mutually exclusive with
  normal goal amp).
- Decay runs normally.

### Cause variants

Add structured variants to the `Cause` enum:

- `DecisionBoostApplied { decision_id, contribution: f32 }`
- `DecisionDampened { decision_id, factor: f32 }`

No stringly-typed explanation text; explanations remain queryable and
refactor-safe.

### Lifecycle

A Decision steers a project until:

1. **Superseded** — a later Decision names that project (per-project
   override, by replay order).
2. **Revoked** — explicit `DecisionRevoked`. `decision_id` must
   reference a prior `DecisionRecorded`; **ingest rejects unknown ids
   loudly**. **Replay silently ignores** an orphan revoke in a
   corrupted log — consistent with the existing replay tolerance for
   impossible updates in `apply_event`. Adding a structured diagnostic
   channel for corrupted-log conditions is a separate concern not
   gated by this ADR.

**No auto-expiry.** The decaying boost erodes naturally.

## Why this is right

### Boost respects entropy

The runtime's other fields are continuous functions of events.
Introducing a *floor* — a hard-clamped protected zone inside the
otherwise smooth simulation — was the initial pick; we walked it back.

A floor mechanically locks `strategic_relevance >= 0.75` while a
Decision is active. Without auto-expiry, a stale Decision keeps a dead
project visibly alive forever. The hidden-rot risk would have to be
patched with display-layer "stale" annotations.

A decaying boost preserves the model's shape. Signals are evidence of
real work; if they keep flowing, the project's raw relevance stays
elevated and the boost is gravy. If they don't, the boost erodes back
toward baseline — the system itself nudges the user to either reaffirm
(via real signals) or revoke (explicit). **A Decision without
follow-through fades; a Decision with follow-through stays hot.**

### Split `over` and `dampen`

Initial design fused narrative and mechanic in a single `over: Vec<ProjectId>`
field. Two failure modes pushed us to split:

- The verb "decide ... over ..." reads as narrative. Users wouldn't
  expect a silent ×0.3 mechanic.
- Revocation released the dampening but left "rejected" in the log.
  "History says rejected, simulation says not anymore" was incoherent.

`over` is now history-only. `dampen` is the explicit mechanical
opt-in. Either may be empty. The common case (named rival + suppress
it) is a two-flag invocation:
`liferuntime decide --chose tnt --over side-x --dampen side-x`.

### Dampening dominates over Goal amplification

Initial design multiplied `delta × goal_amp × dampening`. A
dampened project under a high-importance overlapping Goal would still
rise — softly, but visibly. User intent ("I dampened this") vs system
behavior ("but the Goal pulls") diverged.

Fix: **mutually exclusive**. If a project is dampened, goal
amplification is suppressed for it. Explicit user opt-in beats
implicit tag overlap.

### Per-project stance, not global active-Decisions set

A single Decision can be *partially* superseded: still steers TNT,
no longer steers Side-X. A binary "active" flag on each Decision
would have lied. The derived state is `project_id ->
Option<DecisionStance>`, computed by replay.

`decision list` surfaces this: each Decision is shown with the
projects it currently steers and remaining boost, so partial
supersession is visible.

## Trade-offs

- **Boost is harder to explain than a floor.** Display shows
  `raw + boost = visible_relevance`; two numbers instead of one.
  Mitigated by `decision list` showing remaining boost per
  Decision/project pair.
- **Stale Decisions are still possible.** A Decision can sit in the
  log forever with `boost_remaining ≈ 0`. They're harmless
  mechanically but clutter `decision list`. Acceptable; future
  command could prune zeroed entries from the default view.
- **Per-project state shape adds bookkeeping.** The derived `stance`
  field per Project is replay-rebuildable but real. Symmetric with
  existing per-project fields (relevance, urgency, etc.).
- **Constants are committed.** The choice of `+0.15` initial boost,
  `0.999 / day` boost decay, `0.3` dampening factor lives in
  `systems.rs`. Per grill #6, runtime tunables stay hardcoded in v1;
  these are flagged as policy (not physics) in comments.
- **No goal-conditioned boost magnitudes.** A decision that aligns
  with a high-importance Goal gets the same +0.15 as one that doesn't.
  Cleaner conceptually; arguably under-models the strategic richness.
  Revisitable when we have data.

## What ADR-0008 unlocks

- The Attention Prompt engine can query "is project P currently steered
  by a Decision?" before recommending action on P.
- `explain project P --all` (per CONTEXT.md `#explanation`) can cite
  Decisions by id in the causal chain.
- Future: `dampen` can grow into a `mute` variant (×0 instead of ×0.3)
  if real-world use shows the soft form is too soft.
- Future: the per-project-stance shape generalizes naturally to other
  "user-directed pull" mechanisms (e.g. a "monitor closely" stance).

## What ADR-0008 rules out

- **Goal-level or Signal-level Decisions** in v1. Adding `chose: Goal`
  or `chose: SignalInterpretation` was considered and rejected — Goals
  carry status, Signals are facts, and the chose-Project case covers
  the dominant use case.
- **Persistent floor.** Considered, initially picked, walked back. A
  Decision should not protect a project from entropy indefinitely.
- **Combined `over` + mechanical suppression.** Considered, initially
  picked, walked back. Narrative and mechanic must be separately
  expressible.
- **Auto-expiry / time-based fade of the Decision record.** Decisions
  remain in the log forever; only the boost erodes. Auto-expiry would
  break replay determinism.
- **Stacking boosts.** Two consecutive `chose: TNT` Decisions do not
  add their boosts; the later replaces the earlier.

## Consequences

- New events in `WorldEvent` (`crates/world/src/events.rs`):
  `DecisionRecorded`, `DecisionRevoked`.
- New variants on `Cause` (`crates/world/src/explanation.rs`):
  `DecisionBoostApplied`, `DecisionDampened`.
- New per-project derived field tracking the active Decision stance,
  rebuilt by replay (`crates/world/src/model.rs` /
  `crates/world/src/runtime.rs`).
- New system in the schedule (`crates/world/src/systems.rs`):
  decision-application, running on the per-event schedule from
  ADR-0006. Decay system updated to take boost into account.
- Goal-amplification logic in the resonance system updated to skip
  dampened projects.
- New CLI commands (`apps/cli/src/main.rs`): `decide`,
  `decision list`, `decision revoke`.
- Storage compat (ADR-0005): new event variants are additive — no
  schema migration. Old logs without Decision events replay
  unchanged.
- Determinism: `most-recent wins per project` uses **replay order**,
  not timestamps or event ids. The event store
  (`crates/event-log/src/store.rs`) preserves insertion order; this
  ADR depends on that guarantee.

## Cross-references

- ADR-0003 — Event log is inputs-only. Decisions are user inputs;
  the boost and dampening states are derived, not stored.
- ADR-0005 — Storage roadmap. New event variants additive under the
  Option-B compat contract.
- ADR-0006 — Per-event scheduling. The decision-application system
  runs per event, like matching and decay.
- `docs/CONTEXT.md` — `#decision`, `#per-project-stance`,
  `#chosen-decaying-boost-not-a-floor`,
  `#dampened-x03-with-goal-amp-suppressed`.
- Independent review (Codex, 2026-05-19): findings absorbed —
  per-project stance shape, structured Cause variants, mutually
  exclusive dampen+goal-amp, decaying boost over floor, split
  over/dampen, validated revocation.

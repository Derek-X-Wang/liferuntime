# ADR-0001: CLI-first local runtime

- Status: Accepted
- Date: 2026-05-16

## Context

LifeRuntime is a persistent world simulation, not a productivity app, not
a chatbot, and not a SaaS. The v1 question is not "what does the user
type into a text box" — it is "does the core primitive (Event in → World
changes → Explanation out) actually hold up?"

We need to prove that primitive before we choose a delivery surface.

## Decision

Build a Rust workspace with three crates and one CLI binary:

- `liferuntime-world` — the deterministic simulation core (`WorldRuntime`).
- `liferuntime-event-log` — append-only event storage with two adapters:
  in-memory (tests) and JSONL (local-first production).
- `liferuntime-agent-bridge` — the seam for probabilistic AI providers,
  with one stub adapter (`FakeAgent`).
- `apps/cli` — a `clap`-based CLI: `init`, `project add`, `goal add`,
  `signal add`, `advance`, `inspect`, `explain`, `replay`.

Persist events in `.liferuntime/events.jsonl` next to the user's data,
not in a cloud database. Persist the advance cursor in `cursor.json` and
the rendered output of the last advance in `last_advance.json` so the
CLI can answer `explain latest` without re-running systems.

## Why not SQLite for v1

SQLite is the obvious choice for local persistence, and we will likely
migrate to it. We do not adopt it now because:

- Adding it would introduce a real seam (two adapters) before we have
  proof the simulation primitive itself works.
- JSONL is grep-able and diffable, which is enormously useful while the
  domain model is still mutating.
- The `EventLog` trait is the seam; the SQLite adapter can land later
  without touching `WorldRuntime`.

The cost: large event histories will be slower to load. Acceptable until
we have evidence we need to optimize.

## Why CLI before GUI/TUI/HTTP

- Agents (including AIs writing code in this repo) are far better at
  driving a CLI than a GUI.
- A CLI forces us to design the interface before we design the chrome.
- Once the CLI is right, the GUI/TUI/HTTP/MCP surfaces are mostly
  adapters over the same `WorldRuntime` methods.

## Why a single binary

Not multi-binary (`liferuntime-server`, `liferuntime-client`) and not a
plugin system. Both would be premature seams. We have one user, one
runtime, one machine. If a second deployment shape arrives, we add a
second app under `apps/`.

## Consequences

- Easy to ship the first vertical slice in one PR.
- Easy to delete: this ADR documents what to revisit when v2 demands
  long-lived processes, network surfaces, or third-party agents.
- AI providers and factual-world feeds are not blockers; the
  `AgentBridge` trait reserves space without forcing implementation.

## Status of related future decisions

- Snapshot/projection layer: deferred. Replay is fast enough today.
- Factual World feed: deferred. The `WorldEvent::SignalObserved` shape is
  forward-compatible with externally-sourced signals.
- Multiple Personal Worlds: deferred. The `--dir` flag already supports
  swapping the entire World.

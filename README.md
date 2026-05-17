# LifeRuntime

> A persistent world simulation runtime where events enter the world,
> systems update state, and you can inspect why the world changed.

LifeRuntime is not a notes app, not a task tracker, not a chatbot, and
not an "AI life coach". It is a **deterministic runtime** that models
your projects, goals, and signals as an evolving simulation — so you
can see what's pulling on your attention, why it changed, and where
it's heading.

Built in Rust on top of `bevy_ecs`. Local-first. CLI-driven. Designed
for use by humans *and* AI agents.

```text
Event in  →  World changes  →  Explanation out
```

---

## Why it exists

Most software models life as static artifacts: notes, tasks, calendars,
chats, dashboards. Real life is dynamic — events happen, priorities
shift, projects gain or lose momentum, external news affects personal
strategy.

LifeRuntime preserves **continuity of cognition**: long-term strategic
reasoning that survives across days, weeks, and months. The runtime
itself is the persistent state; AI lives outside it, proposing events
the user (or the runtime) decides whether to ingest.

The deepest design rule: **deterministic core, probabilistic outer
layer.** The world is durable, replayable, and testable. AI assists;
AI does not own reality.

---

## Quick start

```bash
# Build
cargo build --release

# Initialize a fresh runtime directory
liferuntime --dir .liferuntime init

# Add a project, a goal, a signal
liferuntime --dir .liferuntime project add tnt \
  --name "TNT" --tags ai,voice,agent

liferuntime --dir .liferuntime goal add voice-agent \
  --name "Ship voice-first agent" --tags ai,voice --importance 0.9

liferuntime --dir .liferuntime signal add \
  --source manual \
  --summary "Realtime voice models are improving quickly" \
  --tags ai,voice,realtime \
  --confidence 0.8

# See what changed
liferuntime --dir .liferuntime advance

# See what's pulling on attention
liferuntime --dir .liferuntime status

# Inspect a single project
liferuntime --dir .liferuntime inspect project tnt

# Replay to prove determinism
liferuntime --dir .liferuntime replay
```

Sample `status` output:

```text
Active projects (1, trajectory over last 5 advance(s)):
  ↑ TNT                       relevance 0.78  urgency 0.62  slope +0.234 (warming)
```

---

## CLI surface

| Command | What it does |
|---|---|
| `init` | Create a fresh runtime directory. |
| `project add / edit / archive / complete / reactivate` | Project lifecycle. |
| `goal add / edit / achieve / abandon / reactivate` | Goal lifecycle (value-charged verbs). |
| `signal add` | Ingest a signal directly. `--idempotency-key` is cron-safe. |
| `signal analyze` | Run the `FakeAgent` over text; print proposed signals. `--commit` ingests them. |
| `pulse [--at TIMESTAMP]` | Advance event-log time during quiet stretches (vacation, no signals). |
| `advance` | Report changes triggered by events past the cursor. |
| `status [--window N]` | Show active projects with ↑/↓/→ trajectory. |
| `inspect project ID` | Print one project's current state. |
| `explain latest` | Re-print the most recent advance's explanation. |
| `replay` | Count events in the log (proof of replay). |

---

## Architecture

```
liferuntime/
├── apps/cli/                  # The CLI binary
├── crates/
│   ├── world/                 # WorldRuntime — the deterministic core
│   ├── event-log/             # EventLog trait + Memory / Jsonl adapters
│   └── agent-bridge/          # AgentBridge seam + FakeAgent stub
├── docs/
│   ├── CONTEXT.md             # Domain vocabulary (the glossary)
│   ├── LANGUAGE.md            # Architecture vocabulary (deep modules)
│   ├── CONTINUITY.md          # What files preserve your world; backup story
│   ├── adr/                   # Architecture Decision Records
│   └── agents/                # Per-skill conventions (Matt Pocock skill seeds)
└── examples/
```

Three deep modules with strong locality:

1. **`WorldRuntime`** — single high-leverage interface (`ingest`,
   `advance`, `inspect`, `explain`, `trajectories`). Bevy ECS lives
   inside; callers never see it.
2. **`EventLog`** — the seam between in-memory tests
   (`MemoryEventLog`) and local-first production (`JsonlEventLog`).
   Real seam; two adapters.
3. **`AgentBridge`** — the seam between the deterministic core and
   probabilistic AI providers. One stub (`FakeAgent`) ships today;
   AI proposes events, runtime decides whether to ingest. See ADR-0001.

---

## Key principles

| | |
|---|---|
| local-first | over cloud-first |
| runtime | over app |
| event log | over mutable hidden state |
| explanation | over black-box recommendation |
| replayability | over convenience |
| concrete ontology | over premature generic schema |
| deep modules | over many shallow modules |
| deterministic core | over AI-driven mutation |
| vertical slice | over platform architecture |

See `docs/LANGUAGE.md` for the architecture vocabulary
(module, interface, depth, seam, adapter, locality, deletion test).

---

## Documentation

- [`docs/CONTEXT.md`](docs/CONTEXT.md) — **the** glossary. Every public
  name in the codebase maps to a term here.
- [`docs/LANGUAGE.md`](docs/LANGUAGE.md) — architecture vocabulary.
- [`docs/CONTINUITY.md`](docs/CONTINUITY.md) — what to back up; how
  recovery works; the compatibility contract.
- [`docs/adr/`](docs/adr/) — Architecture Decision Records:
  - [ADR-0001](docs/adr/0001-cli-first-local-runtime.md) — CLI-first
    local runtime
  - [ADR-0002](docs/adr/0002-advance-is-a-reporter-not-a-trigger.md) —
    Advance is a reporter, not a trigger
  - [ADR-0003](docs/adr/0003-event-log-is-inputs-only.md) — Event log
    is inputs-only
  - [ADR-0004](docs/adr/0004-event-log-time-not-wall-clock.md) —
    Time-dependent logic uses event-log time
  - [ADR-0005](docs/adr/0005-storage-roadmap-and-compat-contract.md) —
    Storage roadmap (JSONL → SQLite) + compat contract
  - [ADR-0006](docs/adr/0006-per-event-scheduling.md) — Per-event
    scheduling (vs batch advance)
  - [ADR-0007](docs/adr/0007-dir-level-flock-for-concurrent-cli.md) —
    Dir-level flock for concurrent CLI access

---

## Status

V1 vertical slice is complete. The runtime proves the core primitive
end-to-end: `Event in → World changes → Explanation out`.

What works today:
- Projects, Goals, Signals with full lifecycle (create / edit /
  archive / complete / reactivate; goal achieve / abandon).
- Signal-driven Resonance bumps with Goal amplification.
- Time-based Decay using event-log time (replay-safe).
- TimePulse events for cron-friendly time advancement.
- Cursor-based delta reporting via `advance`.
- Attention prompts (`status`) showing ↑ warming / ↓ cooling.
- Idempotency keys for cron-safe retries.
- Dir-level flock for concurrent CLI safety.
- 17 tests through the `WorldRuntime` interface.

What's deferred (with explicit triggers):
- SQLite event log (v2; cutover when replay > 250ms or queries need
  indexes — see ADR-0005).
- Real LLM provider adapters (when a second adapter joins `FakeAgent`).
- Snapshot mechanism (with the SQLite cutover).
- Daemon mode (post-CLI surface).

---

## License

MIT OR Apache-2.0 at your option.

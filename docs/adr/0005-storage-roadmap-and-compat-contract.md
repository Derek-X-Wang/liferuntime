# ADR-0005: Storage roadmap and event log compatibility contract

- Status: Accepted
- Date: 2026-05-16
- Supersedes the SQLite-deferral guidance in ADR-0001 with a concrete
  trigger and migration plan.

## Context

`events.jsonl` is the canonical store of the user's personal world (see
ADR-0001, ADR-0003). As the world grows, we will hit limits of a
text-file event log:

- Replay-from-disk time grows linearly with log size.
- No indexed queries — "show me all signals from May tagged voice"
  requires scanning every line.
- No transactional writes — partial writes risk corruption.
- No schema enforcement.

The kickoff doc identified SQLite as the long-term default. ADR-0001
deferred it on the grounds that v1 needed to prove the core primitive
first. That deferral was right; we now need a plan for the migration
and a compatibility contract that covers both formats.

## Decision

Three intertwined commitments.

### 1. Storage roadmap

**v1.x — JSONL.** `events.jsonl` remains the production store. Grep-able,
diffable, easy to inspect by hand. EventLog trait already abstracts the
backend (real seam: `MemoryEventLog` for tests, `JsonlEventLog` for
production).

**v2.x — SQLite.** A `SqliteEventLog` adapter lands when *any* of these
triggers fires:

- Cold-start replay exceeds ~250 ms on a realistic log.
- A use case demands indexed queries (e.g. `history_for_entity` for
  status / trajectory, time-range queries for snapshots).
- Concurrent multi-process access becomes useful (a daemon that
  receives webhooks alongside the CLI).

The EventLog trait stays the public seam. Switching backends is a
config change for users, not an API change.

### 2. Compatibility contract: option B (loose + migration tool)

We do **not** commit to forever forward-compatibility on individual
event variants. Breaking changes are allowed — but every breaking
change ships with a migration the user runs once.

Concretely:
- Adding new variants → safe; old code can't read new logs, but new
  code reads old logs fine.
- Renaming variants / fields → allowed, with a migration.
- Removing variants → allowed, with a migration.

The `liferuntime migrate` command checks the log's schema version
against the binary's expected version and runs the gap.

### 3. Migration tooling: SQL ecosystem, not hand-rolled Rust

Once we land SQLite, migrations use a mature SQL migration tool
(candidates: `sqlx-cli`, `refinery`, `diesel migrations` — pick when
we land it). Reasons:

- Battle-tested by thousands of production databases.
- Up/down migrations, dry-runs, schema-version tracking are solved.
- The tool's UX is familiar to anyone who has worked with SQL.
- Avoids writing a parallel "Rust migration framework" for one project.

For the JSONL → SQLite cutover itself, a one-time ingest script reads
all events.jsonl, validates each row against the current `WorldEvent`
schema, and inserts into the SQLite schema. After cutover, future
migrations are pure SQL DDL.

JSONL-era migrations (between v0.1 and the SQLite cutover) are
hand-rolled in Rust, but the surface is tiny — v1 has 5–10 event
variants, and we will not accumulate many JSONL-era migrations before
we cut to SQLite.

## Why option B over A (hard forever-compat)

A locks every name we ship today, forever. With only 5 variants we
*could* afford that — but the storage roadmap above (SQLite cutover in
v2.x) gives us a natural break point for cleanup. SQL migrations exist
specifically to handle this kind of structural evolution; relying on
them is cheaper than living with regretted names.

The price of B over A: the user must run `liferuntime migrate` once per
breaking change. Acceptable because:
- Breaks are rare and well-marked in CHANGELOG.
- The CLI can fail loudly with "schema version X expected, log is Y;
  run `liferuntime migrate`".
- Continuity of cognition is preserved (the *data* survives the
  migration, even if internal names change).

## Why not option C (no contract pre-1.0)

The project's core promise is multi-month continuity. If `~/.liferuntime`
dies on every upgrade, the promise dies with it.

## Consequences

- The EventLog trait stays the public seam. v2's SQLite adapter is a
  drop-in.
- Every WorldEvent variant gets a stable serde tag we don't change
  casually. If we must change one, we ship migration + bump the schema
  version recorded somewhere in the log (TBD: header line, sidecar
  file, or first event in an empty log).
- `liferuntime migrate` is reserved as a CLI command for future
  releases; we don't ship it today because nothing needs migrating
  yet.
- Tests should cover "loading a v0.1 log into a v0.2 binary after
  migration" as a regression suite once we ship a real migration.

## Schema version field

A metadata header line at the top of `events.jsonl`:

```jsonl
{"_meta":"liferuntime-schema","version":1}
{"id":"01K...","occurred_at":"...","payload":{"kind":"ProjectCreated",...}}
{"id":"01K...","occurred_at":"...","payload":{"kind":"SignalObserved",...}}
```

JsonlEventLog detects the header on read and skips it. New logs get the
header written on init. Old logs without the header are implicitly
version 1; the first migration that bumps the version retroactively
prepends the header during its run.

Header lives with the data (`cp events.jsonl /backup/` keeps the
version intact) and is forward-compatible with the SQLite cutover (the
sqlite schema stores `version` in a metadata table; semantically
equivalent).

## Open questions

- **Migration testing.** A migration suite needs a corpus of frozen
  example logs from each prior version. Build the corpus lazily as the
  first migration lands.
- **SQLite schema design.** When we ship the SqliteEventLog, decide
  whether events are stored as `(id, occurred_at, payload_json)` rows or
  fully normalized into per-variant tables. Lean toward JSON payload
  for forward-compat with new variants without schema changes.

## Cross-references

- ADR-0001 — CLI-first local runtime (deferred SQLite).
- ADR-0003 — Event log is inputs-only (the schema's stability matters
  more if the log is the canonical input source).
- `crates/event-log/src/lib.rs` — EventLog trait + adapters.

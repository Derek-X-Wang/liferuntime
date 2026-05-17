# ADR-0007: Dir-level flock for concurrent CLI access

- Status: Accepted
- Date: 2026-05-17

## Context

Each `liferuntime` CLI invocation opens the runtime directory, reads
`events.jsonl` + `cursor.json` into an in-memory world, optionally
mutates (`ingest`, `advance`), and exits. Two CLI processes hitting the
same dir concurrently is a real scenario:

- User has a daily cron running `liferuntime pulse`.
- User is also typing `liferuntime signal add` in a terminal.
- Some setup script chains commands with `&&`.

Without coordination:

- `events.jsonl` writes are safe — POSIX `O_APPEND` is atomic for small
  writes (well under PIPE_BUF for our line lengths).
- `cursor.json` writes race. Two processes both read the same cursor,
  both compute deltas, both write back the cursor. One process's events
  end up re-reported as "new" on the next `advance`.
- `advances.jsonl` interleaves cleanly (atomic appends) but ordering
  may swap.
- In-memory caches diverge: each process loaded the log before the
  other appended.

None of these corrupt data (the event log is canonical and append-only)
but the user sees confused output.

## Decision

Acquire an exclusive `flock(2)` on `.liferuntime/lock` at the start of
`open_dir`. The lock is held for the lifetime of the `WorldRuntime`
struct and released when the runtime drops (file close releases the
OS lock).

Concurrent CLI processes therefore serialize at `open_dir` — the second
process blocks until the first exits. Single-CLI workflows are
unaffected.

Implementation uses `std::fs::File::lock()`, stabilized in Rust 1.89.
No external dependency.

## Why exclusive (not shared) for every command

Read-only commands (`inspect`, `explain`, `replay`) don't mutate cursor
or events. In theory they could take a shared lock and run alongside
each other. v1 takes an exclusive lock for everything because:

1. Even reads run the schedule (per-event scheduling, ADR-0006), which
   reads-from + writes-to the in-memory ChangeLog. With concurrent
   processes sharing the same `events.jsonl`, a read after a concurrent
   write would still need re-replay; the lock guarantees ordering.
2. The blast radius of "two reads at once" is small (no contention in
   typical CLI use); the code complexity of shared-vs-exclusive lock
   modes isn't justified.
3. If concurrent read latency becomes a problem, downgrading to a
   shared-mode read lock is a straightforward future change.

## Why not document-only

The cron + CLI scenario is explicitly designed-for: ADR-0004 introduced
`TimePulseObserved` precisely so users could nudge time forward via
cron. "Document one CLI at a time" undermines that intention.

## Why not lock-free with CAS on cursor.json

Lock-free is appealing but only protects the cursor race. The
in-memory cache divergence remains. flock protects everything in one
mechanism with one line of code per command.

## Consequences

- `WorldRuntime` holds a `File` in `_lock: Option<File>`. Released
  automatically on drop.
- `in_memory()` runtimes don't take a lock (no shared resource).
- Lock file at `.liferuntime/lock` is created if absent. Ephemeral;
  excluded from git via the `.liferuntime/` blanket in `.gitignore`.
- On filesystems that don't support `flock` (some network mounts,
  exotic setups), `File::lock()` returns an error which propagates as
  `RuntimeError::Io`. The CLI prints a clear message; the user can
  unset the lock-file path or move the dir to a local filesystem.
- Future daemon mode (deferred per ADR-0001) inherits the locking
  semantics for free.

## What this rules out

- "Multiple CLI clients reading without blocking" — would require
  shared-mode locks (deferable; not blocking v1).
- "Pluggable lock backends" (network locks, distributed locks) — out
  of scope; local-first runtime.

## Cross-references

- ADR-0001 — CLI-first local runtime (lock is a CLI-process boundary).
- ADR-0002 — Advance is reporter (cursor consistency matters).
- ADR-0004 — Event-log time (cron use cases motivate the lock).
- `crates/world/src/runtime.rs` — `open_dir`, `_lock` field.

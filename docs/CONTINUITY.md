# Continuity Contract

The promise of LifeRuntime is **continuity of cognition across months**.
This document is the explicit contract: what files preserve your world,
what you can lose without losing the world, how recovery works, and
what we commit to about upgrades.

## What lives in `.liferuntime/`

After running any CLI command, your runtime directory contains:

```
.liferuntime/
├── events.jsonl       ← THE WORLD. Canonical. Append-only.
├── lock               ← Ephemeral OS flock; safe to delete when no CLI is running.
├── cursor.json        ← "What you've already advanced through." Derivable.
├── advances.jsonl     ← Per-advance change records. Derivable.
└── last_advance.json  ← Pretty-printed cache of the most recent advance. Derivable.
```

### `events.jsonl` — THE world

Everything else is reconstructable from this one file. Lose any other
file, run any command, and the missing file regenerates. Lose
`events.jsonl` and your world is gone.

It is one JSON object per line:

```jsonl
{"_meta":"liferuntime-schema","version":1}
{"id":"01K...","occurred_at":"2026-05-17T10:00:00Z","payload":{"kind":"ProjectCreated","id":"tnt","name":"TNT","tags":["ai","voice"]}}
{"id":"01K...","occurred_at":"2026-05-17T10:01:00Z","payload":{"kind":"SignalObserved","source":"manual","summary":"…","tags":["ai","voice"],"confidence":0.8,"observed_at":null}}
```

- **Header line** records the schema version (ADR-0005, ADR-0007).
- **Each subsequent line** is a `StoredEvent` envelope (`id`,
  `occurred_at`, optional `idempotency_key`) wrapping a `WorldEvent`
  payload.
- Grep-friendly. Diff-friendly. Safe to read by hand.

### Files that aren't `events.jsonl`

| File | What it holds | What happens if you delete it |
|---|---|---|
| `lock` | OS-level flock during a running CLI command | Created on next run. |
| `cursor.json` | Id of the last Event past which `advance` has already reported | Next `advance` re-reports the whole derivation log as "new". State unchanged. |
| `advances.jsonl` | Per-advance change records (foundation for Trajectory) | `liferuntime status` shows quiet trajectories for older projects until new advances accumulate. |
| `last_advance.json` | Pretty-printed cache of the most recent `advance` output | `explain latest` prints "no advance recorded" until next `advance`. |

All four are derivable. The contract is: you can rebuild them by
replaying `events.jsonl`. Three are fully derived; `advances.jsonl` is
historical metadata that doesn't fully regenerate (past advances are
gone), but the *world state* is identical.

## Backup

**Backup `events.jsonl`. Everything else is gravy.**

```bash
# Daily backup
cp .liferuntime/events.jsonl ~/backups/events-$(date +%Y%m%d).jsonl

# Or with the rest (if you want to preserve past explanations)
cp -r .liferuntime ~/backups/liferuntime-$(date +%Y%m%d)/
```

Add `.liferuntime/` to `.gitignore` if you don't want to version it.
Or version it deliberately — `events.jsonl` is human-readable and the
diffs tell the story of your strategic shifts.

## Recovery

Disk dies. Laptop swap. Restore from backup:

```bash
# Drop the events.jsonl back into a fresh dir
mkdir -p ~/liferuntime
cp ~/backups/events-latest.jsonl ~/liferuntime/events.jsonl

# Run any command. Derived files regenerate. State matches.
liferuntime --dir ~/liferuntime status
```

`liferuntime replay` reports the event count, so you can sanity-check
the restored log matches what you expected.

## Determinism

> Same `events.jsonl` → same world.

This holds across:
- Re-runs on the same machine.
- Re-runs after a restore from backup.
- Re-runs after a binary upgrade (within the same schema version).
- Re-runs after the matching algorithm changes — *new* algorithm
  applied to *old* events produces a self-consistent world, which is a
  feature (your historical signals get re-evaluated under your current
  thinking).

Time-dependent systems read **event-log time** (the timestamp of the
most recent Event in the log), not wall-clock time (ADR-0004). So
running `replay` tomorrow gives byte-for-byte the same derived state
as running it today.

The one place wall-clock leaks in: `occurred_at` on a freshly-ingested
event is `Utc::now()` at ingest time. Once written to the log, that
timestamp is the authoritative time forever.

## Upgrades

We commit to **option B compatibility**: most upgrades just work;
breaking changes ship with a `liferuntime migrate` invocation (ADR-0005).

Concretely:
- **Adding event variants** — always safe. New binary reads old logs.
- **Renaming fields / variants** — allowed; migration tool ships
  alongside.
- **Removing variants** — allowed; migration tool rewrites old events.

Old binaries cannot read new logs. If you upgrade and your log was
written by a newer binary somehow, you get a loud error:

```text
event log schema version 2 is newer than this binary's expected
version 1 — upgrade liferuntime or run `liferuntime migrate`
```

## Privacy

LifeRuntime is **local-first**. Your world lives in `.liferuntime/` on
your machine. The runtime does not phone home, does not telemeter,
does not sync, does not call out to any service.

`liferuntime signal analyze` uses the in-process `FakeAgent` stub by
default. When real LLM provider adapters land, they will be opt-in;
choosing one will make outbound network calls explicit (the adapter's
config + the command flag together).

## Multi-machine

The runtime is "one dir per machine per world" by default. To move
between machines, copy `events.jsonl` (or the whole dir for full
fidelity). No central server, no auth, no sync conflicts to resolve.

If you want one world synced across machines, use whatever sync layer
you trust — Syncthing, iCloud, Dropbox, Git. The runtime treats the dir
as the source of truth; the sync layer's job is just to keep two
copies of the dir in agreement.

Caution: **don't run two CLI processes against the same dir at the
same time**. The dir-level flock (ADR-0007) prevents corruption *on the
same machine*; cross-machine simultaneous writes would race the sync
layer, not the runtime. Stagger your machines, or pick one as primary.

## What this contract does *not* promise

- We don't promise the *derived state* is byte-stable across binary
  versions. The matching algorithm may improve, and an old log
  re-derived under the new algorithm will produce slightly different
  ChangeRecords (with the same explanatory power, hopefully better).
- We don't promise infinite log growth is performance-free. ADR-0005
  flags the SQLite cutover; past ~10k events the JSONL adapter starts
  to feel slow on cold start.
- We don't promise zero behavior change in major version bumps. The
  CONTEXT.md vocabulary is stable; the implementation details aren't.

What we *do* promise: you will never silently lose data, and you will
never lose the ability to inspect your event log by hand.

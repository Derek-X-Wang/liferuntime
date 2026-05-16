# Domain Docs

How the engineering skills should consume this repo's domain
documentation when exploring the codebase.

## Layout

Single-context repo. Domain docs live under `docs/`, not at the repo
root.

```
/
├── docs/
│   ├── CONTEXT.md          ← domain vocabulary (the glossary)
│   ├── LANGUAGE.md         ← architecture vocabulary (modules, seams, …)
│   └── adr/
│       └── 0001-cli-first-local-runtime.md
├── crates/
└── apps/
```

> **Path note for skills:** `CONTEXT.md` is at `docs/CONTEXT.md`, not at
> the repo root. Several skills (`improve-codebase-architecture`,
> `diagnose`, `tdd`, `grill-with-docs`) assume a root-level
> `CONTEXT.md` by default — read `docs/CONTEXT.md` instead in this
> repo.

## Before exploring, read these

- **`docs/CONTEXT.md`** — the project's domain vocabulary (World,
  Event, Signal, Project, Goal, Cause, etc.). Use these terms; do not
  drift to synonyms the glossary explicitly avoids.
- **`docs/LANGUAGE.md`** — the architecture vocabulary (Module,
  Interface, Depth, Seam, Adapter, Leverage, Locality, deletion test).
  Use these words when arguing about codebase shape.
- **`docs/adr/`** — read ADRs that touch the area you're about to work
  in.

If any of these files don't exist in a future branch, **proceed
silently**. Don't flag their absence; don't suggest creating them
upfront. The producer skill (`/grill-with-docs`) creates them lazily
when terms or decisions actually get resolved.

## Use the glossary's vocabulary

When your output names a domain concept (in an issue title, a refactor
proposal, a hypothesis, a test name), use the term as defined in
`docs/CONTEXT.md`. Don't drift to synonyms the glossary explicitly
avoids.

If the concept you need isn't in the glossary yet, that's a signal —
either you're inventing language the project doesn't use (reconsider)
or there's a real gap (note it for `/grill-with-docs`).

## Flag ADR conflicts

If your output contradicts an existing ADR, surface it explicitly
rather than silently overriding:

> _Contradicts ADR-0001 (CLI-first local runtime) — but worth reopening
> because…_

## Why this repo uses `docs/CONTEXT.md` instead of root

The `docs/` directory was established before the agent-skills
convention was set up, and we chose to keep narrative documentation
co-located (CONTEXT, LANGUAGE, ADRs all in one place). Moving
`CONTEXT.md` to the root would split the docs and create a discovery
question every time someone reads the tree. Skills override the
default location via this file instead.

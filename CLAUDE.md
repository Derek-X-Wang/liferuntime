# LifeRuntime

Persistent world simulation runtime. The deterministic core lives in
`crates/world` (`WorldRuntime`); the event log seam in
`crates/event-log`; the AI boundary in `crates/agent-bridge`; the CLI in
`apps/cli`.

For domain language, see `docs/CONTEXT.md`. For architecture vocabulary
(modules, seams, depth, leverage), see `docs/LANGUAGE.md`. For past
architectural decisions, see `docs/adr/`.

## Agent skills

### Issue tracker

GitHub Issues via the `gh` CLI. (No git remote is configured yet — push
this repo to GitHub before invoking issue skills like `triage`,
`to-issues`, `to-prd`, or `qa`.) See `docs/agents/issue-tracker.md`.

### Triage labels

Canonical defaults: `needs-triage`, `needs-info`, `ready-for-agent`,
`ready-for-human`, `wontfix`. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context repo. `CONTEXT.md` lives at `docs/CONTEXT.md` (not the
repo root). ADRs live at `docs/adr/`. See `docs/agents/domain.md` for
the consumer rules skills should follow.

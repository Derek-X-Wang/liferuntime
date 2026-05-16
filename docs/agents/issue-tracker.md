# Issue tracker: GitHub

Issues and PRDs for this repo live as GitHub issues. Use the `gh` CLI
for all operations.

> **Note:** as of this writing, no git remote is configured. Push the
> repo to GitHub (`git remote add origin … && git push -u origin main`)
> before invoking any issue-tracker skill. Until then, `gh` commands
> will fail with "no GitHub repo detected".

## Conventions

- **Create an issue**: `gh issue create --title "..." --body "..."`. Use
  a heredoc for multi-line bodies.
- **Read an issue**: `gh issue view <number> --comments`, filtering
  comments with `jq` and also fetching labels.
- **List issues**:
  `gh issue list --state open --json number,title,body,labels,comments --jq '[.[] | {number, title, body, labels: [.labels[].name], comments: [.comments[].body]}]'`
  with appropriate `--label` and `--state` filters.
- **Comment on an issue**: `gh issue comment <number> --body "..."`
- **Apply / remove labels**: `gh issue edit <number> --add-label "..."`
  / `--remove-label "..."`
- **Close**: `gh issue close <number> --comment "..."`

`gh` infers the repo from `git remote -v` automatically when run inside
a clone.

## When a skill says "publish to the issue tracker"

Create a GitHub issue.

## When a skill says "fetch the relevant ticket"

Run `gh issue view <number> --comments`.

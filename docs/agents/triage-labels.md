# Triage Labels

The skills speak in terms of five canonical triage roles. This file
maps those roles to the actual label strings used in this repo's issue
tracker.

| Role (in skill prose) | Label in this repo  | Meaning                                  |
| --------------------- | ------------------- | ---------------------------------------- |
| `needs-triage`        | `needs-triage`      | Maintainer needs to evaluate this issue  |
| `needs-info`          | `needs-info`        | Waiting on reporter for more information |
| `ready-for-agent`     | `ready-for-agent`   | Fully specified, ready for an AFK agent  |
| `ready-for-human`     | `ready-for-human`   | Requires human implementation            |
| `wontfix`             | `wontfix`           | Will not be actioned                     |

When a skill mentions a role (e.g. "apply the AFK-ready triage label"),
use the corresponding label string from the right-hand column.

These labels do not yet exist in any GitHub repo — they will be created
on first use, or you can create them up front with:

```sh
gh label create needs-triage    --color "fbca04" --description "Maintainer needs to evaluate"
gh label create needs-info      --color "d4c5f9" --description "Waiting on reporter"
gh label create ready-for-agent --color "0e8a16" --description "Fully specified, AFK-ready"
gh label create ready-for-human --color "1d76db" --description "Needs human implementation"
gh label create wontfix         --color "ffffff" --description "Will not be actioned"
```

Edit the right-hand column above if you adopt different vocabulary
later.

---
name: closeout
description: Close one explicitly identified CLI target and then remove its explicitly eligible linked worktree. Use when external workflow evidence has already proved a worktree safe to remove.
kind: operation
domain: cli
route: infer
profile: core
applicability: all projects
requires: python, git
provenance: house
---

# Close and remove an explicit linked worktree

`cli:closeout` accepts the same explicit target as `cli:close`, plus exact absolute worktree, branch,
HEAD, remote base, and external eligibility JSON. Eligibility binds that exact target and base OID and
contains positive `merged_to_remote_base` and `no_live_other_owner` observations, an observation time
no more than five minutes old, and non-empty provenance source and evidence ID. Run it from a helper
outside that worktree:

```text
python -B <skill-directory>\scripts\closeout.py run --target target.json --worktree C:\linked \
  --expected-head <sha> --expected-branch Feature/Thing --remote-base origin/main \
  --eligibility eligibility.json --receipt receipt.json
```

It refuses a primary, locked, dirty, untracked, mismatched, unpushed, or unmerged checkout. Eligibility
is positive external evidence for both merged reachability and no live other owner; its absence never
proves safety. After exact tab/frontend closure, it invokes only `git worktree remove <that-worktree>`;
it never sweeps repositories, deletes branches, merges, or forces removal. The receipt distinguishes a
closure failure from a later removal failure so cleanup can be retried deliberately.

When a receipt says `closed-removal-failed`, retry only the removal after every preflight gate still
passes. It does not re-close the absent tab:

```text
python3 -B <skill-directory>/scripts/closeout.py retry-remove --receipt receipt.json --worktree /linked \
  --expected-head <sha> --expected-branch Feature/Thing --remote-base origin/main --eligibility eligibility.json
```

---
name: cd
description: Navigate the current task into a nested directory, repository or linked worktree while preserving its conversation and owner. Use when work moves to another directory, including after creating, cloning, locating or selecting it, or when releasing a directory for removal.
kind: utility
domain: cli
route: infer
profile: core
applicability: all projects
requires: current host directory and session interfaces when available
provenance: house
---

# Navigate the current task

Resolve the destination to an absolute path and read its applicable repository instructions before
working there. Continue the already-authorized task in the same conversation with the same owner.
Ordinary navigation into a nested folder or sibling linked worktree does not require fresh permission
or a user checkpoint.

Distinguish where an operation runs from the host's session directory and project association. A tool's
explicit working-directory argument, an absolute path, `git -C`, or a shell directory change can direct
an operation without changing that association. Keep subsequent operations explicit; a shell directory
change does not establish persistent state for later tool calls. Report a native session change only
after the host confirms the effective destination for this conversation.

```text
Current task: inspect the API, then fix its sibling linked worktree.
Nested destination: C:/work/app/src/api
  Read applicable instructions; run the inspection with that explicit working directory.
Sibling destination: C:/work/app.worktrees/Fix-Api
  Read that checkout's instructions; use it as the explicit working directory for edits and checks.
Keep the existing authorization, conversation and task owner throughout both moves.
Linux paths such as /work/app/src/api and /work/app.worktrees/Fix-Api follow the same procedure.
```

## Native session changes

Use a supported native directory-changing interface when it is exposed to the agent and can change
this conversation. Verify its current contract and result rather than assuming a slash command or
internal setting exists. A setting that takes effect on a later turn does not confirm a switch for the
current turn. Never send a host slash command through a shell or edit host databases or session files
to simulate a change.

Before an actual native switch, inspect this conversation's owned children through the supported host
inventory. Collect completed or failed results and preserve their useful evidence, then retire only
disposable completed or failed owned handles through supported native controls. Preserve active useful
work and checkpoint its task, directory and pending results before switching. Leave unknown or
unrelated handles and shared backend processes alone. Exclude the inventory/preflight process itself
from child cleanup and await its completion before switching. An unavailable or ambiguous inventory
does not authorize blind cleanup.

When the native interface is absent, cannot address this conversation, or cannot safely complete the
switch, continue authorized operations using explicit working directories or absolute paths whenever
that is sufficient. Do not stop repository work or ask the user to enter a command and say `continue`
solely because its directory changed. Describe the remaining native association limitation only when
it affects the task. Do not invent a command, claim a retarget from a successful shell operation, start
a replacement conversation, or hand off ownership to evade a missing native capability.

If a subsequent step actually requires the host association to change, retain ownership and identify
that specific blocked step. Use an exposed supported interface if available; otherwise give an exact
user action only after verifying that this host supports it for the existing conversation. Continue
independent authorized work. Missing native control alone supplies no permission to close this session
or transfer context.

## Releasing a directory

Before moving, renaming, unregistering or removing a directory that owns the host session, verify that
the host has released its association with the old path. Explicit operation directories and successful
Git commands do not establish release. Keep the release-dependent step pending until native evidence
proves it; an empty residual directory or a removal error remains incomplete cleanup.

For a separately authorized ownership transfer needed to release the directory, make the successor the
sole owner of removal and final filesystem verification. Preserve the pending evidence in that
transfer. After verified transfer submission the original stops repository-scoped work and never
attempts the removal itself. The selected session lifecycle owner handles closing the predecessor;
`cli:close` supplies only exact-target closure mechanics. The successor starts from the destination
checkout and removes the old directory only after the predecessor no longer holds it. A pending manual
directory change leaves the current owner responsible and does not complete a transfer.

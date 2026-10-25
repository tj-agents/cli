---
name: close
description: Close one explicitly identified CLI frontend and terminal tab while retaining its worktree. Use when a caller supplies the exact process and terminal identities to close.
kind: utility
domain: cli
route: infer
profile: core
applicability: all projects
requires: python
provenance: house
---

# Close an explicit CLI target

`cli:close` is deterministic plumbing, not session interpretation. The caller supplies a target JSON
with a frontend PID plus creation time, a native terminal identity, and a terminal binding process plus
creation time. It never discovers a target from a model, title, command name, registry, or transcript.

```text
python -B <skill-directory>\scripts\close.py close --target target.json --receipt receipt.json  # Windows
python3 -B <skill-directory>/scripts/close.py close --target target.json --receipt receipt.json # Linux
```

Only Windows Terminal (automation ID), tmux pane, and kitty window identities can close. Each is
re-enumerated and its binding and frontend ancestry are verified immediately before mutation. Closure
succeeds only after the exact tab has disappeared and the exact frontend identity has exited. Konsole
and incomplete or ambiguous identities refuse.

For a final-command close, use `arm` with caller-owned completion and cancellation files. The watcher
does not inspect a transcript or poll an agent: a matching completion token after arming authorizes the
close; a cancellation file prevents it.

```text
python3 -B <skill-directory>/scripts/close.py arm --target target.json --receipt receipt.json \
  --completion completion.json --completion-token opaque-token --cancel cancel.json
```

The JSON receipt is durable and records `armed`, `cancelled`, `refused`, or `closed`.

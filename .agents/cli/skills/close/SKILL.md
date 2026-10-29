---
name: close
description: Close one explicitly identified CLI frontend and terminal tab while retaining its worktree. Use when a caller supplies the exact process and terminal identities to close.
kind: operation
domain: cli
route: infer
profile: core
applicability: all projects
requires: python
provenance: house
---

# Close an explicit CLI target

`cli:close` is deterministic plumbing, not session interpretation. The caller supplies a target JSON
with frontend and dedicated-wrapper PID/creation identities plus a native terminal identity. For tmux
and kitty, the wrapper must descend from the re-enumerated native pane/window root and the frontend
must descend from that wrapper. For Windows Terminal, startup resolves one owned launch title to one UIA
runtime ID from `GetRuntimeId()` and records it as `tab_binding.runtime_id`, with the observed title and
`windows-uia-unique-title-at-launch`. Immediately before Windows mutation, the close helper launches an
isolated probe which attaches only to the verified frontend console, sets a random temporary title, and
requires that exact title to appear on the exact UIA runtime ID before restoring it. It refuses when
that association is unavailable or ambiguous. It never discovers a target from a model, title, command
name, registry, or transcript.

```text
python -B <skill-directory>\scripts\close.py close --target target.json --receipt receipt.json  # Windows
python3 -B <skill-directory>/scripts/close.py close --target target.json --receipt receipt.json # Linux
```

Only Windows Terminal (runtime ID), tmux pane, and kitty window identities can close. Each is
re-enumerated and its wrapper/frontend ancestry is verified immediately before mutation. Closure
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

---
name: clip
description: Put plain text on Tommy's clipboard (Windows, Linux or macOS), verified, so he can paste a command, path or snippet instead of drag-selecting it out of the transcript. Use for a command he has to run himself, and whenever he says "copy that", "put that on my clipboard", "clip that". A Teams or chat message uses teams:message instead. NOT for GitHub review comments, which are previewed in the reply and posted from here. Applies in every repo, personal and work.
kind: utility
domain: cli
route: infer
profile: core
applicability: all projects
requires: python
provenance: house
---

# clip

Terminal text is awkward to select and picks up stray indentation. Anything Tommy is meant to paste
goes on his clipboard. A Teams or chat message goes through `teams:message`, which renders links and code
before copying; this skill copies the text exactly as written.

## How

Write the exact text to a UTF-8 file in the scratchpad, then run the command with `python3` on Linux/macOS
or `python` on Windows:

```sh
python3 '<skill-directory>/scripts/copy_draft.py' --path '<absolute-path-to-file>'
Copied 42 chars.
```

`copy_draft.py` sits beside this file. It reads UTF-8, trims trailing blank lines, copies the text as plain
text, and reads the clipboard back to verify; it exits non-zero rather than reporting a success it did not
achieve. Relay the character count it prints, so a silent failure can't pass as done.

**Never pipe the text through the command line.** Quoting mangles backticks, `$`, curly quotes and
emoji, and a heredoc through PowerShell is worse. The file is the interface.

## What goes on the clipboard

Only the thing to paste: no "here's the command:" preamble, no `>` quoting and no surrounding fence.

Own one current handoff per task. Gather the ready actions Tommy must run and combine related commands
into one pasteable batch, in dependency order, with explicit failure checks before dependent steps. Name
the batch's actions and targets before asking him to run it. Keep required human approvals in that batch;
combining commands does not authorize an agent to perform them.

For parallel work in one conversation, the parent owns the combined handoff. Workers report required
human actions to that owner without copying commands or opening competing run requests.

Keep that clipboard item current until Tommy reports its result. Prepare later text in files without
copying it or opening another run request. If changed state makes the pending batch obsolete, explicitly
supersede every earlier request, reconcile completed actions, then copy one complete replacement as the
last step before presenting it. Never make the user choose between several outstanding clipboard requests.

```text
Ready actions: start the reviewed test run, approve the exact PRs, queue the reviewed changes.
Handoff: one batch that starts tests, checks success, requests approval, then queues only after success.
While it is pending: prepare other work without replacing the clipboard or asking for another paste.
If a PR has already merged: supersede the pending request and issue one batch containing remaining work.
```

**The clipboard is volatile and nothing tells you it changed.** Any copy anywhere on the machine replaces
it. A "Copied ..." line means it landed at that moment, not that it survived since; re-copy when in doubt.

## A command Tommy has to run himself

**Put it on the clipboard. Never print it for him to copy out of the terminal.** Terminal copy bakes in
wrapping and indentation and silently breaks a long command on paste. Show the command in your reply too
so he can read it, but the clipboard copy is the one he runs.

Do not invent a wrapper script to dodge the problem; write a script only when the thing is genuinely worth
keeping and re-running.

A command that creates or moves a credential still goes to him to run, and pipes the value straight to its
destination so it never reaches the transcript. Never echo the value, and never run such a command
yourself; a half-finished attempt can leave a live credential stranded.

## Say so in one line

End with a short line naming what is on the clipboard and its size, e.g. "On your clipboard (538
chars)." Not a paragraph about the clipboard.

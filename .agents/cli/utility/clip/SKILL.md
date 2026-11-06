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

One thing at a time. The clipboard holds one item, so if you prepared two things, copy the one he asked
for and say the other is ready on request. Copying overwrites silently, so do it as the last step before
telling him it is there.

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

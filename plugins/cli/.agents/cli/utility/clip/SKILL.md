---
name: clip
description: Put text on Tommy's clipboard. Use when giving Tommy a command to run or text to paste, or when he asks to copy text. Not for GitHub review drafts.
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
goes on his clipboard.

## How

Write the exact text to a UTF-8 file in the scratchpad, then run the command with `python3` on Linux/macOS
or `python` on Windows:

```sh
python3 '<skill-directory>/scripts/copy_draft.py' --path '<absolute-path-to-file>'
```

`copy_draft.py` sits beside this file.

The script reads UTF-8, trims trailing blank lines, copies, and reads the clipboard back to verify —
it exits non-zero rather than reporting a success it did not achieve. It prints the character count;
relay that, so a silent failure can't pass as done.

**Never pipe the text through the command line.** Quoting mangles backticks, `$`, curly quotes and
emoji, and a heredoc through PowerShell is worse. The file is the interface.

## What goes on the clipboard

**The message with its markdown intact** — that is the point of copying rather than having him
drag-select. Teams renders markdown in the compose box, so backticks around a service, identity,
resource group, role or flag survive as inline code and make the specifics stand out, and a triple
backtick fence renders a command as a block he can copy straight out. Keep them in.

What stays out is anything that belongs to the transcript rather than the message: no "here's the
message:" preamble, no `>` quoting, no bullet markers you added for readability, no surrounding
fence around the whole thing.

One thing at a time. The clipboard holds one item, so if you drafted a message and a command, copy
the one he asked for and say the other is ready on request. Copying overwrites silently, so do it as
the last step before telling him it is there — a later copy in the same turn strands him pasting the
wrong thing.

**The clipboard is volatile and nothing tells you it changed.** Any copy anywhere on the machine
replaces it — a terminal drag-select most of all, which lands plain text with the terminal's own
wrapping and indentation baked in and no HTML flavour. A message copied several turns ago is not
still there. If he says the backticks did not render, do NOT assume you omitted them: re-copy. The
script writes and verifies off the live clipboard at write time, and exits non-zero on a mismatch —
on Windows both an HTML flavour and a plain flavour; on Linux an HTML flavour by default (via
`wl-copy` or `xclip`), falling back to plain text with the markdown kept intact when only `xsel` is
available, and saying so; on macOS always plain text with the markdown kept intact, because `pbcopy`
has no HTML flavour at all. Either way, a bare "Copied ..." line means it truly landed at that
moment — not that it survived since.

**On Linux, default mode is HTML-only — nothing a plain-text target can read.** `wl-copy`/`xclip`
can set only one clipboard flavour per copy, so the default (rich) copy holds `text/html` and nothing
else; a terminal paste or a plain text field gets nothing useful from it, not a plain fallback. Default
mode is for rich targets — Teams, email. For anything pasted into a terminal or a plain field, use
`--plain-only`.

## A command Tommy has to run himself

**Every command he runs himself — a PR approval, a token or credential step, any `!` line — is built
by `scripts/user_command.py`. Never compose one by hand or write your own wrapper script.** Write the
spec to a scratchpad JSON file and run (`python` on Windows):

```sh
python3 '<skill-directory>/scripts/user_command.py' issue --spec '<absolute-path-to-spec.json>'
```

```json
{"unset": ["GH_TOKEN"], "steps": [{"argv": ["gh", "auth", "token", "--user", "tomjseery"], "capture": "GH_TOKEN"}, {"argv": ["gh", "pr", "review", "--approve", "https://github.com/tj-agents/cli/pull/21"]}]}
```

`unset` clears variables first; steps run in order and stop at the first failure; `capture` exports a
step's output as that variable. Optional `"shell": "bash"` or `"powershell"` overrides the host's `!`
shell.

The script renders one `!` line, copies it `--plain-only`, verifies the clipboard, records that it
issued it, and prints the line and a receipt. Relay both: the line verbatim, never retyped or edited.
A producer that already calls it, such as the bot gh wrapper after `gh pr create` or `gh pr ready`,
prints the line itself; relay that output.

A Stop hook blocks a reply containing any `!` command the builder did not issue. Run the builder; do
not reword.

A credential step pipes or `capture`s the value straight to its destination so it never reaches the
transcript. Never echo the value, and never run such a command yourself — a half-finished attempt can
leave a live credential stranded.

**Teams messages only, for the HTML flavour.** GitHub review comments (`draft-comment`,
`respond-comments`) are previewed in the reply and posted from here, not pasted, so they never go on
the clipboard.

## Say so in one line

End with a short line naming what is on the clipboard and its size, e.g. "On your clipboard (538
chars)." Not a paragraph about the clipboard.

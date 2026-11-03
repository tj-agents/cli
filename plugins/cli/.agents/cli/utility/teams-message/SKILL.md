---
name: teams-message
description: Write a Teams or chat message Tommy will paste, with clickable link text instead of raw URLs and backticked identifiers, then check and copy it with one deterministic command. Use for any Teams message, status update, ready-for-review note or reply he will send, in every repo, including every work:ask draft and a message embedded in another answer.
kind: utility
domain: cli
route: infer
profile: core
applicability: all projects
requires: python
provenance: house
---

# Teams message

Owns a Teams message's markdown and its clipboard handover. In work repositories `work:ask` owns
recipient context, voice and sending.

Phrase every link into the sentence as `[link text](https://...)`, never a raw URL. Backtick identifiers,
types, files, services, roles and flags. No headings, lists, bold, blockquotes, em dashes or a fence around
the message; fence only a command the reader will run. Keep it to a few sentences.

Write the exact message to a UTF-8 scratch file and run `teams_message.py` beside this file as the final
action, with `python3` on Linux/macOS or `python` on Windows. It rejects a draft that breaks these rules,
line by line, and otherwise copies it through `cli:clip` and verifies the read-back:

```text
python -B '<skill-directory>/scripts/teams_message.py' --path '<draft.md>'
Draft:  I've been working on [these two pages](https://dev.azure.com/org/proj/_git/web/pullrequest/412), they're ready for review. The main change is the `OrderSummary` grid.
Output: Copied 164 chars, 1 code spans, 1 links, HTML + plain verified on the clipboard.
```

Fix every reported line and rerun; never copy around the check. Reply in one or two lines naming the
recipient or purpose and the verified copy; do not print the draft inline. If the copy fails, say so,
keep the file, and never claim it is on the clipboard.

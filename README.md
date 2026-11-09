# tj

Fast shell tools for agentic programming, on Windows and Linux. `tj` is the deterministic OS and terminal
mechanics layer of [tj-agents](https://github.com/tj-agents): judgement lives in skills, `tj` does the
parts the hosts themselves don't cover, with the same behaviour from Claude Code and Codex. `tj` provides
two everyday commands, plus `tj run`, the root process of every agent tab:

| Command | What it does |
| --- | --- |
| `cr` | Search your Claude Code or Codex sessions by **anything said in them**, preview how each one ended, and press Enter to resume it in the folder it was started from. |
| `f` | Fuzzy-jump to a project folder. `f core` goes straight to the best match; plain `f` opens a picker. |
| `run` | Run an agent in its tab and close its work item out once it is finished: agent stopped, worktree removed, card Done, tab closed. |
| `message` | Send another Claude or Codex session a message, addressed by its tab title, with the same behaviour from either host. |

Any other `tj <name> ...` runs `tj-<name> ...` from PATH, so `tj item` runs the `tj-item` tool from
`tj-agents/work-items`.

## CLI skills

The shared `tj-agents` marketplace also publishes `cli@tj-agents`:

- [`cli:cd`](.agents/cli/utility/cd/SKILL.md) continues an authorized task in a nested directory or linked worktree, using supported native session control when available.
- [`cli:clip`](.agents/cli/utility/clip/SKILL.md) puts plain text on the OS clipboard (Windows, Linux or macOS), verified, so it can be pasted elsewhere; Teams messages use `teams:message`.

`tj-cli` supplies deterministic process, terminal, and linked-worktree mechanics to Agent. Agent owns
session-aware target resolution and lifecycle decisions. Native control supports Windows Terminal, tmux and
Kitty; missing or ambiguous proof produces a refusal receipt. Windows Terminal must allow application title
updates so the console-to-tab association can be verified.

## Install

You need Python 3.12 or newer, [fzf](https://github.com/junegunn/fzf) 0.54 or newer, and either a Rust
toolchain (to build from this checkout) or the [GitHub CLI](https://cli.github.com) (to download a verified
prebuilt release).

**Linux**

```bash
git clone https://github.com/tj-agents/cli ~/projects/tj-agents/cli
~/projects/tj-agents/cli/linux/install.sh
```

**Windows** (PowerShell)

```powershell
git clone https://github.com/tj-agents/cli $HOME\source\repos\tj-agents\cli
& $HOME\source\repos\tj-agents\cli\windows\install.ps1
```

Then open a new terminal.

You can run the installer as often as you like. It updates whatever is out of date and reports `ok`
for everything else, so re-running it after a `git pull` is how you upgrade. It:

1. builds `tj` and `tj_hook` (or downloads the latest verified release) and puts them in `~/.local/bin` on Linux or
   `%LOCALAPPDATA%\Programs\tj` on Windows, adding that folder to your user PATH on Windows;
2. checks for fzf. On Windows it installs fzf with winget if it's missing; on Linux it tells you
   the package to install;
3. installs [Handy](https://github.com/cjpais/Handy) for voice dictation on desktop machines:
   Windows uses winget; Linux desktops use a SHA-256-verified upstream package through APT or DNF,
   with GitHub CLI and sudo required for a non-root installation. Other Linux distributions need Handy
   installed with `handy` on PATH before setup. CI and Linux sessions without a display report this step
   as not applicable; for SSH or WSL terminal use, run setup on the desktop host;
4. installs [worktrunk](https://github.com/max-sixty/worktrunk) 0.81.0 as `git-wt` next to `tj`, from a
   SHA-256-pinned release archive. `tj run` removes worktrees with it;
5. adds one marked block to each shell startup file it finds (bash, zsh, fish, PowerShell):

   ```bash
   # >>> tj-agents/cli >>>
   command -v tj >/dev/null 2>&1 && eval "$(tj init bash)"
   # <<< tj-agents/cli <<<
   ```

| | Linux | Windows |
| --- | --- | --- |
| Check what's set up, change nothing | `linux/install.sh --check` | `windows\install.ps1 -Check` |
| Remove CLI binaries and shell wiring | `linux/install.sh --uninstall` | `windows\install.ps1 -Uninstall` |

### Voice dictation in Claude and Codex

Handy is a desktop prerequisite provisioned by the existing installer. A clone alone does not install
software. Setup checks for Handy on each run, installs it only when missing, and fails if installation
fails. Check mode reports a missing application without installing or downloading anything. CLI uninstall
preserves Handy and its models, as it preserves the shared fzf prerequisite.

Open Handy once, download a speech model, select your microphone and hold-to-talk shortcut, and enable
launch at login in Handy's settings. Use `Shift+Insert` as the paste method for Windows terminals, or the
paste shortcut your terminal accepts. Then focus the Claude or Codex input, speak, review the transcript,
and press Enter yourself. This uses the terminal's normal text input; no per-agent skill injection is
needed. Setup verifies installation, not microphone access, model readiness or successful dictation.

Linux desktop shortcuts and paste helpers depend on X11 or Wayland; complete
[Handy's Linux setup](https://handy.computer/docs/linux) before using dictation. Headless setup does not
claim voice input is ready.

## `cr`: find and resume a session

```text
cr                 pick Claude or Codex, then search this repo's sessions
cr tech debt       open pre-filtered to sessions where "tech" and "debt" were both said
cr --claude        skip the agent picker (or --codex)
cr --all           every project, not just this repo
cr --list          print the rows instead of opening the picker
cr --rebuild       throw the index away and re-read every transcript
```

Inside the picker:

| Key | Action |
| --- | --- |
| type | filter live across everything said in each session |
| `Tab` | show the end of the conversation, plus the lines that match what you typed |
| `Enter` | resume the session in the directory it was started from |
| `Ctrl-A` | widen the search to every project, keeping what you typed |

**Scope.** In a git repo, `cr` searches sessions started in that repo and in any folder that has
ever been under it. If you run it from the main checkout and the repo has other worktrees, it asks
which one to search first. With no sessions in scope, it widens to everything on its own.

**Labels.** Each row shows when the session was last active, its size and its opening prompt.
`PLAN` marks a Claude session that started a plan and never finished it. A work-item id
(`AB#12345`) appears when the prompt or branch names one.

**Speed.** The first run reads every transcript in parallel, which takes about a quarter of a
second for 200 MB of history. After that, only new or changed sessions are re-read. Indexes and
pre-rendered preview panes are kept in your cache folder (`~/.cache/tj` or `%LOCALAPPDATA%\tj`),
never inside the agents' own config folders.

Codex subagent transcripts (guardian reviews and the like) are left out, because they resume into
their parent session.

## `f`: jump to a project

```text
f                  open a picker over every folder under your projects root
f cppnote          jump straight to the best match (cpp/note); separators and case don't matter
f --root D:\work   search somewhere else this once
```

The projects root is `$TJ_F_ROOT` if you set it, otherwise `~/projects`, otherwise
`~/source/repos`. Dependency and build folders (`.git`, `node_modules`, `target`, `bin`, `obj`,
`build` and so on) are skipped.

`f` has to be a shell function rather than a program, because no program can change the
directory of the shell that started it. `tj f` prints the chosen path and the `f` function `cd`s
into it. The functions live in `linux/tj.sh`, `linux/tj.fish` and `windows/tj.ps1`, and
`tj init <bash|zsh|fish|pwsh>` prints the one for your shell.

The Rust source in `src/` is shared by both platforms. Only the installers and the shell functions
differ, so only those are split into `linux/` and `windows/`.

## `tj run`: the agent tab's root process

```text
tj run --host claude|codex --title T --cwd WORKTREE --launch-id U [--item REF] [--parent HOST:ID] -- <agent argv>
tj done              last step of work with no PR: close out after about 10 idle minutes
tj done --cancel     new input arrived; withdraw that (a no-op outside tj run)
tj closeout [REF]    close an item's sessions; with no REF, sweep tabs closed by hand or crashed wrappers
```

The launcher opens the tab (`wt.exe new-tab … tj run …` or `tmux new-window … tj run …`); `tj run` never
opens one. It starts the agent as its child: in a Job Object on Windows, or in its own foreground process
group on Linux, where the agent also dies with the wrapper. It registers the launch with `agent-store
launch` and exports `TJ_RUN_PID`, `TJ_RUN_STARTED_AT`, `TJ_RUN_LAUNCH_ID` and `AGENT_CLI_TAB_TITLE` to the agent.

It closes out when any of these happens:

- every PR for the item has merged, according to `tj item prs` (checked every two minutes). A PR that closed
  without merging keeps the tab open, and the wrapper reports it;
- `tj done` was run and no new input has cancelled it for about 10 minutes;
- `agent-store` marks the launch or its session `closing`, which is what `tj closeout REF` does.

Before stopping anything, it proves that closeout loses nothing: the worktree must be clean, it must have no
commits off the default branch unless the PRs merged, and worktrunk's `pre-remove` hooks must pass. If any of
these fails, the session stays open and the wrapper prints the reason. Closeout then:

1. asks the agent to exit: a console break or SIGINT for Claude, a Ctrl+C keypress for Codex (through
   `tmux send-keys` on Linux);
2. kills what is left after 10 s and waits until no process of the tree remains;
3. runs `git-wt remove --foreground`, but only on a linked worktree, never a primary checkout;
4. runs `tj item progress REF --state Done` and `agent-store end`;
5. exits 0, so Windows Terminal (`closeOnExit` default) or tmux closes the tab.

Helpers run hidden through `tj_hook` on Windows, and in their own session on Linux, so they never touch the
agent's console. If the agent exits by itself, the wrapper ends the launch, keeps the worktree and exits
with the agent's code.

## `tj message` and `tj subscribe`: messaging between sessions

```text
tj message <title|host:id> <text>   send a message; the recipient sees who sent it and how to reply
tj subscribe <title|host:id>        one notice when a Codex child next goes idle or exits
```

The target is looked up with `agent-store sessions --title` (or `agent-store show` for a handle) and must be
exactly one session that has not ended. The caller is the Claude session when `CLAUDECODE=1`, otherwise the
Codex thread in `CODEX_THREAD_ID`. Delivery depends on the target:

| Target | Caller | Delivery |
| --- | --- | --- |
| Codex | either | `codex queue --thread <id>`; it starts a turn at the target's next idle point |
| Claude | Claude | nothing is sent and the exit code is 3: use the native `SendMessage` tool |
| Claude | Codex or a shell | the target session's relay posts it to that session's inbox socket |

Each Claude session gets its relay from the cli plugin's async `SessionStart` hook, `tj bridge`, which lives
until the session exits. It listens on a loopback port and records the port and a random secret in
`<cache>/bridge/<session-id>.json`, readable only by you. It posts each message to
`CLAUDE_CODE_MESSAGING_SOCKET` with an auth line holding `CLAUDE_CODE_MESSAGING_TOKEN`, so the session
verifies it as its own child's message and starts a turn even in `bypassPermissions` mode.

`tj subscribe` is the Codex counterpart of `SendMessage notify_when_idle`. The child must be loaded in the
shared Codex app-server, so an embedded Codex is refused before anything is watched. A background process
rejoins the thread over `codex app-server proxy` and waits for `thread/status/changed` to `idle`, or for
`thread/closed`. It sends one notice to the parent and exits: `codex queue` for a Codex parent, the
parent's own inbox socket for a Claude parent. After 12 hours without either event, the notice reports that
the subscription expired. A Claude child reports to a Codex parent with `tj message`.

## Configuration

| Variable | Default | Meaning |
| --- | --- | --- |
| `TJ_F_ROOT` | `~/projects`, then `~/source/repos` | where `f` searches |
| `TJ_CACHE_DIR` | OS cache folder + `/tj` | where `cr` keeps its index, `tj done` its marks and each Claude relay its endpoint |
| `TJ_BIN_DIR` | `~/.local/bin` / `%LOCALAPPDATA%\Programs\tj` | where the installer puts `tj` and `tj_hook` |
| `CLAUDE_CONFIG_DIR` | `~/.claude` | Claude Code's config folder, as Claude Code itself reads it |
| `CODEX_HOME` | `~/.codex` | Codex's config folder, as Codex itself reads it |

## Hidden processes

`tj_hook -- <executable> [arguments...]` runs a command with inherited input, output,
environment and working directory, returning its exit status. On Windows the launcher
has no console window and starts the command in a console hidden from creation. Ordinary
descendants inherit that hidden console. A Windows Job Object keeps the command tree
owned by the launcher: exiting or terminating the launcher also terminates its descendants.
Windows `.cmd` and `.bat` targets run automatically through the same hidden, job-owned launcher;
paths containing `%`, a matching `.cmd.exe`/`.bat.exe` sidecar, or arguments containing line breaks
or batch shell characters are rejected rather than interpreted as batch source.
Use `tj_hook --preserve-descendants -- <executable> [arguments...]` only when descendants
must outlive a normally exiting root command; cancellation before that exit still terminates
the owned tree. On Linux this option retains the launcher's existing `exec` behavior.
On Linux the launcher replaces itself with the command, retaining its process identity
and signal behavior.

```text
tj_hook -- python -I -B hook.py
```

Python consumers use the installed `tj-cli` package:

```python
from tj_cli import process

result = process.run(["git", "status", "--porcelain"], capture_output=True, text=True, timeout=5)
```

`process.run` and `process.start` invoke the same native `tj_hook` executable. Install
the native CLI and keep its binary directory on the invoking process's PATH; installing
the Python wheel alone does not install the native binaries. A missing launcher fails
explicitly. `run` owns a separate process session on Linux and kills that process group
on timeout or interruption. `start` returns a `Popen` handle for callers managing its
lifetime themselves. Timeout and nonzero-exit errors identify the requested command.

`native_install.pending_native(directory)` checks the managed pair locally and reports missing or
incompatible binaries. It probes the helper's version without downloading or changing files;
the current runtime requires native version 0.2.0 or newer.

Both functions accept typed options for standard streams, working directory, environment,
text encoding, buffering and process creation. Unknown options fail before launch.
`run` additionally supports input, output capture, timeout and exit-status checking.
Shell commands require an explicit shell executable. For `cmd.exe`, `/c` and `/k`
require exactly one complete command string, preserving its shell syntax and quotes.
Separate command arguments are rejected.
The mode must be a separate switch. Supported preceding switches are `/d`, `/s`,
`/q`, `/a`, `/u`, `/E:ON`, `/E:OFF`, `/F:ON`, `/F:OFF`, `/V:ON` and `/V:OFF`.

Hooks must use the launcher as their executable. A host-created shell runs before the
launcher, so its visibility must also be controlled by the host. Code that explicitly
requests a new console can bypass ordinary console inheritance; hook registration and
subprocess checks must enforce the shared path.

## Development

The repository also owns the `tj-cli` Python distribution under `packages/tj-cli/`.
Its regular package groups reusable mechanics by capability. `tj_cli.paths` exposes literal executable-search
PATH operations; `tj_cli.processes` holds exact process identities. Agent and Personal consume the
public API:

```python
from tj_cli.paths import prepend

child_entries = prepend(existing_entries, tools_directory)
```

The API keeps literal search locations and their order, including meaningful empty
entries. Credential filtering belongs to the caller. Python development uses the
repository's uv workspace:

```text
uv sync --locked
uv run ruff format --check
uv run ruff check
uv run mypy
uv run pytest
uv build --package tj-cli
```

The Rust application remains in `src/`:

```bash
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt
```

CI runs all three on Linux and Windows. It then runs each installer twice and fails if the second
run changes anything. A successful current `main` commit automatically publishes the paired prebuilt binaries.

| Path | Owns |
| --- | --- |
| `linux/` | the Linux installer and the bash/zsh/fish functions |
| `windows/` | the Windows installer and the PowerShell functions |
| `src/bin/tj_hook.rs` | the windowless executable entry point |
| `src/process.rs` and `src/process/` | native command execution and Windows process containment |
| `packages/tj-cli/src/tj_cli/process/` | the Python API for the native launcher |
| `src/run/` | `tj run`, `tj done` and `tj closeout` |
| `src/message.rs`, `src/bridge.rs`, `src/subscribe.rs` | `tj message`, the Claude session relay and `tj subscribe` |
| `src/process/agent.rs` and `src/process/agent/` | the agent's job or process group, per-host stop, tree kill |
| `src/f.rs` | folder walk, match scoring, picker |
| `src/cr/parse.rs` | reading Claude and Codex transcripts into searchable text and turns |
| `src/cr/render.rs` | the Tab preview pane |
| `src/cr/index.rs` | the incremental on-disk index |
| `src/cr/scope.rs` | repo and worktree scoping |
| `src/init.rs` | `tj init`, which prints the functions from `linux/` and `windows/` |

# tj

Fast shell tools for agentic programming, on Windows and Linux. `tj` is the deterministic OS and terminal
mechanics layer of [tj-agents](https://github.com/tj-agents): judgement lives in skills, `tj` does the
parts the hosts themselves don't cover, with the same behaviour from Claude Code and Codex. `tj` provides
two everyday commands, plus `tj run`, the root process of every agent tab:

| Command | What it does |
| --- | --- |
| `cr` | Search your Claude Code or Codex sessions by **anything said in them**, preview how each one ended, and press Enter to resume it in the folder it was started from. |
| `f` | Fuzzy-jump to a project folder. `f core` goes straight to the best match; plain `f` opens a picker. |
| `cd` / `cdi` | Use [zoxide](https://github.com/ajeetdsouza/zoxide) to jump to a previously visited directory by name, or choose one interactively with `cdi`. |
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

You need Python 3.12 or newer, [zoxide](https://github.com/ajeetdsouza/zoxide), and either a Rust
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
2. checks for zoxide. On Windows it installs it with winget when missing; on Linux it tells you
   the package to install (`pkexec pacman -S --needed zoxide` on Arch);
3. adds one marked block to each shell startup file it finds (bash, zsh, fish, PowerShell):

   ```bash
   # >>> tj-agents/cli >>>
   command -v tj >/dev/null 2>&1 && eval "$(tj init bash)"
   # <<< tj-agents/cli <<<
   ```

`tj init` owns zoxide initialization for Bash, Zsh, Fish and PowerShell, using `--cmd cd`.
Keep only the tj-agents block in your shell profile; remove separate `zoxide init` lines.
Normal `cd path` still works; after visiting a directory, `cd name` can find it from elsewhere and
`cdi name` opens an interactive picker. `f` searches the projects tree, including directories you have
never visited. Uninstall removes the CLI wiring and binaries, leaving zoxide and its history installed.

| | Linux | Windows |
| --- | --- | --- |
| Check what's set up, change nothing | `linux/install.sh --check` | `windows\install.ps1 -Check` |
| Remove CLI binaries and shell wiring | `linux/install.sh --uninstall` | `windows\install.ps1 -Uninstall` |

[fzf](https://github.com/junegunn/fzf) 0.75.0,
[worktrunk](https://github.com/max-sixty/worktrunk) 0.81.0, installed as `git-wt` next to `tj` for `tj run` to
remove worktrees with, and [Handy](https://github.com/cjpais/Handy) are installed and kept current by
[Agent](https://github.com/tj-agents/agent#install)'s install and refresh, through `tj_cli.fzf_install`,
`tj_cli.worktrunk_install` and `tj_cli.handy_install` at the `tj-cli` revision Agent pins.
The CLI shell installers own native binaries, shell wiring and zoxide. Agent owns the additional tools listed above.

The managed installers below each write a JSON receipt in the tj bin directory. The shared format records `schema: 1`,
`version`, `hashes` (installed file path to SHA-256), `identity` and `archive_digest`. Native receipts also
record the immutable source revision; Handy records its absolute desktop application path.
Unreceipted or changed installations need installation again. The `pending` functions return a reason
string for drift, or `None` when current, using files only:

| API | Receipt under the tj bin directory |
| --- | --- |
| `tj_cli.fzf_install.pending(bin_dir: Path) -> str \| None` | `.tj-fzf.json` |
| `tj_cli.worktrunk_install.pending(bin_dir: Path) -> str \| None` | `.tj-worktrunk.json` |
| `tj_cli.handy_install.pending(bin_dir: Path) -> str \| None` | `.tj-handy.json` |
| `tj_cli.native_install.pending(bin_dir: Path, *, revision: str \| None = None) -> str \| None` | `.tj-native.json` |

The installers' `--bin-dir` defaults to `TJ_BIN_DIR`, otherwise
`~/.local/bin` on Linux or `%LOCALAPPDATA%\Programs\tj` on Windows. Pass Agent's pinned CLI revision
to native `pending` to check revision drift too.
The existing native `pending_native` checks runtime compatibility by invoking `tj_hook`; refresh stale
checks should use `pending`. Agent's receipt watch registration is a separate integration change.

### Voice dictation in Claude and Codex

Handy is a desktop prerequisite provisioned by Agent's install and refresh. They check for Handy on each
run, install or upgrade it to the pinned 0.10.0 when its receipt is missing or stale, and fail if installation
fails: Windows uses winget; Linux desktops use a
SHA-256-verified upstream package through APT or DNF, with GitHub CLI and sudo required for a non-root
installation. Automatic Linux setup requires one of those package managers. CI and Linux sessions
without a display report the step as not applicable; for SSH or WSL terminal use, install on the desktop
host. Install `--check` reports a missing or stale installation without installing or downloading anything.
`python -m tj_cli.handy_install --no-escalate` never calls sudo or starts a Windows installation that
could prompt for elevation. A required privileged Linux installation or Windows desktop setup fails
with one line; an intact current installation remains a no-op. The installer entry point decides whether
Handy applies to its environment. Handy's `pending(bin_dir)` always checks the supplied directory;
callers decide whether to skip the desktop prerequisite in CI or headless sessions.

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
tj run [--host claude|codex] [--title T] [--cwd WORKTREE] [--launch-id U] [--item REF] [--parent HOST:ID] -- <agent argv>
tj done              the session's last step: close out after about 10 idle minutes
tj done --cancel     new input arrived; withdraw that (a no-op outside tj run)
tj reload --revision R   publish installed plugins; every tab reloads them in place once idle
tj refresh           update plugins and the machine install, then apply them to this tab now
tj closeout [REF]    close an item's sessions; with no REF, sweep tabs closed by hand or crashed wrappers
```

The launcher opens the tab (`wt.exe new-tab … tj run …` or `tmux new-window … tj run …`); `tj run` never
opens one. It starts the agent as its child: in a Job Object on Windows, or in its own foreground process
group on Linux, where the agent also dies with the wrapper. It registers the launch with `agent-store
launch` and exports `TJ_RUN_PID`, `TJ_RUN_STARTED_AT`, `TJ_RUN_LAUNCH_ID` and `AGENT_CLI_TAB_TITLE` to the agent.

Only the agent's command line is required, so a shell function can run a typed `claude` or `codex` under it:
the host defaults to the executable's name, the folder to the current one, the title to the repository or
worktree folder's name (suffixed `-2`, `-3`… while an open launch holds it) and the launch id to a random one.
The executable must be the native `claude` or `codex`, not an npm `.cmd` shim.

Before each start, `tj run` invokes Agent's `tj-prepare` with the host, working directory
and host arguments. Agent reconciles the repository's selected plugins and its standards index before
the host reads native instructions. Missing preparation or a preparation failure prints a warning and
the session still starts. Install Agent to provide `tj-prepare`.

Only the tab's own agent speaks for it. `tj done` and the tab part of `tj refresh` act only when
the caller descends from the wrapper through its agent, with no second copy of that agent in between; a tab,
shell or agent started from inside the session inherits `TJ_RUN_*` but is ignored. `tj launch` prints the
launch the caller belongs to, for Agent's `SessionStart` binding.

Updates reach running tabs without a restart. `tj reload --revision R`, which Agent's plugin update runs once,
replaces `<cache>/published`; every wrapper checks it and its own executable on each half-second poll and
applies a new revision in place:

- Claude: once Claude's own record for the agent process, `<claude home>/sessions/<pid>.json`, has said
  `idle` for 2 s and the keyboard and mouse have been idle 2 s (Windows; Linux has only Claude's status), the
  wrapper types `/reload-plugins` and Enter into the shared console (`WriteConsoleInputW`, or `tmux
  send-keys`). Claude reports a running turn, tool or `!` command as busy, so none is typed into. Claude has no
  other way to reload a running interactive session. Text typed but not sent is invisible to the wrapper; the
  command is appended to it.
- Codex: the wrapper asks the shared app-server for `config/batchWrite` with `reloadUserConfig`, which reloads
  plugins, skills, hooks and `config.toml` into every loaded thread. The TUI's own `$` and `@` completion
  lists stay as they were until it restarts.

A replaced `tj` executable restarts the agent once the new file has settled and runs, Claude has been idle
30 s with no shell running under it (a background task), and input has been idle 30 s. The wrapper stops the agent the way closeout does
and hands the tab to the new binary, which starts the agent again as `claude --resume <id>` or `codex resume
<id>`, keeping the original options and dropping the prompt and any earlier session choice. Linux replaces the
process in place; Windows keeps the old wrapper as the tab's root, waiting for its successor, since the tab
closes when its first process exits. The launch row, its marks and the conversation are kept. The id is the
session `agent-store show --launch-id` binds to the launch; with none bound the agent keeps running and the
wrapper says so once. A resume that exits with an error leaves the tab open with the reason. Codex exposes no
turn state to the wrapper, so a Codex tab takes a new `tj` on `tj refresh` or its next start.

`tj refresh`, from any shell or typed as `! tj refresh` in either host, runs `tj plugins update` for each
installed host and `tj install`, then applies the result to its own tab at once, reloading in place or,
when `tj` itself changed, restarting into the new binary.

It closes out only when one of these happens:

- `tj done` was run and no prompt, or Claude tool use, has cancelled it for about 10 minutes;
- `agent-store` marks the launch or its session `closing`, which is what `tj closeout REF` does.

A merged PR never stops a session: an item can be merged before its session starts or while it still has
work. The first time `tj item prs` (checked every two minutes) reports every PR for the item merged, the
wrapper asks the agent once, through its Claude relay or Codex queue, to run `tj done` if nothing is left.
An agent that is unsure carries on and the tab stays open. A refused check withdraws `tj done` and prints
the reason; the agent runs it again once the reason is fixed.

Before stopping anything, it checks every linked worktree recorded for the exact launch and native session, including roots created after launch and in other repositories. Each root must be registered, clean and integrated into its freshly fetched default branch; a merged item PR alone cannot authorize deletion. Worktrunk supplies the content checks for squash and rebase merges. Another session with live or unknown process ownership keeps the worktree open.

After stopping its own agent process tree, the wrapper repeats those checks and refuses changed ownership or content. Each worktree and branch must disappear before its ownership record is released. The item becomes Done and the launch closes only after all owned roots are removed. Primary checkouts remain. A missing or unrecorded ownership root requires recovery; cleanup never guesses ownership from a neighbouring folder or adopts legacy location metadata.

1. asks the agent to exit: a console break or SIGINT for Claude, a Ctrl+C keypress for Codex (through
   `tmux send-keys` on Linux);
2. kills what is left after 10 s and waits until no process of the tree remains;
3. runs `git-wt remove --foreground` for each owned linked worktree;
4. runs `tj item progress REF --state Done` and `agent-store end`;
5. exits 0, so Windows Terminal (`closeOnExit` default) or tmux closes the tab.

Helpers run hidden through `tj_hook` on Windows, and in their own session on Linux, so they never touch the
agent's console. If the agent exits by itself, nothing is left running, so the wrapper also cleans up when every PR for the item has merged. Otherwise it ends the launch and keeps the unfinished worktrees. It preserves the agent's exit code. `tj closeout` also inspects ended launches whose cleanup has not completed.

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
| `TJ_CACHE_DIR` | OS cache folder + `/tj` | where `cr` keeps its index, `tj done` and `tj refresh` their marks, `tj reload` the published revision and each Claude relay its endpoint |
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
| `src/run/` | `tj run`, `tj done`, `tj reload`, `tj refresh` and `tj closeout` |
| `src/message.rs`, `src/bridge.rs`, `src/subscribe.rs` | `tj message`, the Claude session relay and `tj subscribe` |
| `src/process/agent.rs` and `src/process/agent/` | the agent's job or process group, per-host stop, tree kill |
| `src/f.rs` | folder walk, match scoring, picker |
| `src/cr/parse.rs` | reading Claude and Codex transcripts into searchable text and turns |
| `src/cr/render.rs` | the Tab preview pane |
| `src/cr/index.rs` | the incremental on-disk index |
| `src/cr/scope.rs` | repo and worktree scoping |
| `src/init.rs` | `tj init`, which prints the functions from `linux/` and `windows/` |

# tj

Fast shell tools for agentic programming, on Windows and Linux. `tj` is the deterministic OS and terminal
mechanics layer of [tj-agents](https://github.com/tj-agents): judgement lives in skills, `tj` does the
parts the hosts themselves don't cover. Start a managed conversation with `tj claude` or `tj codex`, using
the provider's native terminal interface and arguments. Session search and directory navigation are also
available:

| Command | What it does |
| --- | --- |
| `cr` | Search your Claude Code or Codex sessions by **anything said in them**, preview how each one ended, and press Enter to resume it in the folder it was started from. |
| `f` | Fuzzy-jump to a project folder. `f core` goes straight to the best match; plain `f` opens a picker. |
| `cd` / `cdi` | Use [zoxide](https://github.com/ajeetdsouza/zoxide) to jump to a previously visited directory by name, or choose one interactively with `cdi`. |
| `tj claude` / `tj codex` | Start an independent managed conversation in the current terminal, with native arguments and recorded completion. |
| `tj finish` / `tj wait` / `tj hold` | Request completion after the final response, record a genuine wait, or keep the session open. |
| `tj receipt` | Inspect the durable result and closeout record for a managed launch. |
| `tj message` | Address another session by title or handle through the common messaging interface. |

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

Managed conversations also need [Agent's installation and repository setup](https://github.com/tj-agents/agent#install)
and at least one installed native provider. Use your own provider authentication and repository access.
Agent supplies the host adapters, lifecycle hooks and `tj` command shims; the CLI-only installation supplies
the binaries and shell tools below.

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
[worktrunk](https://github.com/max-sixty/worktrunk) 0.81.0, installed as `git-wt` next to `tj` for managed
worktree cleanup, and [Handy](https://github.com/cjpais/Handy) are installed and kept current by
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
Native install replaces the `tj`/`tj_hook` pair even while tabs run it: the old files move to
`<bin-dir>/.tj-retired` (Windows can rename a running executable but not delete it) and each later install
removes the ones no longer running. Running wrappers then hand over to the new `tj` when idle.

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

## Managed sessions

```text
tj claude [native arguments]
tj codex [native arguments]
tj claude --resume <session-id>
tj codex resume <session-id>
tj claude auth login
tj codex login
```

`tj claude` and `tj codex` run the provider's native terminal interface in the current terminal. Native
flags, prompts, subcommands and resume arguments pass through to that provider; relative directory
arguments keep their meaning. Help, version, authentication and other maintenance commands run directly
without creating a managed conversation. Use the provider's own authentication, model selection and access
controls. Omitting TJ's permission preference preserves native controls; see
[Agent's repository permissions](https://github.com/tj-agents/agent#repository-permissions).

A public launch is independent: it does not inherit a calling agent's parent or work item. A work item is
optional. To delegate into a named Windows Terminal or tmux tab, use
[Agent's handoff](https://github.com/tj-agents/agent/blob/main/.agents/agent/utility/handoff/SKILL.md), which
requires exactly one of `--parent <host:id>` and `--independent`. `tj start` supplies the work-item launch
workflow. A plain `claude` or `codex` command starts the provider directly and does not add TJ supervision
or closeout receipts.

The managed wrapper owns the native child process: a Job Object on Windows or a foreground process group
on Linux. It records a launch and binds the native session through Agent's SessionStart hook. The title
defaults to the repository or worktree folder name, with a numeric suffix while another open launch holds
it. Only the managed session's own agent can change its completion state; inherited environment variables
alone do not establish ownership.

Initial registration is pending while the native host starts. If TJ reports that it is still awaiting
native session registration, check that Agent is enabled and its SessionStart hook succeeded. Invalid
bindings and Store errors are reported immediately. Exiting without a verified binding leaves managed
completion unverified and preserves the provider's exit code.

In a repository selecting TJ plugins, startup invokes `tj prepare` to check selected plugins and the
standards index before the host reads instructions. Startup and refresh leave authored instructions and
repository settings untouched. Drift reports an explicit `tj prepare --host <host> --cwd <path> --write`
command. Preparation must succeed before startup records a launch, binds a native session, or transfers
recovery ownership and worktree claims. A preparation failure reports its cause and stops native launch.
Install and enable Agent in that repository for managed completion.

Work-item launches accept a full tracker reference or URL. A bare id such as `461` or `#461` is resolved
by `tj-item --cwd WORKTREE reference ID` using that project's tracker profile before a launch is recorded
or an agent starts. `tj closeout ID` uses the current directory's profile before attempting closeout.
A missing profile or failed resolution stops the command; a Git code remote never selects a tracker.
Recovery and binary handover carry the resolved reference with the original agent arguments.

### Completion, waits and keep-open

Run these commands from the managed agent's own tool process:

```text
tj finish
tj wait user "The user must choose the deployment environment"
tj wait child "The delegated review must return"
tj wait ci "The required checks must pass"
tj wait approval "Deployment approval is pending"
tj hold
tj hold --release
tj receipt
```

`tj finish` records intent for the current native turn. The agent then writes its final response normally.
Agent's native Stop adapter records that response against the exact session and turn; a finish request
alone is insufficient. Missing, interrupted or conflicting evidence leaves the session incomplete. TJ
does not infer success from response wording, a clean process exit, idleness or a merged PR.

`tj wait <user|child|ci|approval> <reason>` records what is pending and withdraws finish intent. It does not
start a poller or synthesize input. Pending native background work also prevents closeout. Once work
resumes, the agent completes it and requests finish for that turn. `tj hold` keeps the session open even
when a final result is available. The hold survives new input and resuming that conversation; only
`tj hold --release` releases it, and release alone does not request completion.

Submitted native input cancels pending completion and invalidates closeout authorization for the previous turn.
Idleness creates no user messages, nudges or model turns.

Neither provider's idle record proves the absence of unsent drafts or excludes new input during shutdown.
TJ therefore delivers the final result but retains the native window and worktrees, with the reason visible
in its receipt. Exit Claude or Codex normally after checking its native work; TJ then rechecks the final result, native liveness and
repository state before cleanup. A finish request does not authorize TJ to discard unobservable native work.

The completion record retains the final response, session and launch identity, wait or hold state, and
receipts for result delivery, cleanup and tracker completion. A result being recorded is distinct from
closeout succeeding. Read the record from the owning agent with `tj receipt`, or from a terminal after
the session has closed:

```text
tj receipt --launch <launch-id>
```

Records live in durable application data, separately from disposable caches. For a parented session,
result delivery uses the common [messaging interface](#tj-message-and-tj-feed-messaging-between-sessions)
and the exact recorded parent. Independent sessions retain their native final response and local receipt.

### Safe closeout and recovery

After normal native exit, TJ checks every linked worktree claimed by the exact native session for the
current launch or earlier launches of the same work item, including roots in other repositories. Earlier
wrappers must be provably stopped. Each root must be registered, clean and integrated into its freshly
fetched default branch; a merged item PR alone cannot authorize deletion. Worktrunk supplies the content
checks for squash and rebase merges. Authored changes, live or unknown process ownership, and unmanaged
claims prevent removal. Primary checkouts remain.

Historical cleanup requires a shared linked work item. Unlinked launches retain separate claims and
cleanup journals; their common native conversation does not establish a shared task. Cleanup never guesses
ownership from a neighbouring folder or adopts legacy location metadata.

With a valid result receipt and closeout authorization, the wrapper:

1. waits for a normal native exit;
2. rechecks completion authorization and native liveness;
3. rechecks ownership and content, and runs `git-wt remove --foreground` for each safe linked worktree;
4. releases each claim only after its worktree and branch are gone, marks the linked item Done when there
   is one, and closes the launch;
5. records completed closeout and exits, returning to the calling shell or closing a launcher-owned tab.

Cleanup journals and completion receipts retain unfinished stages for recovery. A failed native exit
preserves unfinished worktrees; a successful exit permits cleanup only with a valid final result and all
the same checks. Helpers run hidden through `tj_hook` on Windows and in their own session on Linux.

```text
tj closeout <work-item-ref>
tj closeout
tj recover --open
```

`tj closeout <ref>` retries verified completion for dead launches of that item; without a reference it
sweeps interrupted closeout work. It skips live wrappers and cannot replace missing final evidence or
override a hold. Resume an incomplete session to finish its work. `tj recover --open` reopens launches
recorded as open whose native agent is no longer running. Logoff, shutdown or a crash can leave that mark;
on Windows an ordinary tab close records it closed. On Linux a tab hangup can arrive before the wrapper
records the close. Recovery verifies both native and wrapper liveness and atomically moves the exact
predecessor's claims when binding the replacement launch. Unknown liveness or a competing recovery
refuses admission before the provider starts; holds and historical completion records remain intact.
This requires Agent Store v0.6.0 or newer, installed by `tj install`. Public Claude resume also transfers
the selected conversation's claims: its startup hook must prove that the replacement is the only live
native process for that conversation and that the previous wrapper has stopped.
See [Agent's recovery details](https://github.com/tj-agents/agent#lifecycle).

A merged PR never completes a session. Every two minutes the wrapper checks a linked item's PRs and
records a feed notification when all have merged. The item and sorted PR URLs identify the event durably,
so later polls, feed reads, wrapper restarts and launches of the same session do not repeat it. A newly
merged PR set gets a new event.

### Runtime updates

The [plugin update flow](docs/plugin-updates.md) owns marketplace checks, installation and reloads in
running sessions. Separately, the wrapper detects a replacement `tj` executable. Once the new file has
settled and runs, Claude and input have been idle for 30 seconds, and no background shell remains, it
hands over to the new binary and resumes the same conversation. The original options remain; the prompt
and earlier session choice are removed. Completion already awaiting closeout prevents that restart.
Repository preparation must pass for the resume command before the running agent is stopped. If handover
or successor startup then fails, the launch stays open for `tj recover --open` after the failure is repaired.

Linux replaces the wrapper in place. Windows keeps the old wrapper as the tab's root while its successor
runs. Launch identity and the conversation remain. Without a bound native session, the current agent
continues and the wrapper reports the missing binding. A failed resume leaves the reason visible. Codex
currently takes a replacement binary on its next start.

## `tj message` and `tj feed`: messaging between sessions

```text
tj message <title|host:id> <text> [--request-id <id>]
tj feed                           show this session's unread feed messages, once, with how to reply
tj feed --wait [--timeout <secs>]  wait for a feed message (default 12 hours); exit 1 if none arrived
```

Both hosts use `tj message`; TJ owns the provider routing decision. Address an exact session handle or a
title resolving to one registered session that has not ended. Claude-to-Claude messages use the sending
Claude session's native adapter; messages involving Codex, or sent from a plain terminal, use the feed.
Native routing requires Agent's adapter to be active in a Claude host with the supported mod API
(Claude Code 2.1.306 or newer). It preserves the source session identity and native delivery controls.

| Receipt | Meaning |
| --- | --- |
| Native `submitted` | The source provider accepted or queued the send; recipient approval, rendering and reading are not proved. |
| Feed `stored` | The record was appended to the recipient's feed; no wake or reading is proved. |
| Native `not-delivered`, `unknown`, or missing receipt | Success is unverified; TJ does not switch transports or automatically resend. |

A held, refused or uncertain native send never falls back to the feed. Reuse `--request-id <id>` only for
the exact same message and recipient: it preserves that operation's identity when inspecting or retrying,
instead of creating a new send. A claimed native request is not resent after an adapter restart. Automatic
final-result delivery uses the same routing and a stable identity for its session, launch and turn.

<!-- Parent reconciliation required before publication: verify the source Claude mod is installed and
loaded on supported hosts and complete real native acceptance on Windows and Linux. The bridge maps
native isDelivered=true to submitted only; keep recipient approval/read claims out of this contract.
Confirm the final provider minimum and user-facing errors after integration. -->

The feed transport stores messages in `<cache>/feed/<host>-<id>.jsonl`, with `<host>-<id>.read` holding the byte
offset already shown. Writers use an OS file lock and flush complete records; a torn last line is separated
before the next append. On Unix the folder and feeds are readable only by you. Unreadable lines are
reported and passed over. Sender host, handle and title stay with the message and its reply command.

Feed delivery does not submit user input or start a model turn. A session reads `tj feed` when useful.
When awaiting a result through the feed, it can run `tj feed --wait` through its host's background-command
tool so the result returns through that tool's native completion. Native message delivery follows the
provider's own behavior. See [agent:message](https://github.com/tj-agents/agent/blob/main/.agents/agent/utility/message/SKILL.md)
for the agent workflow.

## Configuration

| Variable | Default | Meaning |
| --- | --- | --- |
| `TJ_F_ROOT` | `~/projects`, then `~/source/repos` | where `f` searches |
| `TJ_CACHE_DIR` | OS cache folder + `/tj` | session search indexes, published plugin updates, wrapper update checks and session feeds |
| `TJ_STATE_DIR` | OS local application data folder + `/tj` | durable completion records under `completion/`, native operation requests and receipts under `native/`, and message route bindings under `messages/` |
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
| `src/run/` | internal managed supervision (`tj _session`), lifecycle commands, update handling and `tj closeout` |
| `src/completion.rs`, `src/completion/` | completion state and durable result receipts |
| `src/message.rs`, `src/feed.rs` | `tj message`, `tj feed` and durable notification identity |
| `src/process/agent.rs` and `src/process/agent/` | the agent's job or process group, per-host stop, tree kill |
| `src/f.rs` | folder walk, match scoring, picker |
| `src/cr/parse.rs` | reading Claude and Codex transcripts into searchable text and turns |
| `src/cr/render.rs` | the Tab preview pane |
| `src/cr/index.rs` | the incremental on-disk index |
| `src/cr/scope.rs` | repo and worktree scoping |
| `src/init.rs` | `tj init`, which prints the functions from `linux/` and `windows/` |

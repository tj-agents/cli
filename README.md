# tj

Fast shell tools for agentic programming, on Windows and Linux. It's a single Rust binary, `tj`,
with two everyday commands:

| Command | What it does |
| --- | --- |
| `cr` | Search your Claude Code or Codex sessions by **anything said in them**, preview how each one ended, and press Enter to resume it in the folder it was started from. |
| `f` | Fuzzy-jump to a project folder. `f core` goes straight to the best match; plain `f` opens a picker. |

## Install

You need [fzf](https://github.com/junegunn/fzf) and either a Rust toolchain (to build) or the
[GitHub CLI](https://cli.github.com) (to download a prebuilt release).

**Linux**

```bash
git clone https://github.com/tj-agents/cli ~/projects/tj-agents/cli
~/projects/tj-agents/cli/linux/install.sh
```

**Windows** (PowerShell, no administrator rights needed)

```powershell
git clone https://github.com/tj-agents/cli $HOME\source\repos\tj-agents\cli
& $HOME\source\repos\tj-agents\cli\windows\install.ps1
```

Then open a new terminal.

You can run the installer as often as you like. It updates whatever is out of date and reports `ok`
for everything else, so re-running it after a `git pull` is how you upgrade. It:

1. builds `tj` (or downloads the latest release) and puts it in `~/.local/bin` on Linux or
   `%LOCALAPPDATA%\Programs\tj` on Windows, adding that folder to your user PATH on Windows;
2. checks for fzf. On Windows it installs fzf with winget if it's missing; on Linux it tells you
   the package to install;
3. adds one marked block to each shell startup file it finds (bash, zsh, fish, PowerShell):

   ```bash
   # >>> tj-agents/cli >>>
   command -v tj >/dev/null 2>&1 && eval "$(tj init bash)"
   # <<< tj-agents/cli <<<
   ```

| | Linux | Windows |
| --- | --- | --- |
| Check what's set up, change nothing | `linux/install.sh --check` | `windows\install.ps1 -Check` |
| Remove everything it added | `linux/install.sh --uninstall` | `windows\install.ps1 -Uninstall` |

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

## Configuration

| Variable | Default | Meaning |
| --- | --- | --- |
| `TJ_F_ROOT` | `~/projects`, then `~/source/repos` | where `f` searches |
| `TJ_CACHE_DIR` | OS cache folder + `/tj` | where `cr` keeps its index |
| `TJ_BIN_DIR` | `~/.local/bin` / `%LOCALAPPDATA%\Programs\tj` | where the installer puts `tj` |
| `CLAUDE_CONFIG_DIR` | `~/.claude` | Claude Code's config folder, as Claude Code itself reads it |
| `CODEX_HOME` | `~/.codex` | Codex's config folder, as Codex itself reads it |

## Development

```bash
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt
```

CI runs all three on Linux and Windows. It then runs each installer twice and fails if the second
run changes anything. To publish prebuilt binaries, push a tag such as `v0.2.0`.

| Path | Owns |
| --- | --- |
| `linux/` | the Linux installer and the bash/zsh/fish functions |
| `windows/` | the Windows installer and the PowerShell functions |
| `src/f.rs` | folder walk, match scoring, picker |
| `src/cr/parse.rs` | reading Claude and Codex transcripts into searchable text and turns |
| `src/cr/render.rs` | the Tab preview pane |
| `src/cr/index.rs` | the incremental on-disk index |
| `src/cr/scope.rs` | repo and worktree scoping |
| `src/init.rs` | `tj init`, which prints the functions from `linux/` and `windows/` |

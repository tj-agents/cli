# `tj recover` + `tj watch` — reopen the agent sessions a crash closed

Branch `Feature/SessionRecovery` in `~/source/repos/tj-agents/cli`. Delete this plan in the PR that
completes it.

## Outcome

After a reboot, a terminal crash or anything else that kills the open agent sessions at once,
`tj recover` reopens every Claude Code and Codex session that was open, each in its original working
directory, as terminal tabs, on Windows and on Tommy's Arch machine (kitty), and in any other terminal.
`tj recover --list` runs the identical selection and prints exactly what `tj recover` would reopen and
the launch commands, changing nothing. Ignore the old `machine:recover-agents` skill and
`agent_recovery.py` in tj-agents/core entirely; do not port from them.

## Authorization

Tommy approved on 2026-10-09: Rust inside `tj`, a `tj watch` snapshotter that starts automatically at
login, kitty on Linux with support for any terminal. Authorized: implement, test, commit, push this
branch, open a PR with plain `gh pr create` (personal repo: no work item), get CI green, review, merge to
`main`. Not authorized: changes to tj-agents/core (retiring the old skill is a separate, unapproved
follow-up), publishing a release.

## Decisions

- **Liveness without agent hooks**, from state the agents already maintain:
  - Claude: `<claude_home>/sessions/<pid>.json` with `pid`, `sessionId`, `cwd`, `name`, `procStart`,
    `pidDomain`, `kind`, `status`, `updatedAt`. Observed 2026-10-09 (Claude Code 2.1.282, Windows):
    exactly the live interactive `claude.exe` processes had files; Desktop app and `auth login` did not.
    `procStart` on Windows is the process creation FILETIME as a decimal string; `pidDomain` is
    `win32:<host>`. Live = pid alive and its start time equals `procStart`. Ignore other `pidDomain`
    hosts and non-`interactive` kinds. Sibling `<pid>.<hash>.key` files are not session records.
  - Codex: `<codex_home>/thread-writer-locks/<thread-id>.lock` is byte-range locked while a thread is
    open (Windows `LockFile` refused on two live threads). The files persist after close, so only the
    lock means live. Codex records nothing that survives a crash: no exit marker in the rollout tail,
    no open flag in `state_5.sqlite` `threads`.
- **`tj watch` is required for Codex.** Single instance (lock file in tj's state dir), polls every ~10s,
  keeps one record per session: agent, id, cwd, name/title, first seen, last seen live, and how it
  ended. Atomic writes (temp + rename). It never deletes history needed by the selection rule.
- **Recover set = the last abrupt stop:** sessions not live now whose last-seen-live is the watcher's
  final tick before the most recent boot, or that vanished together (two or more in one tick) in the
  most recent burst. A session that vanished alone was closed deliberately and is never reopened.
  A dead-pid Claude registry file is extra positive proof (a clean exit deletes it). Skip sessions whose
  cwd no longer exists and report them. With no watcher state, fall back to dead-pid Claude registry
  files only and say Codex sessions cannot be determined.
- **Autostart, installed once by the installers** (idempotent, covered by `--check`/`-Check` and
  `--uninstall`/`-Uninstall`):
  - Windows: a shortcut in the user's Startup folder to a console-less watcher (for example a second
    `tj-watch` binary with `windows_subsystem = "windows"`), so no window flashes at login. No Run key,
    no Task Scheduler, no hidden PowerShell.
  - Linux: a `systemd --user` unit enabled with `systemctl --user enable --now`; where no user systemd
    exists (CI containers), fall back to `~/.config/autostart/tj-watch.desktop` or report and skip.
- **Reopening, one adapter per terminal, chosen automatically, overridable with `TJ_TERMINAL`:**
  - Windows Terminal: one `wt.exe -w new new-tab --title <t> -d <cwd> <agent> <args> ; new-tab ...`
    call. Escape `;` inside arguments as `\;`.
  - Inside tmux (`$TMUX`): `tmux new-window -c <cwd> -n <t> <agent> <args>` per session.
  - kitty: write a kitty session file (`new_tab <t>`, `cd <cwd>`, `launch <agent> <args>`) and run
    `kitty --detach --session <file>`; this needs no remote control. When `$KITTY_LISTEN_ON` is set,
    `kitty @ launch --type=tab --cwd <cwd> --tab-title <t>` may open them in the current window.
  - WezTerm: `wezterm cli spawn --cwd <cwd> -- <agent> <args>`.
  - Anything else: `$TERMINAL` or the first of a small known list (alacritty, foot, ghostty, konsole,
    gnome-terminal, xterm) launched once per session with its own cwd/exec flags; last resort prints the
    commands.
- **Resume commands:** Claude `claude --resume <id>` in the cwd; Codex `codex resume <id>` in the cwd.
  Verify whether resume keeps the Claude session name (`-n`) and the Codex model/effort; pass them
  explicitly when it does not (Codex: from the rollout's session metadata).
- **AV-quiet rules (Tommy's Bitdefender flagged the old tool):** no WMI/CIM, no process enumeration, no
  reading other processes' command lines or memory, no UI Automation or SendKeys, no killing processes,
  no `cmd /c`, no `powershell -Command`/encoded commands, no hidden-window or detached script spawns.
  Pid checks open one known pid: Windows `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` +
  `GetProcessTimes`; Linux `/proc/<pid>/stat`. Also replace the profile wiring's `Invoke-Expression (& tj
  init pwsh | Out-String)` with dot-sourcing a script file the installer writes, keeping bash/zsh/fish
  behaviour equivalent.
- Follow `AGENTS.md`: everything works on Windows and Linux, paths go through `src/paths.rs`, installers
  stay idempotent, transcript/registry shapes get fixtures in `tests/fixtures/`.

## Evidence so far

- 2026-10-09: `windows\install.ps1` built and installed `tj 0.1.0` to
  `%LOCALAPPDATA%\Programs\tj`, added it to the user PATH and wired both PowerShell profiles; `tj cr
  --list` ran. Bitdefender did not quarantine or block it.

## Open questions (answer them while implementing; record answers here)

1. Does Claude Code prune dead-pid `sessions/*.json` on its next start?
2. What `procStart` format does Claude Code write on Linux? Until observed on Arch, treat an
   unparseable `procStart` as pid-alive-only.
3. Which lock does Codex take on Linux? `flock` and `fcntl` locks do not see each other there; read
   openai/codex's thread-writer-lock code and probe with the same kind.
4. Do `claude --resume` and `codex resume` keep name, model and effort?

## Next Steps

Lane L4: the design above is decided; what remains is implementation with local, test-caught choices.

1. Answer open questions 1, 3 and 4 (2 needs the Arch machine; leave it to Tommy's smoke test) and
   record the answers above.
2. Implement `src/live.rs` (Claude registry + Codex lock probe, per OS), `tj watch`, `tj recover`
   (selection, `--list`, terminal adapters), installer autostart steps and the profile-wiring change,
   with unit tests for the selection rule (reboot, burst, single close, missing cwd), command building
   per adapter (including `wt` `;` escaping), and registry fixtures. Update `README.md`.
3. `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test`; rerun
   `windows\install.ps1` twice (second run all `ok`) and exercise `tj recover --list` against the live
   machine.
4. Commit, push, open the PR, get CI green on both OSes, review, address findings, merge. Delete this
   plan in the final PR commit and add an "Arch smoke test" checklist (kitty tabs, systemd unit,
   question 2) to the PR description for Tommy.

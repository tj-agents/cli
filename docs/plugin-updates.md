# Plugin updates

The `tj-agents/marketplace` repository pins plugin revisions. Its generated host marketplaces
describe those pins to Claude Code and Codex. Agent owns native marketplace and plugin installation;
this CLI owns detection of a new marketplace commit and delivery to running sessions.

In a repository selecting a `@tj-agents` plugin, managed `tj claude` and `tj codex` conversations check the
marketplace at launch and while the session remains open. The wrapper sends a conditional request for the
`main` commit, comparing it with the first line of `<cache>/published/<host>`. The per-host checked timestamp
and lock coordinate tabs;
checks are spaced by two minutes, limiting the two hosts' normal combined rate to about 60 requests an
hour. A one-second network timeout keeps the check off the session's supervision loop. A changed
commit starts `tj plugins update --host <host>` in the background; output goes to
`<cache>/update/<host>.log`. The same commit is retried after ten minutes if convergence did not
publish it. A recorded updater PID and start time prevent overlapping updater processes for the same
host, including across wrapper exits. A failed updater is reported in that log with its retry policy.

Each updater runs inside the existing `tj_hook` helper with a five-minute lifetime and a ten-second
cleanup allowance. Timeout, observer shutdown and normal updater exit all terminate and reap remaining
owned descendants before publishing a matching cleanup receipt. Windows uses the helper's Job Object;
Linux uses the existing process-group and subreaper cleanup inside that dedicated helper. A timed-out
update remains a failure and can retry after the same ten-minute window. Missing, mismatched or unsettled
cleanup receipts persist `<cache>/update/<host>.unsettled`, report the reason in the update log and block
further updater launches across wrapper restarts. A dead updater PID alone does not prove cleanup.
Retries resume only when the matching receipt confirms that the owned process tree has settled; an
unverified tree requires explicit inspection and cleanup before its failure state can be cleared.

If the lightweight GitHub probe cannot connect, it hands verification to the existing native plugin
updater in the background. The host still starts. These unverified attempts share the ten-minute retry
window across tabs and wrapper restarts; a newly verified changed commit can start once the previous
updater exits.
Only a successful native convergence publishes an installed revision. The update log records a safe
transport category and the native updater's result, rather than a repeated terminal warning.

For Codex sessions using an existing shared daemon outside the managed wrapper, Agent starts a daemon-scoped
observer during installation or SessionStart. One observer holds a per-daemon lock and shares the
per-host check coordination with managed sessions; it watches the publication file and requests the same
app-server reload. It attaches only to an existing daemon and does not create a session or service.
It exits on daemon disconnect or when no threads remain loaded. If a thread arrives during shutdown,
the observer resumes or a subsequent start acquires ownership. An embedded `--no-daemon` session is
outside this observer's reach.

Agent's plugin update checks the configured marketplaces, fetches changed ones and brings the host's
installed plugins to their pins. It checks repository standards, updates the Agent command runtime and,
when the installed state changed successfully, calls `tj update --host <host> --revision <commit>`.
The publish file contains the revision and a unique stamp, so separate successful publications of the
same revision are still distinct events. `tj install` also publishes changes. An installation failure
does not publish a successful update.

Startup and plugin refresh check repository settings and instructions without rewriting them, in both
primary and linked checkouts. Drift reports the explicit `tj prepare --host <host> --cwd <path> --write`
command. Successful plugin activation can still report repository drift; that warning is separate from a
failed native installation. Explicit repository adoption and install reconciliation own configuration
changes. See [Agent's installation and scope rules](https://github.com/tj-agents/agent#install).

Each running wrapper watches its host's publication file. Claude waits until its turn and input are
idle, then sends a durable `reload_plugins` request through Agent's native adapter. It marks the
publication applied only after a matching completion receipt. The adapter invokes the native command
without editing or submitting the user's composer. Missing adapters, failed reloads and missing
receipts leave activation unverified. Adapter replacement can reconcile a reload once; message
requests are never replayed.

Managed Codex uses an embedded `--no-daemon` engine. The wrapper reports that plugin activation remains
unverified until a native restart; it cannot refresh that engine through an unrelated shared daemon.
The external daemon observer still requests `config/batchWrite` with `reloadUserConfig: true` for its
own daemon. That request alone does not prove each loaded thread refreshed successfully or replace
instructions delivered in earlier turns. Neither path types into or submits a draft.

The wrapper separately detects a replacement `tj` binary. It can resume an idle Claude session under
the new binary with the same conversation; Codex currently takes the new binary on its next start.

## Acceptance checks

- Merge a marketplace pin change with `tj claude` and `tj codex` sessions already open. Without a manual
  update, observe the installation and each host's publication. For Claude, keep an unsent draft,
  observe the native completion receipt when idle, and exercise changed behavior in the same
  conversation with the draft intact. Managed Codex must report activation as unverified until a
  native restart. For external daemon sessions, exercise changed behavior in a later turn instead of
  relying on an RPC acknowledgement or completion list.
- Start and resume a Codex session in a selecting repository. Confirm its SessionStart hooks finish
  within their timeouts and its repository standards are available.
- Exercise `tj claude`, `tj codex`, `tj plugins update`, `tj install`, `tj start`, `tj status`, `tj recover`,
  `tj prepare` and `tj session` from each host and a plain terminal on supported Windows and Linux
  environments; record the actual outcome of each invocation.
- Snapshot authored instructions and repository settings in primary and linked checkouts before startup
  and plugin refresh. Confirm they remain unchanged, including when drift is reported; exercise explicit
  `tj prepare --write` separately.
- After `tj install`, inspect both hosts' user settings and confirm no plugin is enabled globally
  except Codex's own built-in marketplaces.
- Time a successful no-op `tj plugins update` for each host, recording the exit status and any remaining
  work against the two-second target.

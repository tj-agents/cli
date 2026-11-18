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
publish it.

If the lightweight GitHub probe cannot connect, it hands verification to the existing native plugin
updater in the background. The host still starts. These unverified attempts share the ten-minute retry
window across tabs and wrapper restarts; a newly verified changed commit can still start immediately.
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
Codex refresh checks running engines before replacing cached plugin files. An embedded engine retains
paths to its loaded hook files, so refresh keeps those files and reports the affected sessions. Reloading a
shared daemon cannot authorize replacement of files still loaded by a separate embedded process.
The publish file contains the revision and a unique stamp, so separate successful publications of the
same revision are still distinct events. `tj install` also publishes changes. An installation failure
does not publish a successful update.

Startup and plugin refresh check repository settings and instructions without rewriting them, in both
primary and linked checkouts. Drift reports the explicit `tj prepare --host <host> --cwd <path> --write`
command. Successful plugin activation can still report repository drift; that warning is separate from a
failed native installation. Explicit repository adoption and install reconciliation own configuration
changes. See [Agent's installation and scope rules](https://github.com/tj-agents/agent#install).
Failed startup preparation stops before launching the provider or transferring recovery ownership;
advisory repository drift alone does not fail preparation.

Each running wrapper watches its host's publication file. Claude waits until its turn and input are
idle, then sends a durable reload request to Agent's in-process native adapter. The adapter invokes
Claude's `command.run` API without editing the composer. The wrapper acknowledges the request only after
the native command returns; a missing adapter or receipt leaves activation unverified. The receipt retains
native command output. It does not prove every plugin's changed behavior, which the acceptance checks below
verify separately. Messaging continues while a native reload waits to run.

Replacing Agent itself can retire the caller before it records the result. Its replacement waits for the
old adapter's queue lease, then makes at most one additional native reload request to confirm completion.
Native reload preserves drafts; message delivery is never repeated by this reconciliation. Receipts name
their adapter lease so a late retired caller cannot overwrite the replacement's result. A second lost
caller remains unverified rather than starting an endless reload loop.

Managed Codex uses an owned embedded engine, which the shared daemon cannot reload. Its wrapper leaves activation unverified, reports that a
native restart is required and retains the current process. It does not acknowledge a reload against an
unrelated daemon as an update to that window. Automatic adoption by an already-running embedded Codex
interface remains unsupported.

The daemon observer above uses `config/batchWrite` with `reloadUserConfig: true` for threads actually
loaded in its daemon. The server retains some session settings and does not rewrite instructions already
delivered in previous turns. A successful request alone does not prove that each thread refreshed
successfully. Codex's completion lists may require a restart to reflect changes.

The wrapper separately detects a replacement `tj` binary. It can resume an idle Claude session under
the new binary with the same conversation; Codex currently takes the new binary on its next start.

## Acceptance checks

- Merge a marketplace pin change with `tj claude` and `tj codex` sessions already open. Without a manual
  update, observe the installation, each host's publication and the changed plugin behavior in each
  same conversation. For Claude, preserve an unsent draft and its cursor through native reload, including
  when Agent replaces its own adapter. For Codex, exercise changed
  behavior in a later turn instead of relying on an RPC acknowledgement or completion list.
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

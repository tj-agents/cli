# Plugin updates

The `tj-agents/marketplace` repository pins plugin revisions. Its generated host marketplaces
describe those pins to Claude Code and Codex. Agent owns native marketplace and plugin installation;
this CLI owns detection of a new marketplace commit and delivery to running sessions.

In a repository selecting a `@tj-agents` plugin, `tj run` checks the marketplace at launch and while
the tab remains open. It sends a conditional request for the `main` commit, comparing it with the
first line of `<cache>/published/<host>`. The per-host checked timestamp and lock coordinate tabs;
checks are spaced by two minutes, limiting the two hosts' normal combined rate to about 60 requests an
hour. A one-second network timeout keeps the check off the session's supervision loop. A changed
commit starts `tj plugins update --host <host>` in the background; output goes to
`<cache>/update/<host>.log`. The same commit is retried after ten minutes if convergence did not
publish it.

For Codex sessions using an existing shared daemon outside `tj run`, Agent starts a daemon-scoped
observer during installation or SessionStart. One observer holds a per-daemon lock and shares the
per-host check coordination with `tj run`; it watches the publication file and requests the same
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

Each running wrapper watches its host's publication file. Claude waits until its turn and input are
idle, then types `/reload-plugins` and Enter into the shared console. Unsubmitted text in the console
is not visible to the wrapper and the command can be appended to it. Codex requests
`config/batchWrite` with `reloadUserConfig: true` over WebSocket through the shared app-server proxy. This reaches
sessions using that daemon; an embedded Codex session, such as one started with `--no-daemon`, is not
addressed by this request. The server
refreshes loaded threads' runtime configuration; it retains some session settings and does not rewrite
instructions already delivered in previous turns. A successful request alone does not prove that each
thread refreshed successfully. Codex's completion lists may require a restart to reflect changes.

The wrapper separately detects a replacement `tj` binary. It can resume an idle Claude session under
the new binary with the same conversation; Codex currently takes the new binary on its next start.

## Acceptance checks

- Merge a marketplace pin change with Claude and Codex `tj run` tabs already open. Without a manual
  update, observe the installation, each host's publication and the changed plugin behavior in each
  same conversation. For Claude, observe the reload command when idle; for Codex, exercise changed
  behavior in a later turn instead of relying on an RPC acknowledgement or completion list.
- Start and resume a Codex session in a selecting repository. Confirm its SessionStart hooks finish
  within their timeouts and its repository standards are available.
- Exercise `tj run`, `tj plugins update`, `tj install`, `tj start`, `tj status`, `tj recover`,
  `tj prepare` and `tj session` from each host and a plain terminal on supported Windows and Linux
  environments; record the actual outcome of each invocation.
- After `tj install`, inspect both hosts' user settings and confirm no plugin is enabled globally
  except Codex's own built-in marketplaces.
- Time a successful no-op `tj plugins update` for each host, recording the exit status and any remaining
  work against the two-second target.

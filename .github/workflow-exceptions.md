# Workflow exceptions

Each zizmor exception names one exact rule, workflow, line and column.
Job naming exceptions in `workflow-baseline.json` preserve published check contexts.

| Rule | Location | Reason |
| --- | --- | --- |
| `dangerous-triggers` | `release.yml:4:3` | Release qualification accepts successful own-repository push runs, independently verifies current main and both CI workflows in tj_cli.native_release, then builds that qualified SHA before publication. |
| `undocumented-permissions` | `release.yml:27:7` | Tommy forbids explanatory code comments. The exact permission grants the named qualification, status-reading or release-publication job its required API access. |
| `undocumented-permissions` | `release.yml:87:7` | Tommy forbids explanatory code comments. The exact permission grants the named qualification, status-reading or release-publication job its required API access. |
| `superfluous-actions` | `rust-ci.yml:27:15` | The SHA-pinned toolchain action reads the repository rust-toolchain.toml and installs its requested components on Windows and Linux; keep that selected setup. |

# cli

Read `README.md` first; it owns usage, install and the source layout.

- Every change must work on both Windows and Linux. Never hardcode a home-relative folder layout or a
  path separator: go through `src/paths.rs`, and compare paths with `paths::norm`/`paths::within`.
- `linux/install.sh` and `windows/install.ps1` must stay idempotent: a second run reports `ok` for every step, and
  CI fails if it doesn't. Shell wiring lives only inside the `# >>> tj-agents/cli >>>` block.
- Transcript formats belong to Claude Code and Codex and change without notice. Parse them as JSON,
  tolerate missing fields and torn last lines, and cover each new shape with a fixture in
  `tests/fixtures/`.
- Bump `INDEX_VERSION` in `src/cr/index.rs` whenever parsing or rendering changes what is stored.
- Before committing: `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test`.

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

<!-- BEGIN personal:rules sha256:f878befe2d5e1e69bc6e5cc2ed10c6e684ce894987ff4a9861c7cc2b802fe802 -->
# Personal rules

These rules govern every project that explicitly adopts `personal@tj-agents`. Adoption records the
project's personal scope; a home directory, author name or employer association never establishes it, and
employer projects stay outside this profile unless the user explicitly extends that scope. Before planning
or starting work, confirm the task is clear against each rule below.

## 1. Clean cut

Keep one current supported solution. When replacing behavior, remove the superseded names, aliases,
fallback implementations, adapters, configuration formats, compatibility branches and forwarding or
migration docs. Migrate all known callers, project declarations, installed registrations, generated
artifacts and live runtime entry points in the same cutover, and coordinate upgrades with the actual
collaborating developers. A commit that removes or renames a skill while a tracked source still names its
old `<package>:<name>` is blocked until every reference is migrated or deleted.

Verify the entry points people actually use on every supported platform, including Windows and Linux
launch paths where supported. Source edits, successful generation, passing unit tests and version commands
alone leave a migration unproven; check installed bindings and fresh runtime behavior. Record any
unverified entry point as incomplete, with its resolver and observable acceptance condition.

```text
Replace old-launch with launch:
1. Migrate the registered shell command, configuration and actual callers.
2. Delete the old entry and regenerate installed settings.
3. Invoke launch from a fresh supported Windows/Linux session and check its actual behavior.
Result: launch --version alone leaves this migration incomplete.
```

A backwards-compatibility exception exists only as an explicit user requirement. Surface it as a
deliberate policy decision, identify the affected supported behavior and reconcile this profile's scope
before implementing it, keeping the exception visible rather than inside a fallback or adapter.

## 2. Proportionate implementation

Solve the demonstrated problem with the smallest suitable implementation. Add abstractions, dependencies,
configuration switches, duplicate paths or process machinery only for a concrete current need. Prefer the
capabilities the project already selects. Verify the behavior and meaningful failure paths in proportion
to the change, with tests that prove behavior rather than restate the implementation.

## 3. Plans

Track work as a GitHub issue or project item. A plan committed under `plans/` is only the detailed
implementation plan behind one such item and links to it; working plans stay machine-local.

This policy is authored once here. A selecting project receives it at SessionStart, and adoption generates
its marked native instruction block from this body and checks it for drift. A selected plugin or an
`always` tier alone is not proof of loading.
<!-- END personal:rules -->

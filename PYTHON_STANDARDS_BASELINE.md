# Python standards baseline

Existing violations at `a661c0e0349ad34f936868dfcce20ef20a29708b` are baselined by exact authored file and rule. New files receive all enabled checks. Remove an exception when repairing its source. The tool configuration owns enforcement; this document records the rollout evidence and rationale.

## Ruff

Reason: preserve existing implementations during the standards rollout. Each row records the existing count and a representative required repair. No wildcard path or whole-rule ignores were added.

| File | Rule | Existing count | Required repair |
| --- | --- | ---: | --- |
| `packages/tj-cli/src/tj_cli/__init__.py` | `F822` | 2 | Undefined name `paths` in `__all__` |
| `packages/tj-cli/src/tj_cli/clipboard/_windows.py` | `ANN401` | 9 | Dynamically typed expressions (typing.Any) are disallowed in `user32` |
| `packages/tj-cli/src/tj_cli/native_install.py` | `TRY300` | 2 | Consider moving this statement to an `else` block |
| `packages/tj-cli/tests/clipboard/test_guard.py` | `PLC2701` | 3 | Private name import `_guard` from external module `tj_cli.clipboard` |
| `packages/tj-cli/tests/clipboard/test_posix.py` | `PLC2701` | 3 | Private name import `_posix` from external module `tj_cli.clipboard` |
| `packages/tj-cli/tests/clipboard/test_text.py` | `PLC2701` | 2 | Private name import `_text` from external module `tj_cli.clipboard` |
| `packages/tj-cli/tests/clipboard/test_windows_clipboard.py` | `PLC2701` | 2 | Private name import `_windows` from external module `tj_cli.clipboard` |
| `packages/tj-cli/tests/clipboard/test_windows_clipboard.py` | `PLR6301` | 4 | Method `DestroyWindow` could be a function, class method, or static method |
| `packages/tj-cli/tests/process/test_execution.py` | `PT011` | 5 | `pytest.raises(subprocess.CalledProcessError)` is too broad, set the `match` parameter or use a more specific exception |
| `packages/tj-cli/tests/test_handy_install.py` | `ANN401` | 2 | Dynamically typed expressions (typing.Any) are disallowed in `**kwargs` |

## Custom checker

Reason: preserve exact pre-existing custom-checker findings during the standards rollout. Each entry maps to one authored path and rule; repair the stated design issue and remove its exception. No generated or vendored path is included.

| File | Rule | Existing count | Required repair |
| --- | --- | ---: | --- |
| packages/tj-cli/src/tj_cli/clipboard/__init__.py | PY008 | 1 | Move private initialization behind a public module boundary. |
| packages/tj-cli/src/tj_cli/clipboard/_errors.py | PY011 | 1 | Introduce and use the package's shared exception root. |
| packages/tj-cli/src/tj_cli/clipboard/_guard.py | PY016 | 1 | Inject process state or confine the read to an entry-point main. |
| packages/tj-cli/src/tj_cli/clipboard/_posix.py | PY016 | 1 | Inject process state or confine the read to an entry-point main. |
| packages/tj-cli/src/tj_cli/handy_install.py | PY011 | 1 | Introduce and use the package's shared exception root. |
| packages/tj-cli/src/tj_cli/handy_install.py | PY016 | 1 | Inject process state or confine the read to an entry-point main. |
| packages/tj-cli/src/tj_cli/native_install.py | PY011 | 1 | Introduce and use the package's shared exception root. |
| packages/tj-cli/src/tj_cli/native_install.py | PY016 | 3 | Inject process state or confine the read to an entry-point main. |
| packages/tj-cli/src/tj_cli/processes.py | PY011 | 1 | Introduce and use the package's shared exception root. |
| packages/tj-cli/src/tj_cli/worktrunk_install.py | PY011 | 1 | Introduce and use the package's shared exception root. |
| packages/tj-cli/tests/clipboard/test_dispatch.py | PY014 | 1 | Add matching source coverage or relocate the integration test. |
| packages/tj-cli/tests/clipboard/test_guard.py | PY014 | 1 | Add matching source coverage or relocate the integration test. |
| packages/tj-cli/tests/clipboard/test_posix.py | PY014 | 1 | Add matching source coverage or relocate the integration test. |
| packages/tj-cli/tests/clipboard/test_text.py | PY014 | 1 | Add matching source coverage or relocate the integration test. |
| packages/tj-cli/tests/clipboard/test_windows_clipboard.py | PY014 | 1 | Add matching source coverage or relocate the integration test. |
| packages/tj-cli/tests/process/conftest.py | PY016 | 1 | Inject process state or confine the read to an entry-point main. |
| packages/tj-cli/tests/process/fixtures/background_child.py | PY016 | 2 | Inject process state or confine the read to an entry-point main. |
| packages/tj-cli/tests/process/fixtures/background_root.py | PY009 | 1 | Move import-time process I/O and environment reads into main or an explicit function. |
| packages/tj-cli/tests/process/fixtures/background_root.py | PY016 | 3 | Inject process state or confine the read to an entry-point main. |
| packages/tj-cli/tests/process/fixtures/batch_argv.py | PY009 | 1 | Move import-time process I/O and environment reads into main or an explicit function. |
| packages/tj-cli/tests/process/fixtures/batch_argv.py | PY016 | 2 | Inject process state or confine the read to an entry-point main. |
| packages/tj-cli/tests/process/test_execution.py | PY014 | 1 | Add matching source coverage or relocate the integration test. |
| packages/tj-cli/tests/process/test_execution.py | PY016 | 5 | Inject process state or confine the read to an entry-point main. |
| packages/tj-cli/tests/process/test_execution.py | PY017 | 8 | Inject a controllable clock. |
| packages/tj-cli/tests/process/test_windows.py | PY014 | 1 | Add matching source coverage or relocate the integration test. |
| packages/tj-cli/tests/process/test_windows.py | PY017 | 2 | Inject a controllable clock. |
| packages/tj-cli/tests/test_distribution_updates.py | PY014 | 1 | Add matching source coverage or relocate the integration test. |
| packages/tj-cli/tests/test_handy_install.py | PY016 | 1 | Inject process state or confine the read to an entry-point main. |
| packages/tj-cli/tests/test_native_install.py | PY016 | 1 | Inject process state or confine the read to an entry-point main. |
| tests/fixtures/dummy_agent.py | PY016 | 4 | Inject process state or confine the read to an entry-point main. |
| tests/fixtures/fake_codex.py | PY009 | 1 | Move import-time process I/O and environment reads into main or an explicit function. |
| tests/fixtures/fake_codex.py | PY016 | 9 | Inject process state or confine the read to an entry-point main. |
| tests/fixtures/fake_inbox.py | PY016 | 1 | Inject process state or confine the read to an entry-point main. |
| tests/test_cli_operations.py | PY014 | 1 | Add matching source coverage or relocate the integration test. |
| tests/test_cli_operations.py | PY017 | 2 | Inject a controllable clock. |

## Expanded Python scope

These exact entries cover unchanged legacy diagnostics exposed when the root Python scope expanded. Repair each diagnostic and remove its matching baseline entry.

### Ruff

| File | Rule | Existing count | Required repair |
| --- | --- | ---: | --- |
| .agents/cli/utility/clip/scripts/copy_draft.py | E501 | 13 | Repair the existing E501 diagnostic and remove this exact ignore. |
| .agents/cli/utility/clip/scripts/copy_draft.py | TID251 | 1 | Repair the existing TID251 diagnostic and remove this exact ignore. |
| .agents/cli/utility/clip/scripts/copy_draft.py | TRY300 | 1 | Repair the existing TRY300 diagnostic and remove this exact ignore. |
| .agents/tests/test_cli_runtime.py | E501 | 1 | Repair the existing E501 diagnostic and remove this exact ignore. |
| .agents/tests/test_cli_runtime.py | I001 | 1 | Repair the existing I001 diagnostic and remove this exact ignore. |
| .agents/tests/test_cli_runtime.py | TID251 | 1 | Repair the existing TID251 diagnostic and remove this exact ignore. |
| .agents/tests/test_copy_draft.py | E501 | 5 | Repair the existing E501 diagnostic and remove this exact ignore. |
| .agents/tests/test_copy_draft.py | TID251 | 1 | Repair the existing TID251 diagnostic and remove this exact ignore. |
| tests/fixtures/dummy_agent.py | E501 | 2 | Repair the existing E501 diagnostic and remove this exact ignore. |
| tests/fixtures/dummy_agent.py | PTH109 | 1 | Repair the existing PTH109 diagnostic and remove this exact ignore. |
| tests/fixtures/dummy_agent.py | PTH123 | 1 | Repair the existing PTH123 diagnostic and remove this exact ignore. |
| tests/fixtures/fake_codex.py | B905 | 1 | Repair the existing B905 diagnostic and remove this exact ignore. |
| tests/fixtures/fake_codex.py | E501 | 3 | Repair the existing E501 diagnostic and remove this exact ignore. |
| tests/fixtures/fake_codex.py | PTH123 | 1 | Repair the existing PTH123 diagnostic and remove this exact ignore. |
| tests/fixtures/fake_inbox.py | E501 | 1 | Repair the existing E501 diagnostic and remove this exact ignore. |
| tests/fixtures/fake_inbox.py | PLC2701 | 1 | Repair the existing PLC2701 diagnostic and remove this exact ignore. |
| tests/fixtures/fake_inbox.py | PLW1514 | 2 | Repair the existing PLW1514 diagnostic and remove this exact ignore. |
| tests/fixtures/fake_inbox.py | PTH123 | 3 | Repair the existing PTH123 diagnostic and remove this exact ignore. |
| tests/test_cli_operations.py | E402 | 1 | Repair the existing E402 diagnostic and remove this exact ignore. |
| tests/test_cli_operations.py | I001 | 1 | Repair the existing I001 diagnostic and remove this exact ignore. |
| tests/test_cli_operations.py | PT009 | 17 | Repair the existing PT009 diagnostic and remove this exact ignore. |
| tests/test_cli_operations.py | TID251 | 2 | Repair the existing TID251 diagnostic and remove this exact ignore. |

### Ruff format

| File | Existing count | Required repair |
| --- | ---: | --- |
| .agents/cli/utility/clip/scripts/copy_draft.py | 1 | Format this existing file and remove this exact exclusion. |
| .agents/tests/test_cli_runtime.py | 1 | Format this existing file and remove this exact exclusion. |
| .agents/tests/test_copy_draft.py | 1 | Format this existing file and remove this exact exclusion. |
| tests/fixtures/dummy_agent.py | 1 | Format this existing file and remove this exact exclusion. |
| tests/fixtures/fake_codex.py | 1 | Format this existing file and remove this exact exclusion. |

### Mypy

| Module | Rule | Existing count | Required repair |
| --- | --- | ---: | --- |
| dummy_agent | attr-defined | 1 | Repair the existing attr-defined diagnostic and remove this exact override. |
| fake_codex | type-arg | 3 | Repair the existing type-arg diagnostic and remove this exact override. |
| fake_inbox | attr-defined | 1 | Repair the existing attr-defined diagnostic and remove this exact override. |
| test_cli_operations | attr-defined | 1 | Repair the existing attr-defined diagnostic and remove this exact override. |
| test_cli_operations | dict-item | 1 | Repair the existing dict-item diagnostic and remove this exact override. |
| test_cli_operations | index | 1 | Repair the existing index diagnostic and remove this exact override. |
| test_cli_operations | no-untyped-call | 11 | Repair the existing no-untyped-call diagnostic and remove this exact override. |
| test_cli_operations | no-untyped-def | 14 | Repair the existing no-untyped-def diagnostic and remove this exact override. |
| test_cli_operations | union-attr | 7 | Repair the existing union-attr diagnostic and remove this exact override. |
| test_cli_runtime | arg-type | 1 | Repair the existing arg-type diagnostic and remove this exact override. |
| test_cli_runtime | no-untyped-def | 1 | Repair the existing no-untyped-def diagnostic and remove this exact override. |
| test_cli_runtime | union-attr | 2 | Repair the existing union-attr diagnostic and remove this exact override. |

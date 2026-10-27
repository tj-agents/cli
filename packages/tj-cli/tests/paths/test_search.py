import os
import shutil
from pathlib import Path

import pytest

from tj_cli.paths import prepend


@pytest.mark.parametrize(
    ("entries", "directory", "expected"),
    [
        (("/shell", "/tools", "/shell"), "/wrapper", ("/wrapper", "/shell", "/tools")),
        (("/tools", "/wrapper", "/tools"), "/wrapper", ("/wrapper", "/tools")),
        (("", "/tools", ""), "/wrapper", ("/wrapper", "", "/tools")),
        (
            ("/tools", "/Tools", "/tools/"),
            "/wrapper",
            ("/wrapper", "/tools", "/Tools", "/tools/"),
        ),
        ((), "/wrapper", ("/wrapper",)),
    ],
)
def test_prepend_preserves_literal_search_precedence(
    entries: tuple[str, ...], directory: str, expected: tuple[str, ...]
) -> None:
    assert prepend(iter(entries), directory) == expected


def test_prepend_is_stable_across_repeated_shell_launches() -> None:
    initial = prepend(("/shell", "/tools", "/shell"), "/wrapper")

    assert prepend(("/shell", *initial), "/wrapper") == initial


def test_prepend_preserves_executable_search_order(tmp_path: Path) -> None:
    first = tmp_path / "first"
    second = tmp_path / "second"
    first.mkdir()
    second.mkdir()
    name = "probe.exe" if os.name == "nt" else "probe"
    for directory in (first, second):
        executable = directory / name
        executable.write_bytes(b"")
        executable.chmod(0o755)

    path = os.pathsep.join(prepend((str(first), str(second), str(first)), str(second)))

    assert shutil.which(name, path=path) == str(second / name)


@pytest.mark.skipif(os.name == "nt", reason="POSIX symlink parent traversal")
def test_prepend_preserves_executable_after_symlink_parent_entry(
    tmp_path: Path,
) -> None:
    tools = tmp_path / "tools"
    child = tmp_path / "other" / "child"
    tools.mkdir()
    child.mkdir(parents=True)
    (tools / "link").symlink_to(child, target_is_directory=True)
    executable = tools / "probe"
    executable.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    executable.chmod(0o755)
    entries = (str(tools / "link" / ".."), str(tools))

    path = os.pathsep.join(prepend(entries, str(tmp_path / "wrapper")))

    assert shutil.which("probe", path=path) == str(executable)


@pytest.mark.skipif(os.name == "nt", reason="POSIX missing-parent traversal")
def test_prepend_preserves_executable_after_missing_parent_entry(
    tmp_path: Path,
) -> None:
    tools = tmp_path / "tools"
    tools.mkdir()
    executable = tools / "probe"
    executable.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    executable.chmod(0o755)
    entries = (str(tmp_path / "missing" / ".." / "tools"), str(tools))

    path = os.pathsep.join(prepend(entries, str(tmp_path / "wrapper")))

    assert shutil.which("probe", path=path) == str(executable)

from __future__ import annotations

import os
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

import tj_cli


@pytest.mark.parametrize("delivered", [False, True])
def test_console_probe_uses_its_copied_package(tmp_path: Path, delivered: bool) -> None:
    source = Path(tj_cli.__file__).parent
    if delivered:
        source = Path(__file__).resolve().parents[4] / "plugins/cli/python/tj_cli"
    package = tmp_path / "bundle/python/tj_cli"
    shutil.copytree(source, package)
    foreign = tmp_path / "foreign/tj_cli"
    foreign.mkdir(parents=True)
    (foreign / "__init__.py").write_text(
        "raise AssertionError('foreign package imported')\n", encoding="utf-8"
    )
    environment = {**os.environ, "PYTHONPATH": str(foreign.parent)}
    environment.pop("PYTHONHOME", None)
    entry = package / "_console_probe_entry.py"
    result = subprocess.run(
        [sys.executable, "-I", "-B", str(entry), "--help"],
        cwd=tmp_path,
        env=environment,
        capture_output=True,
        text=True,
        timeout=10,
    )
    assert result.returncode == 0, result.stderr
    assert "--frontend-pid" in result.stdout
    (package / "__init__.py").unlink()
    refused = subprocess.run(
        [sys.executable, "-I", "-B", str(entry), "--help"],
        cwd=tmp_path,
        env=environment,
        capture_output=True,
        text=True,
        timeout=10,
    )
    assert refused.returncode != 0
    assert "declared tj_cli import boundary is unavailable" in refused.stderr
    assert "foreign package imported" not in refused.stderr

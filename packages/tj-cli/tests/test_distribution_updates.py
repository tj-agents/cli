"""Local source edits must invalidate the wheel used by runtime generation."""

import shutil
import subprocess
from pathlib import Path


def test_source_edit_rebuilds_the_noneditable_distribution(tmp_path: Path) -> None:
    package = Path(__file__).resolve().parents[1]
    source = tmp_path / "package"
    source.mkdir()
    shutil.copyfile(package / "pyproject.toml", source / "pyproject.toml")
    shutil.copytree(
        package / "src",
        source / "src",
        ignore=shutil.ignore_patterns("__pycache__", "*.pyc"),
    )
    module = source / "src/tj_cli/cache_fixture.py"
    uv = shutil.which("uv")
    assert uv is not None
    command = [
        uv,
        "run",
        "--project",
        str(source),
        "--no-editable",
        "python",
        "-I",
        "-B",
        "-c",
        "from tj_cli.cache_fixture import VALUE; print(VALUE)",
    ]
    for value in ("first", "second"):
        module.write_text(f"VALUE = {value!r}\n", encoding="utf-8")
        result = subprocess.run(
            command, capture_output=True, text=True, timeout=120, check=True
        )
        assert result.stdout.strip() == value

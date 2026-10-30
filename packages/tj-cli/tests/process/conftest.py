import os
import shutil
import subprocess
from collections.abc import Iterator
from pathlib import Path

import pytest


@pytest.fixture(scope="session")
def native_launcher(tmp_path_factory: pytest.TempPathFactory) -> Path:
    root = Path(__file__).resolve().parents[4]
    subprocess.run(
        [
            "cargo",
            "build",
            "--locked",
            "--bin",
            "tj_hook",
            "--manifest-path",
            str(root / "Cargo.toml"),
        ],
        check=True,
        capture_output=True,
    )
    filename = "tj_hook.exe" if os.name == "nt" else "tj_hook"
    installed = tmp_path_factory.mktemp("native-launcher") / filename
    shutil.copy2(root / "target" / "debug" / filename, installed)
    return installed


@pytest.fixture(autouse=True)
def launcher_on_path(
    native_launcher: Path, monkeypatch: pytest.MonkeyPatch
) -> Iterator[None]:
    monkeypatch.setenv(
        "PATH", str(native_launcher.parent) + os.pathsep + os.environ["PATH"]
    )
    yield

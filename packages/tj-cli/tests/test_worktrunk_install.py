import hashlib
import io
import platform
import subprocess
import tarfile
import urllib.request
import zipfile
from pathlib import Path

import pytest

from tj_cli import tool_receipts, worktrunk_install


def test_a_download_with_the_wrong_digest_installs_nothing(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    monkeypatch.setattr(platform, "system", lambda: "Linux")
    monkeypatch.setattr(platform, "machine", lambda: "x86_64")
    monkeypatch.setattr(
        urllib.request, "urlopen", lambda url, timeout: io.BytesIO(b"tampered")
    )
    assert worktrunk_install.main(["--bin-dir", str(tmp_path)]) == 1
    assert "failed SHA-256 verification" in capsys.readouterr().err
    assert list(tmp_path.iterdir()) == []


def test_check_reports_a_missing_binary_without_downloading(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    def unexpected(*args: object, **kwargs: object) -> None:
        pytest.fail("check mode must not download")

    monkeypatch.setattr(urllib.request, "urlopen", unexpected)
    assert worktrunk_install.main(["--bin-dir", str(tmp_path), "--check"]) == 1
    assert "missing    git-wt" in capsys.readouterr().out


@pytest.mark.parametrize("system", ["Windows", "Linux"])
def test_install_writes_receipt_and_repairs_changed_binary_without_executing_it(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    system: str,
) -> None:
    monkeypatch.setattr(platform, "system", lambda: system)
    monkeypatch.setattr(platform, "machine", lambda: "x86_64")
    monkeypatch.setattr(
        subprocess, "run", lambda *a, **kw: pytest.fail("file-only probe")
    )
    archive_name, _ = worktrunk_install.ARCHIVES[system]
    contents = io.BytesIO()
    payload = b"fake git-wt executable"
    if system == "Windows":
        with zipfile.ZipFile(contents, "w") as zipped:
            zipped.writestr("git-wt.exe", payload)
    else:
        with tarfile.open(fileobj=contents, mode="w:xz") as tar:
            member = tarfile.TarInfo(f"{archive_name.removesuffix('.tar.xz')}/git-wt")
            member.size = len(payload)
            tar.addfile(member, io.BytesIO(payload))
    archive = contents.getvalue()
    monkeypatch.setitem(
        worktrunk_install.ARCHIVES,
        system,
        (archive_name, hashlib.sha256(archive).hexdigest()),
    )
    monkeypatch.setattr(
        urllib.request, "urlopen", lambda url, timeout: io.BytesIO(archive)
    )
    args = ["--bin-dir", str(tmp_path)]
    assert worktrunk_install.main(args) == 0
    target = worktrunk_install.executable(tmp_path)
    receipt = tool_receipts.read(tmp_path / worktrunk_install.RECEIPT)
    assert receipt is not None and receipt.version == worktrunk_install.VERSION
    before = {p.name: p.stat().st_mtime_ns for p in tmp_path.iterdir()}
    assert worktrunk_install.main(args) == 0
    assert before == {p.name: p.stat().st_mtime_ns for p in tmp_path.iterdir()}
    target.write_bytes(b"modified")
    assert worktrunk_install.pending(tmp_path) is not None
    assert worktrunk_install.main(args) == 0
    assert target.read_bytes() == payload
    assert worktrunk_install.pending(tmp_path) is None
    assert worktrunk_install.main([*args, "--uninstall"]) == 0
    assert not list(tmp_path.iterdir())


def test_explicit_directory_does_not_need_windows_default_environment(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(platform, "system", lambda: "Windows")
    for variable in ("LOCALAPPDATA", "TJ_BIN_DIR", "CI"):
        monkeypatch.delenv(variable, raising=False)
    assert worktrunk_install.main(["--bin-dir", str(tmp_path), "--check"]) == 1

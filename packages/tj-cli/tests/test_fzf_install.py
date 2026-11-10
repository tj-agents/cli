import hashlib
import io
import platform
import subprocess
import tarfile
import urllib.request
import zipfile
from pathlib import Path

import pytest

from tj_cli import fzf_install, tool_receipts


@pytest.mark.parametrize("system", ["Windows", "Linux"])
@pytest.mark.parametrize("machine", ["AMD64", "x86_64", "ARM64", "aarch64"])
def test_verified_install_is_idempotent_and_repairs_drift(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    system: str,
    machine: str,
) -> None:
    monkeypatch.setattr(platform, "system", lambda: system)
    monkeypatch.setattr(platform, "machine", lambda: machine)
    monkeypatch.setattr(
        subprocess, "run", lambda *a, **kw: pytest.fail("must not run binaries")
    )
    contents = io.BytesIO()
    payload = b"fake fzf executable"
    if system == "Windows":
        with zipfile.ZipFile(contents, "w") as zipped:
            zipped.writestr("fzf.exe", payload)
    else:
        with tarfile.open(fileobj=contents, mode="w:gz") as tar:
            member = tarfile.TarInfo("fzf")
            member.size = len(payload)
            tar.addfile(member, io.BytesIO(payload))
    archive = contents.getvalue()
    architecture = "amd64" if machine in {"AMD64", "x86_64"} else "arm64"
    archive_name, _ = fzf_install.ARCHIVES[system, architecture]
    monkeypatch.setitem(
        fzf_install.ARCHIVES,
        (system, architecture),
        (archive_name, hashlib.sha256(archive).hexdigest()),
    )
    downloads: list[str] = []

    def download(url: str, timeout: int) -> io.BytesIO:
        downloads.append(url)
        return io.BytesIO(archive)

    monkeypatch.setattr(urllib.request, "urlopen", download)
    args = ["--bin-dir", str(tmp_path)]
    assert fzf_install.main([*args, "--check"]) == 1
    assert not downloads
    assert fzf_install.main(args) == 0
    target = fzf_install.executable(tmp_path)
    assert target.read_bytes() == payload
    receipt = tool_receipts.read(tmp_path / fzf_install.RECEIPT)
    assert receipt is not None and receipt.version == fzf_install.VERSION
    before = {p.name: p.stat().st_mtime_ns for p in tmp_path.iterdir()}
    assert fzf_install.main(args) == 0
    assert fzf_install.main([*args, "--check"]) == 0
    assert before == {p.name: p.stat().st_mtime_ns for p in tmp_path.iterdir()}
    assert len(downloads) == 1
    assert downloads[0].endswith(archive_name)
    target.write_bytes(b"modified")
    assert fzf_install.pending(tmp_path) is not None
    assert fzf_install.main(args) == 0
    assert fzf_install.pending(tmp_path) is None
    assert len(downloads) == 2
    assert fzf_install.main([*args, "--uninstall"]) == 0
    assert not list(tmp_path.iterdir())


def test_bad_download_leaves_previous_installation_untouched(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(platform, "system", lambda: "Linux")
    monkeypatch.setattr(platform, "machine", lambda: "x86_64")
    target = tmp_path / "fzf"
    target.write_bytes(b"previous")
    monkeypatch.setattr(
        urllib.request, "urlopen", lambda url, timeout: io.BytesIO(b"bad")
    )
    assert fzf_install.main(["--bin-dir", str(tmp_path)]) == 1
    assert target.read_bytes() == b"previous"
    assert not (tmp_path / fzf_install.RECEIPT).exists()


def test_unsupported_platform_does_not_download(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(platform, "system", lambda: "Linux")
    monkeypatch.setattr(platform, "machine", lambda: "mips")
    monkeypatch.setattr(
        urllib.request, "urlopen", lambda *a, **kw: pytest.fail("unsupported")
    )
    assert fzf_install.main(["--bin-dir", str(tmp_path)]) == 1


def test_explicit_directory_does_not_need_windows_default_environment(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(platform, "system", lambda: "Windows")
    for variable in ("LOCALAPPDATA", "TJ_BIN_DIR", "CI"):
        monkeypatch.delenv(variable, raising=False)
    assert fzf_install.main(["--bin-dir", str(tmp_path), "--check"]) == 1

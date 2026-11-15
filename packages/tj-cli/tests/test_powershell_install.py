import hashlib
import io
import tarfile
import urllib.request
import zipfile
from pathlib import Path

import pytest

from tj_cli import host_os, powershell_install


@pytest.mark.parametrize("system", ["Windows", "Linux"])
def test_install_checks_entire_tree_and_repairs_corruption(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, system: str
) -> None:
    monkeypatch.setattr(host_os, "system", lambda: system)
    monkeypatch.setattr(host_os, "machine", lambda: "x86_64")
    payload = io.BytesIO()
    filename = "pwsh.exe" if system == "Windows" else "pwsh"
    if system == "Windows":
        with zipfile.ZipFile(payload, "w") as archive:
            archive.writestr(filename, b"executable")
            archive.writestr("lib/data", b"library")
        name = "test.zip"
    else:
        with tarfile.open(fileobj=payload, mode="w:gz") as archive:
            for path, data in [(filename, b"executable"), ("lib/data", b"library")]:
                entry = tarfile.TarInfo(path)
                entry.size = len(data)
                archive.addfile(entry, io.BytesIO(data))
        name = "test.tar.gz"
    archive_data = payload.getvalue()
    monkeypatch.setitem(
        powershell_install.ASSETS,
        (system, "x64"),
        (name, hashlib.sha256(archive_data).hexdigest()),
    )
    monkeypatch.setattr(
        urllib.request, "urlopen", lambda url, timeout: io.BytesIO(archive_data)
    )
    assert powershell_install.pending(tmp_path) is not None
    binary = powershell_install.ensure(tmp_path)
    assert binary.read_bytes() == b"executable"
    assert powershell_install.pending(tmp_path) is None
    before = binary.stat().st_mtime_ns
    assert powershell_install.ensure(tmp_path) == binary
    assert binary.stat().st_mtime_ns == before
    (binary.parent / "lib" / "data").write_bytes(b"bad")
    assert powershell_install.pending(tmp_path) is not None
    assert powershell_install.ensure(tmp_path) == binary
    assert (binary.parent / "lib" / "data").read_bytes() == b"library"
    assert powershell_install.pending(tmp_path) is None


def test_rejects_checksum_mismatch_without_installing(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(host_os, "system", lambda: "Windows")
    monkeypatch.setattr(host_os, "machine", lambda: "AMD64")
    monkeypatch.setattr(
        urllib.request, "urlopen", lambda url, timeout: io.BytesIO(b"tampered")
    )
    with pytest.raises(powershell_install.PowerShellInstallError, match="SHA-256"):
        powershell_install.ensure(tmp_path)
    assert not list(tmp_path.iterdir())


def test_rejects_archive_traversal(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(host_os, "system", lambda: "Windows")
    monkeypatch.setattr(host_os, "machine", lambda: "AMD64")
    content = io.BytesIO()
    with zipfile.ZipFile(content, "w") as archive:
        archive.writestr("../escape", b"bad")
    data = content.getvalue()
    monkeypatch.setitem(
        powershell_install.ASSETS,
        ("Windows", "x64"),
        ("test.zip", hashlib.sha256(data).hexdigest()),
    )
    monkeypatch.setattr(
        urllib.request, "urlopen", lambda url, timeout: io.BytesIO(data)
    )
    with pytest.raises(powershell_install.PowerShellInstallError, match="unsafe"):
        powershell_install.ensure(tmp_path)
    assert not (tmp_path / "escape").exists()

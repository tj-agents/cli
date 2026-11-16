import hashlib
import io
import shutil
import tarfile
import urllib.request
import zipfile
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from threading import Event

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


def test_preserves_unowned_files_on_repair(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(host_os, "system", lambda: "Windows")
    monkeypatch.setattr(host_os, "machine", lambda: "AMD64")
    content = io.BytesIO()
    with zipfile.ZipFile(content, "w") as archive:
        archive.writestr("pwsh.exe", b"binary")
    data = content.getvalue()
    monkeypatch.setitem(
        powershell_install.ASSETS,
        ("Windows", "x64"),
        ("test.zip", hashlib.sha256(data).hexdigest()),
    )
    monkeypatch.setattr(
        urllib.request, "urlopen", lambda url, timeout: io.BytesIO(data)
    )
    binary = powershell_install.ensure(tmp_path)
    unrelated = binary.parent / "personal.txt"
    unrelated.write_text("keep", encoding="utf-8")
    binary.write_bytes(b"changed")
    with pytest.raises(powershell_install.PowerShellInstallError, match="unowned"):
        powershell_install.ensure(tmp_path)
    assert unrelated.read_text(encoding="utf-8") == "keep"
    assert binary.read_bytes() == b"changed"


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
    assert not powershell_install.executable(tmp_path).exists()


def test_recovers_interrupted_replacement_without_downloading(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(host_os, "system", lambda: "Windows")
    monkeypatch.setattr(host_os, "machine", lambda: "AMD64")
    content = io.BytesIO()
    with zipfile.ZipFile(content, "w") as archive:
        archive.writestr("pwsh.exe", b"binary")
    data = content.getvalue()
    monkeypatch.setitem(
        powershell_install.ASSETS,
        ("Windows", "x64"),
        ("test.zip", hashlib.sha256(data).hexdigest()),
    )
    monkeypatch.setattr(
        urllib.request, "urlopen", lambda url, timeout: io.BytesIO(data)
    )
    binary = powershell_install.ensure(tmp_path)
    binary.parent.rename(binary.parent.parent / ".previous")

    def unexpected(*args: object, **kwargs: object) -> None:
        pytest.fail("recovery should reuse the prior runtime")

    monkeypatch.setattr(urllib.request, "urlopen", unexpected)
    assert powershell_install.ensure(tmp_path).read_bytes() == b"binary"
    assert powershell_install.pending(tmp_path) is None
    assert not (binary.parent.parent / ".previous").exists()


def test_concurrent_installers_download_once(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(host_os, "system", lambda: "Windows")
    monkeypatch.setattr(host_os, "machine", lambda: "AMD64")
    content = io.BytesIO()
    with zipfile.ZipFile(content, "w") as archive:
        archive.writestr("pwsh.exe", b"binary")
    data = content.getvalue()
    monkeypatch.setitem(
        powershell_install.ASSETS,
        ("Windows", "x64"),
        ("test.zip", hashlib.sha256(data).hexdigest()),
    )
    entered, release = Event(), Event()
    calls: list[str] = []

    def download(url: str, timeout: int) -> io.BytesIO:
        calls.append(url)
        entered.set()
        assert release.wait(10)
        return io.BytesIO(data)

    monkeypatch.setattr(urllib.request, "urlopen", download)
    with ThreadPoolExecutor(max_workers=2) as pool:
        first = pool.submit(powershell_install.ensure, tmp_path)
        assert entered.wait(10)
        second = pool.submit(powershell_install.ensure, tmp_path)
        release.set()
        assert first.result() == second.result()
    assert len(calls) == 1
    assert powershell_install.pending(tmp_path) is None


def test_finishes_interrupted_cleanup_without_replacing_current_runtime(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(host_os, "system", lambda: "Windows")
    monkeypatch.setattr(host_os, "machine", lambda: "AMD64")
    content = io.BytesIO()
    with zipfile.ZipFile(content, "w") as archive:
        archive.writestr("pwsh.exe", b"binary")
    data = content.getvalue()
    monkeypatch.setitem(
        powershell_install.ASSETS,
        ("Windows", "x64"),
        ("test.zip", hashlib.sha256(data).hexdigest()),
    )
    monkeypatch.setattr(
        urllib.request, "urlopen", lambda url, timeout: io.BytesIO(data)
    )
    binary = powershell_install.ensure(tmp_path)
    previous = binary.parent.parent / ".previous"
    shutil.copytree(binary.parent, previous)
    before = binary.stat().st_mtime_ns
    assert powershell_install.pending(tmp_path) is not None
    assert powershell_install.ensure(tmp_path) == binary
    assert binary.stat().st_mtime_ns == before
    assert not previous.exists()
    assert powershell_install.pending(tmp_path) is None


def test_recovers_previous_runtime_if_replacement_is_incomplete(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(host_os, "system", lambda: "Windows")
    monkeypatch.setattr(host_os, "machine", lambda: "AMD64")
    content = io.BytesIO()
    with zipfile.ZipFile(content, "w") as archive:
        archive.writestr("pwsh.exe", b"binary")
    data = content.getvalue()
    monkeypatch.setitem(
        powershell_install.ASSETS,
        ("Windows", "x64"),
        ("test.zip", hashlib.sha256(data).hexdigest()),
    )
    monkeypatch.setattr(
        urllib.request, "urlopen", lambda url, timeout: io.BytesIO(data)
    )
    binary = powershell_install.ensure(tmp_path)
    previous = binary.parent.parent / ".previous"
    shutil.copytree(binary.parent, previous)
    binary.write_bytes(b"damaged")
    assert powershell_install.ensure(tmp_path).read_bytes() == b"binary"
    assert not previous.exists()
    assert powershell_install.pending(tmp_path) is None


def test_recovers_cleanup_after_previous_receipt_was_removed(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(host_os, "system", lambda: "Windows")
    monkeypatch.setattr(host_os, "machine", lambda: "AMD64")
    content = io.BytesIO()
    with zipfile.ZipFile(content, "w") as archive:
        archive.writestr("pwsh.exe", b"binary")
        archive.writestr("lib/data", b"library")
    data = content.getvalue()
    monkeypatch.setitem(
        powershell_install.ASSETS,
        ("Windows", "x64"),
        ("test.zip", hashlib.sha256(data).hexdigest()),
    )
    monkeypatch.setattr(
        urllib.request, "urlopen", lambda url, timeout: io.BytesIO(data)
    )
    binary = powershell_install.ensure(tmp_path)
    previous = binary.parent.parent / ".previous"
    shutil.copytree(binary.parent, previous)
    remove_tree = shutil.rmtree

    def interrupted(path: Path, *args: object, **kwargs: object) -> None:
        if Path(path) == previous:
            (previous / powershell_install.RECEIPT).unlink()
            (previous / "lib" / "data").unlink()
            raise OSError("simulated crash during cleanup")
        remove_tree(path)

    monkeypatch.setattr(shutil, "rmtree", interrupted)
    with pytest.raises(OSError, match="simulated crash"):
        powershell_install.ensure(tmp_path)
    monkeypatch.setattr(shutil, "rmtree", remove_tree)
    assert powershell_install.pending(tmp_path) is not None
    assert powershell_install.ensure(tmp_path) == binary
    assert binary.read_bytes() == b"binary"
    assert not previous.exists()
    assert powershell_install.pending(tmp_path) is None


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

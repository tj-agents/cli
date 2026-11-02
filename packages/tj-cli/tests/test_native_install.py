import hashlib
import json
import os
import platform as platform_module
import shutil
import subprocess
import zipfile
from pathlib import Path
from unittest import mock

import pytest

from tj_cli import native_install


def _archive(path: Path, entries: dict[str, bytes]) -> str:
    with zipfile.ZipFile(path, "w") as contents:
        for name, value in entries.items():
            contents.writestr(name, value)
    return hashlib.sha256(path.read_bytes()).hexdigest()


def _release(digest: str, revision: str = "a" * 40) -> dict[str, object]:
    return {
        "target_commitish": revision,
        "tag_name": f"main-{revision}",
        "draft": False,
        "assets": [{"name": "native.zip", "id": 7, "digest": f"sha256:{digest}"}],
    }


def _fake_release(
    monkeypatch: pytest.MonkeyPatch,
    archive: Path,
    digest: str,
    revision: str = "a" * 40,
) -> None:
    monkeypatch.setattr(native_install, "_asset_name", lambda: "native.zip")
    monkeypatch.setattr(
        shutil,
        "which",
        lambda command: "/gh" if command == "gh" else None,
    )
    monkeypatch.setattr(
        native_install, "_gh_json", lambda endpoint: _release(digest, revision)
    )
    monkeypatch.setattr(
        native_install,
        "_download_asset",
        lambda asset_id, destination: shutil.copyfile(archive, destination),
    )


def test_missing_native_reports_the_pair(tmp_path: Path) -> None:
    assert native_install.missing_native(tmp_path) == native_install._binary_names()


def test_relative_bin_override_is_bound_before_a_host_changes_directory(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv("TJ_BIN_DIR", "native bin")
    assert native_install.default_bin_dir() == tmp_path.resolve() / "native bin"


def test_release_requests_pin_github_even_with_a_host_override(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("GH_HOST", "other.example")
    with mock.patch.object(
        native_install, "_run", return_value=mock.Mock(stdout="{}")
    ) as run:
        native_install._gh_json("repos/tj-agents/cli/releases/latest")
    assert run.call_args.args[0][:5] == [
        "gh",
        "api",
        "--hostname",
        "github.com",
        "repos/tj-agents/cli/releases/latest",
    ]
    with mock.patch.object(native_install, "_run_download") as download:
        native_install._download_asset(7, tmp_path / "asset")
    assert download.call_args.args[0][2:4] == ["--hostname", "github.com"]


def test_invalid_digest_leaves_existing_pair_untouched(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    archive = tmp_path / "native.zip"
    names = native_install._binary_names()
    digest = _archive(archive, {names[0]: b"new-tj", names[1]: b"new-hook"})
    _fake_release(monkeypatch, archive, digest)
    assert native_install.ensure_native(tmp_path / "bin") == names
    before = {name: (tmp_path / "bin" / name).read_bytes() for name in names}
    _fake_release(monkeypatch, archive, "0" * 64)
    with pytest.raises(native_install.NativeInstallError, match="SHA-256"):
        native_install.ensure_native(tmp_path / "bin")
    assert {name: (tmp_path / "bin" / name).read_bytes() for name in names} == before


def test_archive_traversal_is_refused_before_install(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    archive = tmp_path / "native.zip"
    names = native_install._binary_names()
    digest = _archive(
        archive, {names[0]: b"tj", names[1]: b"hook", "../outside": b"no"}
    )
    _fake_release(monkeypatch, archive, digest)
    with pytest.raises(native_install.NativeInstallError, match="unexpected"):
        native_install.ensure_native(tmp_path / "bin")
    assert not (tmp_path / "outside").exists()


def test_same_verified_release_does_not_rewrite_mtimes(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    archive = tmp_path / "native.zip"
    names = native_install._binary_names()
    digest = _archive(archive, {names[0]: b"tj", names[1]: b"hook"})
    _fake_release(monkeypatch, archive, digest)
    directory = tmp_path / "bin"
    assert native_install.ensure_native(directory) == names
    before = {name: (directory / name).stat().st_mtime_ns for name in names}

    def unexpected_download(asset_id: int, destination: Path) -> None:
        pytest.fail("an intact installed release must not download again")

    monkeypatch.setattr(native_install, "_download_asset", unexpected_download)
    assert native_install.ensure_native(directory) == ()
    assert {name: (directory / name).stat().st_mtime_ns for name in names} == before


def test_exact_revision_mismatch_rejects_release(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    archive = tmp_path / "native.zip"
    names = native_install._binary_names()
    digest = _archive(archive, {names[0]: b"tj", names[1]: b"hook"})
    _fake_release(monkeypatch, archive, digest, "a" * 40)
    with pytest.raises(native_install.NativeInstallError, match="does not match"):
        native_install.ensure_native(tmp_path / "bin", revision="b" * 40)


def test_missing_installed_binary_is_repaired(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    archive = tmp_path / "native.zip"
    names = native_install._binary_names()
    digest = _archive(archive, {names[0]: b"tj", names[1]: b"hook"})
    _fake_release(monkeypatch, archive, digest)
    directory = tmp_path / "bin"
    native_install.ensure_native(directory)
    (directory / names[1]).unlink()
    assert native_install.ensure_native(directory) == names
    assert not native_install.missing_native(directory)


@pytest.mark.parametrize("reserved", ["binary", "receipt"])
def test_directory_collision_preserves_user_files(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, reserved: str
) -> None:
    archive = tmp_path / "native.zip"
    names = native_install._binary_names()
    digest = _archive(archive, {names[0]: b"tj", names[1]: b"hook"})
    _fake_release(monkeypatch, archive, digest)
    directory = tmp_path / "bin"
    collision = directory / (
        names[0] if reserved == "binary" else native_install._RECEIPT
    )
    collision.mkdir(parents=True)
    important = collision / "user-data"
    important.write_bytes(b"preserve")
    with pytest.raises(native_install.NativeInstallError, match="regular file"):
        native_install.ensure_native(directory)
    assert important.read_bytes() == b"preserve"
    assert not (directory / names[1]).exists()


def test_uninstall_does_not_claim_unreceipted_binaries_are_absent(
    tmp_path: Path,
) -> None:
    (tmp_path / native_install._binary_names()[0]).write_bytes(b"previous install")
    with pytest.raises(
        native_install.NativeInstallError, match="run install before uninstall"
    ):
        native_install.uninstall_native(tmp_path)


@pytest.mark.skipif(os.name != "nt", reason="Windows executable sharing semantics")
def test_running_windows_binary_defers_upgrade_without_changing_the_pair(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    archive = tmp_path / "native.zip"
    names = native_install._binary_names()
    directory = tmp_path / "bin"
    directory.mkdir()
    executable = directory / names[0]
    shutil.copyfile(Path(os.environ["SYSTEMROOT"]) / "System32/ping.exe", executable)
    (directory / names[1]).write_bytes(b"old-hook")
    before = executable.read_bytes()
    digest = _archive(archive, {names[0]: b"new-tj", names[1]: b"new-hook"})
    _fake_release(monkeypatch, archive, digest)
    child = subprocess.Popen(
        [str(executable), "-n", "30", "127.0.0.1"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
    )
    try:
        assert child.poll() is None
        with pytest.raises(native_install.NativeInstallError, match="in use"):
            native_install.ensure_native(directory)
        assert executable.read_bytes() == before
        assert (directory / names[1]).read_bytes() == b"old-hook"
        assert not (directory / native_install._RECEIPT).exists()
    finally:
        child.terminate()
        child.wait(timeout=10)


def test_failed_pair_replacement_restores_old_pair(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    directory = tmp_path / "bin"
    directory.mkdir()
    names = native_install._binary_names()
    for name in names:
        (directory / name).write_bytes(f"old-{name}".encode())
    (directory / native_install._RECEIPT).write_text(
        json.dumps({"identity": "b" * 40, "hashes": {}}), encoding="utf-8"
    )
    original_replace = Path.replace

    def failing_replace(source: Path, destination: Path) -> Path:
        if source.name == names[1] and source.parent.name == "new":
            raise OSError("injected replacement failure")
        return original_replace(source, destination)

    monkeypatch.setattr(Path, "replace", failing_replace)
    with pytest.raises(
        native_install.NativeInstallError, match="previous pair restored"
    ):
        native_install._replace_pair(
            directory,
            {names[0]: b"new-tj", names[1]: b"new-hook"},
            "a" * 40,
            {names[0]: "1", names[1]: "2"},
        )
    assert {(directory / name).read_bytes() for name in names} == {
        f"old-{name}".encode() for name in names
    }


@pytest.mark.parametrize(
    ("operating_system", "asset", "names"),
    [
        ("linux", "tj-x86_64-unknown-linux-gnu.tar.gz", ("tj", "tj_hook")),
        ("win32", "tj-x86_64-pc-windows-msvc.zip", ("tj.exe", "tj_hook.exe")),
    ],
)
def test_supported_platform_mapping(
    monkeypatch: pytest.MonkeyPatch,
    operating_system: str,
    asset: str,
    names: tuple[str, str],
) -> None:
    system = "Windows" if operating_system == "win32" else "Linux"
    monkeypatch.setattr(platform_module, "system", lambda: system)
    assert native_install._asset_name() == asset
    assert native_install._binary_names() == names

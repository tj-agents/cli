import contextlib
import hashlib
import json
import os
import platform
import shutil
import subprocess
import sys
import types
from pathlib import Path
from typing import Any

import pytest

from tj_cli import handy_install, tool_receipts


@pytest.fixture(autouse=True)
def windows_registry(monkeypatch: pytest.MonkeyPatch) -> dict[tuple[int, int], str]:
    entries: dict[tuple[int, int], str] = {}

    def open_key(
        hive: int, name: str, reserved: int, access: int
    ) -> contextlib.nullcontext[tuple[int, int]]:
        assert name == r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Handy"
        key = (hive, access)
        if key not in entries:
            raise FileNotFoundError
        return contextlib.nullcontext(key)

    def query_value(key: tuple[int, int], name: str) -> tuple[str, int]:
        if name == "DisplayVersion":
            return handy_install.VERSION, 1
        assert name == "InstallLocation"
        return entries[key], 1

    registry = types.ModuleType("winreg")
    for name, value in {
        "HKEY_CURRENT_USER": 1,
        "HKEY_LOCAL_MACHINE": 2,
        "KEY_READ": 1,
        "KEY_WOW64_64KEY": 256,
        "KEY_WOW64_32KEY": 512,
        "OpenKey": open_key,
        "QueryValueEx": query_value,
    }.items():
        setattr(registry, name, value)
    monkeypatch.setitem(sys.modules, "winreg", registry)
    return entries


@pytest.fixture(autouse=True)
def isolated_desktop(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    monkeypatch.setattr(platform, "system", lambda: "Windows")
    monkeypatch.setattr(shutil, "which", lambda command: None)
    for variable in ("CI", "DISPLAY", "WAYLAND_DISPLAY", "ProgramW6432"):
        monkeypatch.delenv(variable, raising=False)
    monkeypatch.setenv("TJ_BIN_DIR", str(tmp_path / "bin"))
    monkeypatch.setenv("LOCALAPPDATA", str(tmp_path / "local data"))
    monkeypatch.setenv("ProgramFiles", str(tmp_path / "program files"))


def _windows_installation(entries: dict[tuple[int, int], str] | None = None) -> Path:
    executable = Path(os.environ["LOCALAPPDATA"]) / "Handy" / "handy.exe"
    executable.parent.mkdir(parents=True)
    executable.write_bytes(b"installed")
    if entries is not None:
        entries[(1, 257)] = str(executable.parent)
    return executable


def test_check_reports_missing_without_starting_a_process(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    def unexpected_process(*args: object, **kwargs: object) -> None:
        pytest.fail("check mode must not run a package manager or download")

    monkeypatch.setattr(subprocess, "run", unexpected_process)
    assert handy_install.main(["--check"]) == 1
    assert "missing    Handy" in capsys.readouterr().out


def test_existing_windows_installation_is_unchanged(
    capsys: pytest.CaptureFixture[str],
) -> None:
    executable = _windows_installation()
    _receipt(executable)
    before = executable.stat().st_mtime_ns
    assert handy_install.main([]) == 0
    assert handy_install.main(["--check"]) == 0
    assert executable.stat().st_mtime_ns == before
    assert capsys.readouterr().out.count("ok         Handy") == 2


@pytest.mark.parametrize("hive", [1, 2])
@pytest.mark.parametrize("view", [256, 512])
def test_registered_windows_custom_location_is_recognized(
    tmp_path: Path, windows_registry: dict[tuple[int, int], str], hive: int, view: int
) -> None:
    directory = tmp_path / "custom apps é" / "voice"
    directory.mkdir(parents=True)
    executable = directory / "handy.exe"
    executable.write_bytes(b"installed")
    windows_registry[(hive, 1 | view)] = f'"{directory}"'
    assert handy_install.find_handy() == executable
    _receipt(executable)
    assert handy_install.main(["--check"]) == 0
    assert handy_install.main([]) == 0
    executable.unlink()
    assert handy_install.main(["--check"]) == 1


def test_windows_installs_once_and_verifies_the_result(
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
    windows_registry: dict[tuple[int, int], str],
) -> None:
    commands: list[list[str]] = []
    monkeypatch.setattr(
        shutil, "which", lambda name: "winget.exe" if name == "winget" else None
    )

    def run(arguments: list[str], **kwargs: object) -> subprocess.CompletedProcess[str]:
        commands.append(arguments)
        _windows_installation(windows_registry)
        return subprocess.CompletedProcess(arguments, 0, "", "")

    monkeypatch.setattr(subprocess, "run", run)
    assert handy_install.main([]) == 0
    assert handy_install.main([]) == 0
    assert len(commands) == 1
    assert commands[0][:5] == [
        "winget.exe",
        "install",
        "--id",
        "cjpais.Handy",
        "--exact",
    ]
    assert commands[0][commands[0].index("--scope") + 1] == "user"
    output = capsys.readouterr().out
    assert "installed  Handy" in output
    assert "ok         Handy" in output


def test_windows_missing_winget_is_actionable(
    capsys: pytest.CaptureFixture[str],
) -> None:
    assert handy_install.main([]) == 1
    assert "App Installer (winget)" in capsys.readouterr().err


@pytest.mark.parametrize("failure", [False, True])
def test_windows_failed_or_ineffective_install_is_not_success(
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
    failure: bool,
) -> None:
    monkeypatch.setattr(
        shutil, "which", lambda name: "winget.exe" if name == "winget" else None
    )

    def run(arguments: list[str], **kwargs: object) -> subprocess.CompletedProcess[str]:
        if failure:
            raise subprocess.CalledProcessError(1, arguments)
        return subprocess.CompletedProcess(arguments, 0, "", "")

    monkeypatch.setattr(subprocess, "run", run)
    assert handy_install.main([]) == 1
    output = capsys.readouterr()
    assert "installed  Handy" not in output.out
    assert "error      Handy" in output.err


@pytest.mark.parametrize("binary_output", [False, True])
@pytest.mark.parametrize("timeout", [False, True])
def test_subprocess_failure_keeps_stdout_and_stderr(
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
    binary_output: bool,
    timeout: bool,
) -> None:
    monkeypatch.setattr(
        shutil, "which", lambda name: "winget.exe" if name == "winget" else None
    )
    stdout = b"dependency missing" if binary_output else "dependency missing"
    stderr = (
        b"install dependency first" if binary_output else "install dependency first"
    )

    def run(arguments: list[str], **kwargs: object) -> subprocess.CompletedProcess[str]:
        if timeout:
            raise subprocess.TimeoutExpired(
                arguments, 600, output=stdout, stderr=stderr
            )
        raise subprocess.CalledProcessError(1, arguments, output=stdout, stderr=stderr)

    monkeypatch.setattr(subprocess, "run", run)
    assert handy_install.main([]) == 1
    output = capsys.readouterr()
    assert "dependency missing" in output.err
    assert "install dependency first" in output.err
    assert "installed  Handy" not in output.out


@pytest.mark.parametrize("variable", ["DISPLAY", "WAYLAND_DISPLAY"])
def test_linux_desktop_requires_handy(
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
    variable: str,
) -> None:
    monkeypatch.setattr(platform, "system", lambda: "Linux")
    monkeypatch.setenv(variable, "desktop")
    assert handy_install.main(["--check"]) == 1
    assert "missing    Handy" in capsys.readouterr().out


def test_headless_linux_does_not_install_desktop_software(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    monkeypatch.setattr(platform, "system", lambda: "Linux")
    assert handy_install.main([]) == 0
    assert "no desktop session" in capsys.readouterr().out


def test_ci_does_not_install_desktop_software(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    monkeypatch.setenv("CI", "true")
    assert handy_install.main([]) == 0
    assert "Handy (CI)" in capsys.readouterr().out


@pytest.mark.parametrize(
    ("manager", "machine", "suffix"),
    [
        ("apt-get", "x86_64", "_amd64.deb"),
        ("apt-get", "aarch64", "_arm64.deb"),
        ("dnf", "x86_64", ".x86_64.rpm"),
        ("dnf", "aarch64", ".aarch64.rpm"),
    ],
)
@pytest.mark.parametrize("root", [False, True])
@pytest.mark.parametrize("current", [None, "0.9.0", "0.10.0", "0.11.0"])
def test_linux_installs_verified_package_once(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    manager: str,
    machine: str,
    suffix: str,
    root: bool,
    current: str | None,
) -> None:
    monkeypatch.setattr(platform, "system", lambda: "Linux")
    monkeypatch.setattr(platform, "machine", lambda: machine)
    monkeypatch.setenv("DISPLAY", ":0")
    installed = tmp_path / "handy"
    package = b"upstream package"
    commands: list[list[str]] = []
    installed_version = current
    if current == handy_install.VERSION:
        installed.write_bytes(b"previous installation")
        installed.chmod(0o755)
        _receipt(installed)
        installed.write_bytes(b"damaged installation")

    def which(command: str) -> str | None:
        if command == "handy":
            return str(installed) if installed.exists() else None
        return f"/tools/{command}" if command in {manager, "gh", "sudo"} else None

    def run(arguments: list[str], **kwargs: Any) -> subprocess.CompletedProcess[str]:
        nonlocal installed_version
        commands.append(arguments)
        if arguments[0] == "dpkg-deb" or (
            arguments[0] == "rpm" and "--package" in arguments
        ):
            return subprocess.CompletedProcess(arguments, 0, "handy", "")
        if arguments[0] in {"dpkg-query", "rpm"}:
            if "--listfiles" in arguments or "--list" in arguments:
                return subprocess.CompletedProcess(arguments, 0, f"{installed}\n", "")
            if installed_version is None:
                raise subprocess.CalledProcessError(1, arguments)
            return subprocess.CompletedProcess(arguments, 0, installed_version, "")
        if arguments == ["id", "-u"]:
            return subprocess.CompletedProcess(
                arguments, 0, "0\n" if root else "1000\n", ""
            )
        if (
            arguments[-1]
            == f"repos/cjpais/Handy/releases/tags/v{handy_install.VERSION}"
        ):
            release = {
                "tag_name": f"v{handy_install.VERSION}",
                "draft": False,
                "assets": [
                    {
                        "id": 123,
                        "name": f"Handy_1.0{suffix}",
                        "digest": f"sha256:{hashlib.sha256(package).hexdigest()}",
                    }
                ],
            }
            return subprocess.CompletedProcess(arguments, 0, json.dumps(release), "")
        if arguments[0] == "/tools/gh":
            kwargs["stdout"].write(package)
        else:
            prefix = [] if root else ["/tools/sudo"]
            operation = (
                "reinstall"
                if manager == "dnf" and current == handy_install.VERSION
                else "downgrade"
                if manager == "dnf" and current == "0.11.0"
                else "install"
            )
            options = (
                [
                    "--allow-downgrades",
                    "--reinstall",
                    "-o",
                    "Dpkg::Options::=--force-confdef",
                    "-o",
                    "Dpkg::Options::=--force-confold",
                ]
                if manager == "apt-get"
                else []
            )
            assert arguments[:-1] == [*prefix, manager, operation, "-y", *options]
            assert kwargs["env"]["DEBIAN_FRONTEND"] == "noninteractive"
            assert Path(arguments[-1]).read_bytes() == package
            installed.write_bytes(b"executable")
            installed.chmod(0o755)
            installed_version = handy_install.VERSION
        return subprocess.CompletedProcess(arguments, 0, "", "")

    monkeypatch.setattr(shutil, "which", which)
    monkeypatch.setattr(subprocess, "run", run)
    assert handy_install.main(["--no-escalate"] if root else []) == 0
    assert installed.read_bytes() == b"executable"
    before = len(commands)
    assert handy_install.main(["--no-escalate"]) == 0
    assert len(commands) == before
    gh_commands = [args for args in commands if args[0] == "/tools/gh"]
    assert len(gh_commands) == 2
    assert all(args[2:4] == ["--hostname", "github.com"] for args in gh_commands)


@pytest.mark.parametrize("digest", [None, "sha256:" + "0" * 64])
def test_linux_unverified_download_never_reaches_package_manager(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    digest: str | None,
) -> None:
    commands: list[list[str]] = []

    def run(arguments: list[str], **kwargs: Any) -> subprocess.CompletedProcess[str]:
        commands.append(arguments)
        if "stdout" in kwargs:
            kwargs["stdout"].write(b"corrupt")
            return subprocess.CompletedProcess(arguments, 0, "", "")
        release = {
            "tag_name": f"v{handy_install.VERSION}",
            "draft": False,
            "assets": [{"id": 123, "name": "Handy_1_amd64.deb", "digest": digest}],
        }
        return subprocess.CompletedProcess(arguments, 0, json.dumps(release), "")

    monkeypatch.setattr(subprocess, "run", run)
    with pytest.raises(handy_install.HandyInstallError, match="SHA-256"):
        handy_install._download_linux_package("gh", "_amd64.deb", tmp_path)
    assert all(arguments[0] == "gh" for arguments in commands)


def test_linux_unsupported_package_manager_gives_manual_resolution(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    monkeypatch.setattr(platform, "system", lambda: "Linux")
    monkeypatch.setattr(platform, "machine", lambda: "x86_64")
    monkeypatch.setenv("DISPLAY", ":0")
    assert handy_install.main([]) == 1
    assert "handy on PATH" in capsys.readouterr().err


def _receipt(executable: Path, version: str = handy_install.VERSION) -> None:
    tool_receipts.write(
        Path(os.environ["TJ_BIN_DIR"]) / handy_install.RECEIPT,
        tool_receipts.Receipt(
            version, {str(executable.resolve()): tool_receipts.sha256(executable)}
        ),
    )


def test_unreceipted_application_requires_installation_without_probing(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _windows_installation()
    monkeypatch.setattr(
        subprocess, "run", lambda *a, **kw: pytest.fail("file-only probe")
    )
    assert handy_install.pending() is not None
    assert handy_install.main(["--check"]) == 1


@pytest.mark.parametrize("version", ["0.9.0", "0.11.0"])
def test_windows_reinstalls_the_pin_for_version_drift(
    monkeypatch: pytest.MonkeyPatch,
    version: str,
    windows_registry: dict[tuple[int, int], str],
) -> None:
    executable = _windows_installation(windows_registry)
    _receipt(executable, version)
    commands: list[list[str]] = []
    monkeypatch.setattr(
        shutil, "which", lambda name: "winget.exe" if name == "winget" else None
    )

    def run(arguments: list[str], **kwargs: object) -> subprocess.CompletedProcess[str]:
        commands.append(arguments)
        executable.write_bytes(b"upgraded")
        return subprocess.CompletedProcess(arguments, 0, "", "")

    monkeypatch.setattr(subprocess, "run", run)
    assert handy_install.main([]) == 0
    assert commands[0][commands[0].index("--version") + 1] == handy_install.VERSION
    assert "--force" in commands[0]
    assert handy_install.pending() is None
    assert handy_install.main([]) == 0
    assert len(commands) == 1


def test_windows_no_escalate_refuses_before_winget(
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    monkeypatch.setattr(
        subprocess, "run", lambda *a, **kw: pytest.fail("must not launch installer")
    )
    assert handy_install.main(["--no-escalate"]) == 1
    assert len(capsys.readouterr().err.splitlines()) == 1
    executable = _windows_installation()
    _receipt(executable)
    assert handy_install.main(["--no-escalate"]) == 0


def test_linux_no_escalate_never_calls_sudo_or_downloads(
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    monkeypatch.setattr(platform, "system", lambda: "Linux")
    monkeypatch.setattr(platform, "machine", lambda: "x86_64")
    monkeypatch.setenv("DISPLAY", ":0")
    monkeypatch.setattr(
        shutil,
        "which",
        lambda name: f"/tools/{name}" if name in {"apt-get", "gh", "sudo"} else None,
    )
    commands: list[list[str]] = []

    def run(arguments: list[str], **kwargs: object) -> subprocess.CompletedProcess[str]:
        commands.append(arguments)
        assert arguments == ["id", "-u"]
        return subprocess.CompletedProcess(arguments, 0, "1000\n", "")

    monkeypatch.setattr(subprocess, "run", run)
    assert handy_install.main(["--no-escalate"]) == 1
    assert len(capsys.readouterr().err.splitlines()) == 1
    assert commands == [["id", "-u"]]


def test_windows_receipts_registered_pin_instead_of_older_path_copy(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    windows_registry: dict[tuple[int, int], str],
) -> None:
    shadow = tmp_path / "portable" / "handy.exe"
    shadow.parent.mkdir()
    shadow.write_bytes(b"old portable version")
    managed = tmp_path / "managed" / "handy.exe"
    managed.parent.mkdir()
    monkeypatch.setattr(
        shutil, "which", lambda name: str(shadow) if name == "handy" else "winget.exe"
    )

    def run(arguments: list[str], **kwargs: object) -> subprocess.CompletedProcess[str]:
        managed.write_bytes(b"pinned installation")
        windows_registry[(1, 257)] = str(managed.parent)
        return subprocess.CompletedProcess(arguments, 0, "", "")

    monkeypatch.setattr(subprocess, "run", run)
    assert handy_install.main([]) == 0
    receipt = tool_receipts.read(Path(os.environ["TJ_BIN_DIR"]) / handy_install.RECEIPT)
    assert receipt is not None
    assert receipt.hashes == {str(managed.resolve()): tool_receipts.sha256(managed)}
    assert handy_install.pending() is None


def test_windows_installer_success_without_pinned_registration_is_failure(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    executable = _windows_installation()
    monkeypatch.setattr(
        shutil,
        "which",
        lambda name: str(executable) if name == "handy" else "winget.exe",
    )
    monkeypatch.setattr(
        subprocess, "run", lambda *a, **kw: subprocess.CompletedProcess([], 0, "", "")
    )
    assert handy_install.main([]) == 1
    assert not (Path(os.environ["TJ_BIN_DIR"]) / handy_install.RECEIPT).exists()


@pytest.mark.parametrize("manager", ["apt-get", "dnf"])
def test_linux_wrong_package_version_never_gets_a_receipt(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    manager: str,
) -> None:
    monkeypatch.setattr(platform, "system", lambda: "Linux")
    monkeypatch.setenv("DISPLAY", ":0")
    package = tmp_path / "package"
    package.write_bytes(b"verified package")
    monkeypatch.setattr(handy_install, "_linux_package", lambda: (manager, "suffix"))
    monkeypatch.setattr(handy_install, "_download_linux_package", lambda *a: package)
    monkeypatch.setattr(
        shutil, "which", lambda name: "/tools/gh" if name == "gh" else None
    )

    def run(arguments: list[str], **kwargs: object) -> subprocess.CompletedProcess[str]:
        if arguments == ["id", "-u"]:
            return subprocess.CompletedProcess(arguments, 0, "0", "")
        if arguments[0] == "dpkg-deb" or "--package" in arguments:
            return subprocess.CompletedProcess(arguments, 0, "handy", "")
        if arguments[0] in {"dpkg-query", "rpm"}:
            return subprocess.CompletedProcess(arguments, 0, "0.11.0", "")
        return subprocess.CompletedProcess(arguments, 0, "", "")

    monkeypatch.setattr(subprocess, "run", run)
    assert handy_install.main(["--no-escalate"]) == 1
    assert not (Path(os.environ["TJ_BIN_DIR"]) / handy_install.RECEIPT).exists()

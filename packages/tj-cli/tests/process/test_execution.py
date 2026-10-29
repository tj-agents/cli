import ctypes
import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

import pytest

from tj_cli import process


def test_run_preserves_arguments_streams_environment_directory_and_exit(
    tmp_path: Path,
) -> None:
    arguments = ["", "two words", 'a"b', "tail\\", "é日本", "&|<>%!"]
    code = (
        "import json,os,sys; "
        "print(json.dumps([sys.argv[1:],sys.stdin.read(),"
        "os.getcwd(),os.environ['HOOK_PROBE']])); "
        "sys.stderr.write('diagnostic'); sys.exit(37)"
    )
    command = [sys.executable, "-c", code, *arguments]
    result = process.run(
        command,
        input="event",
        capture_output=True,
        text=True,
        encoding="utf-8",
        cwd=tmp_path,
        env={**os.environ, "HOOK_PROBE": "present"},
        timeout=30,
    )
    assert result.returncode == 37
    assert result.args == command
    assert result.stderr == "diagnostic"
    assert json.loads(result.stdout) == [arguments, "event", str(tmp_path), "present"]


def test_run_check_retains_the_requested_command() -> None:
    command = [sys.executable, "-c", "import sys; print('failed'); sys.exit(5)"]
    with pytest.raises(subprocess.CalledProcessError) as caught:
        process.run(command, capture_output=True, text=True, check=True, timeout=30)
    assert caught.value.returncode == 5
    assert caught.value.cmd == command
    assert caught.value.stdout == "failed\n"


def test_run_missing_executable_reports_diagnostic() -> None:
    result = process.run(
        ["tj-hook-nonexistent-command"], capture_output=True, text=True, timeout=30
    )
    assert result.returncode == 127
    assert "tj_hook:" in result.stderr


def test_run_requires_installed_launcher(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("PATH", "")
    with pytest.raises(FileNotFoundError, match="tj_hook is not installed"):
        process.run([sys.executable, "-c", "pass"])


@pytest.mark.skipif(sys.platform != "win32", reason="Windows executable search")
def test_launcher_comes_from_path_without_implicit_current_directory(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    (tmp_path / "tj_hook.exe").write_bytes(b"not an executable")
    monkeypatch.chdir(tmp_path)
    result = process.run(
        [sys.executable, "-c", "print('installed launcher')"],
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert result.returncode == 0, result.stderr
    assert result.stdout == "installed launcher\n"


@pytest.mark.parametrize("entry", ["tools", ""])
def test_launcher_resolution_precedes_child_directory_and_environment(
    native_launcher: Path, tmp_path: Path, monkeypatch: pytest.MonkeyPatch, entry: str
) -> None:
    installed = tmp_path / entry
    installed.mkdir(exist_ok=True)
    shutil.copy2(native_launcher, installed / native_launcher.name)
    child_directory = tmp_path / "child"
    competing = child_directory / "tools"
    competing.mkdir(parents=True)
    shadow = competing / native_launcher.name
    shadow.write_bytes(b"not an executable")
    shadow.chmod(0o755)
    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv("PATH", entry + os.pathsep + os.environ["PATH"])
    result = process.run(
        [sys.executable, "-c", "print('selected launcher')"],
        cwd=child_directory,
        env={**os.environ, "PATH": str(competing)},
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert result.returncode == 0, result.stderr
    assert result.stdout == "selected launcher\n"


def test_spawn_rejects_implicit_shell() -> None:
    with pytest.raises(ValueError, match="explicit executable"):
        process.spawn([sys.executable], shell=True)


def test_run_inherits_default_standard_streams() -> None:
    child = "import sys; print(sys.stdin.read()); sys.stderr.write('diagnostic')"
    driver = (
        "import sys; from tj_cli import process; "
        f"sys.exit(process.run([sys.executable,'-c',{child!r}]).returncode)"
    )
    result = subprocess.run(
        [sys.executable, "-c", driver],
        input="event",
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert result.returncode == 0, result.stderr
    assert result.stdout == "event\n"
    assert result.stderr == "diagnostic"


@pytest.mark.skipif(sys.platform != "win32", reason="Windows explicit shell")
def test_run_explicit_cmd_command_string() -> None:
    result = process.run(
        ["cmd.exe", "/d", "/c", 'echo "hello world" & exit /b 7'],
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert result.returncode == 7, result.stderr
    assert result.stdout.strip() == '"hello world"'


@pytest.mark.skipif(sys.platform != "win32", reason="Windows batch commands")
def test_run_explicit_cmd_batch_with_spaces(tmp_path: Path) -> None:
    script = tmp_path / "hook with spaces.cmd"
    script.write_text("@echo off\necho %~1\nexit /b 9\n", encoding="utf-8")
    result = process.run(
        ["cmd.exe", "/d", "/s", "/c", f'"{script}" "two words"'],
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert result.returncode == 9, result.stderr
    assert result.stdout == "two words\n"


@pytest.mark.skipif(sys.platform != "win32", reason="Windows shell boundary")
@pytest.mark.parametrize("mode", ["/c", "/k"])
@pytest.mark.parametrize("operator", ["&", "|"])
def test_run_rejects_separate_cmd_arguments(
    tmp_path: Path, mode: str, operator: str
) -> None:
    marker = tmp_path / "injected.txt"
    result = process.run(
        ["cmd.exe", "/d", mode, "echo", f'a{operator}echo injected>"{marker}"'],
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert result.returncode == 127
    assert "require exactly one complete command string" in result.stderr
    assert not marker.exists()


@pytest.mark.skipif(sys.platform != "win32", reason="Windows shell boundary")
@pytest.mark.parametrize("mode", ["/c", "/k"])
def test_run_rejects_missing_cmd_command(mode: str) -> None:
    result = process.run(
        ["cmd.exe", "/d", mode], capture_output=True, text=True, timeout=30
    )
    assert result.returncode == 127
    assert "require exactly one complete command string" in result.stderr


@pytest.mark.skipif(sys.platform != "win32", reason="Windows shell boundary")
@pytest.mark.parametrize("mode", ["/c", "/k"])
@pytest.mark.parametrize("form", ["compact", "combined", "prefix"])
def test_run_rejects_compact_cmd_switches(tmp_path: Path, mode: str, form: str) -> None:
    marker = tmp_path / "injected.txt"
    tail = f'safe&echo injected>"{marker}"'
    arguments = {
        "compact": ["/d", f"{mode}echo", tail],
        "combined": [f"/d{mode}", "echo", tail],
        "prefix": [f"{mode}echo", "safe", mode, f"echo {tail}"],
    }
    result = process.run(
        ["cmd.exe", *arguments[form]],
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert result.returncode == 127
    assert "cmd.exe" in result.stderr
    assert not marker.exists()


def _alive(pid: int) -> bool:
    if sys.platform == "win32":
        kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel.OpenProcess.argtypes = [ctypes.c_uint32, ctypes.c_int, ctypes.c_uint32]
        kernel.OpenProcess.restype = ctypes.c_void_p
        kernel.WaitForSingleObject.argtypes = [ctypes.c_void_p, ctypes.c_uint32]
        kernel.CloseHandle.argtypes = [ctypes.c_void_p]
        handle = kernel.OpenProcess(0x100000, False, pid)
        if not handle:
            return False
        try:
            return bool(kernel.WaitForSingleObject(handle, 0) == 258)
        finally:
            kernel.CloseHandle(handle)
    else:
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            return False
        status = Path(f"/proc/{pid}/stat")
        try:
            if status.read_text(encoding="utf-8").rpartition(")")[2].split()[0] == "Z":
                return False
        except FileNotFoundError:
            return False
        return True


def test_run_timeout_identifies_requested_command() -> None:
    command = [sys.executable, "-c", "import time; time.sleep(60)"]
    with pytest.raises(subprocess.TimeoutExpired) as caught:
        process.run(command, capture_output=True, timeout=0.1)
    assert caught.value.cmd == command


@pytest.mark.skipif(os.name != "nt", reason="Windows launcher job ownership")
def test_terminating_launcher_kills_ready_descendant(
    tmp_path: Path,
) -> None:
    record = tmp_path / "child.pid"
    child_code = "import time; time.sleep(60)"
    code = (
        "import pathlib,subprocess,sys,time; "
        f"p=subprocess.Popen([sys.executable,'-c',{child_code!r}]); "
        "pathlib.Path(sys.argv[1]).write_text(str(p.pid)); "
        "print('ready',flush=True); time.sleep(60)"
    )
    command = [sys.executable, "-c", code, str(record)]
    pid = None
    with process.spawn(
        command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True
    ) as launched:
        try:
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline:
                contents = (
                    record.read_text(encoding="utf-8") if record.is_file() else ""
                )
                if contents.isdecimal():
                    pid = int(contents)
                    break
                assert launched.poll() is None
                time.sleep(0.01)
            assert pid is not None, "child did not become ready"
        finally:
            if launched.poll() is None:
                launched.kill()
            launched.communicate(timeout=30)
    deadline = time.monotonic() + 5
    while _alive(pid) and time.monotonic() < deadline:
        time.sleep(0.01)
    assert not _alive(pid)


@pytest.mark.skipif(sys.platform != "linux", reason="Linux process group ownership")
def test_run_timeout_kills_linux_descendant(tmp_path: Path) -> None:
    record = tmp_path / "child.pid"
    command = [
        "/bin/sh",
        "-c",
        'sleep 60 & printf %s "$!" > "$1"; printf ready; printf diagnostic >&2; wait',
        "sh",
        str(record),
    ]
    with pytest.raises(subprocess.TimeoutExpired) as caught:
        process.run(command, capture_output=True, text=True, timeout=1)
    assert caught.value.stdout == b"ready"
    assert caught.value.stderr == b"diagnostic"
    pid = int(record.read_text(encoding="utf-8"))
    assert not _alive(pid)


@pytest.mark.skipif(sys.platform != "linux", reason="Linux timeout partial bytes")
def test_run_timeout_retains_incomplete_utf8() -> None:
    command = ["/bin/sh", "-c", r"printf '\342'; printf '\342' >&2; sleep 60"]
    with pytest.raises(subprocess.TimeoutExpired) as caught:
        process.run(
            command, capture_output=True, text=True, encoding="utf-8", timeout=1
        )
    assert caught.value.stdout == b"\xe2"
    assert caught.value.stderr == b"\xe2"


@pytest.mark.skipif(os.name != "nt", reason="Windows console visibility")
def test_hook_and_unmodified_descendant_inherit_hidden_console() -> None:
    probe = (
        "import ctypes,json; k=ctypes.WinDLL('kernel32'); u=ctypes.WinDLL('user32'); "
        "k.GetConsoleWindow.restype=ctypes.c_void_p; "
        "u.IsWindowVisible.argtypes=[ctypes.c_void_p]; "
        "h=k.GetConsoleWindow(); "
        "print(json.dumps([bool(h),bool(u.IsWindowVisible(h))]))"
    )
    code = (
        f"exec({probe!r}); import subprocess,sys; "
        f"subprocess.run([sys.executable,'-c',{probe!r}],check=True)"
    )
    result = process.run(
        [sys.executable, "-u", "-c", code], capture_output=True, text=True, timeout=30
    )
    assert result.returncode == 0, result.stderr
    assert [json.loads(line) for line in result.stdout.splitlines()] == [
        [True, False],
        [True, False],
    ]


@pytest.mark.skipif(os.name != "nt", reason="Windows process exit status")
def test_native_launcher_preserves_full_windows_exit_status(
    native_launcher: Path,
) -> None:
    result = subprocess.run(
        [
            str(native_launcher),
            sys.executable,
            "-c",
            "import ctypes; ctypes.WinDLL('kernel32').ExitProcess(0xC0000005)",
        ],
        capture_output=True,
        timeout=30,
    )
    assert result.returncode == 0xC0000005

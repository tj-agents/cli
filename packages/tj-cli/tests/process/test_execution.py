import ctypes
import json
import os
import shutil
import subprocess
import sys
import time
from collections.abc import Callable
from pathlib import Path
from types import SimpleNamespace

import pytest

from tj_cli import process

BACKGROUND_ROOT = Path(__file__).with_name("fixtures") / "background_root.py"
BACKGROUND_CHILD = Path(__file__).with_name("fixtures") / "background_child.py"
BATCH_ECHO = Path(__file__).with_name("fixtures") / "batch_echo.cmd"
BATCH_FORWARD = Path(__file__).with_name("fixtures") / "batch_forward.cmd"
BATCH_ARGV = Path(__file__).with_name("fixtures") / "batch_argv.py"


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


def test_native_launcher_reports_its_version(native_launcher: Path) -> None:
    result = subprocess.run(
        [str(native_launcher), "--version"], capture_output=True, text=True, timeout=30
    )
    assert result.returncode == 0, result.stderr
    assert result.stdout.startswith("tj_hook ")


@pytest.mark.skipif(os.name != "nt", reason="Windows native batch dispatch")
@pytest.mark.parametrize("exit_code", [0, 23])
def test_native_launcher_runs_batch_targets_with_normal_arguments(
    native_launcher: Path, exit_code: int
) -> None:
    result = subprocess.run(
        [str(native_launcher), str(BATCH_ECHO), "space value", str(exit_code)],
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert result.returncode == exit_code, result.stderr
    assert result.stdout == "space value\n"


@pytest.mark.skipif(os.name != "nt", reason="Windows native batch dispatch")
def test_native_launcher_forwards_safely_encodable_batch_arguments(
    native_launcher: Path, tmp_path: Path
) -> None:
    output = tmp_path / "argv.json"
    values = ["space value", "ü", 'quote"value']
    result = subprocess.run(
        [
            str(native_launcher),
            str(BATCH_FORWARD),
            sys.executable,
            str(BATCH_ARGV),
            str(output),
            *values,
        ],
        capture_output=True,
        text=True,
        encoding="utf-8",
        timeout=30,
    )
    assert result.returncode == 0, result.stderr
    assert json.loads(output.read_text(encoding="utf-8")) == values


@pytest.mark.skipif(os.name != "nt", reason="Windows native batch dispatch")
def test_native_launcher_refuses_batch_dispatch_misuse_and_unsafe_targets(
    native_launcher: Path, tmp_path: Path
) -> None:
    direct = subprocess.run(
        [
            str(native_launcher),
            "--tj-hook-batch-dispatch",
            "--",
            str(BATCH_ECHO),
            "ran",
            "0",
        ],
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert direct.returncode == 127
    assert direct.stdout == ""
    forwarded = subprocess.run(
        [
            str(native_launcher),
            "--",
            "--tj-hook-batch-dispatch",
            str(BATCH_ECHO),
            "ran",
            "0",
        ],
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert forwarded.returncode == 127
    assert forwarded.stdout == ""
    percent = tmp_path / "target%.cmd"
    shutil.copy2(BATCH_ECHO, percent)
    rejected = subprocess.run(
        [str(native_launcher), str(percent), "ran", "0"],
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert rejected.returncode == 127
    assert rejected.stdout == ""
    line_break = subprocess.run(
        [str(native_launcher), str(BATCH_ECHO), "never\r\nrun", "0"],
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert line_break.returncode == 127
    assert line_break.stdout == ""
    for shell_character in "&|^%":
        sentinel = tmp_path / f"injected-{ord(shell_character)}"
        rejected = subprocess.run(
            [
                str(native_launcher),
                str(BATCH_ECHO),
                f"safe{shell_character}echo injected>{sentinel}",
                "0",
            ],
            capture_output=True,
            text=True,
            timeout=30,
        )
        assert rejected.returncode == 127
        assert rejected.stdout == ""
        assert not sentinel.exists()
    sidecar = tmp_path / "target.cmd"
    shutil.copy2(BATCH_ECHO, sidecar)
    (tmp_path / "target.cmd.exe").write_text("not executable", encoding="utf-8")
    rejected = subprocess.run(
        [str(native_launcher), str(sidecar), "ran", "0"],
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert rejected.returncode == 127
    assert rejected.stdout == ""
    nested = subprocess.run(
        [
            str(native_launcher),
            "--",
            str(native_launcher),
            "--tj-hook-batch-dispatch",
            "--",
            str(percent),
            "ran",
            "0",
        ],
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert nested.returncode == 127
    assert nested.stdout == ""


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


def test_start_rejects_implicit_shell() -> None:
    with pytest.raises(ValueError, match="explicit executable"):
        process.start([sys.executable], shell=True)


@pytest.mark.parametrize("execute", [process.run, process.start])
@pytest.mark.parametrize("option", ["preexec_fn", "startupinfo", "universal_newlines"])
def test_rejects_unsupported_process_options_before_launch(
    execute: Callable[..., object], option: str, tmp_path: Path
) -> None:
    marker = tmp_path / "launched"
    command = [
        sys.executable,
        "-c",
        "import pathlib,sys; pathlib.Path(sys.argv[1]).touch()",
        str(marker),
    ]
    with pytest.raises(TypeError, match=f"unsupported process options: {option}"):
        execute(command, **{option: True})
    assert not marker.exists()


@pytest.mark.parametrize("payload", [b"binary\x00\xff", "text é日本"])
def test_run_preserves_binary_and_text_input(payload: str | bytes) -> None:
    result = process.run(
        [
            sys.executable,
            "-c",
            "import sys; sys.stdout.buffer.write(sys.stdin.buffer.read())",
        ],
        input=payload,
        text=isinstance(payload, str),
        encoding="utf-8" if isinstance(payload, str) else None,
        capture_output=True,
        timeout=30,
    )
    assert result.returncode == 0, result.stderr
    assert result.stdout == payload


def test_run_redirects_to_an_open_file(tmp_path: Path) -> None:
    output = tmp_path / "output"
    with output.open("wb") as stream:
        result = process.run(
            [sys.executable, "-c", "import sys; sys.stdout.buffer.write(b'written')"],
            stdout=stream,
            bufsize=0,
            close_fds=True,
            timeout=30,
        )
    assert result.returncode == 0
    assert result.stdout is None
    assert output.read_bytes() == b"written"


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


@pytest.mark.skipif(sys.platform != "win32", reason="Windows executable aliases")
@pytest.mark.parametrize("suffix", [".", " "])
def test_run_classifies_cmd_executable_aliases(tmp_path: Path, suffix: str) -> None:
    executable = shutil.which("cmd.exe")
    assert executable is not None
    alias = executable + suffix
    accepted = process.run(
        [alias, "/d", "/c", "echo accepted"],
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert accepted.returncode == 0, accepted.stderr
    assert accepted.stdout.strip() == "accepted"
    marker = tmp_path / "injected.txt"
    rejected = process.run(
        [alias, "/d", "/c", "echo", f'safe&echo injected>"{marker}"'],
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert rejected.returncode == 127
    assert "require exactly one complete command string" in rejected.stderr
    assert not marker.exists()


@pytest.mark.skipif(sys.platform != "win32", reason="Windows App Execution Aliases")
@pytest.mark.parametrize("name", ["py.exe", "winget.exe"])
def test_run_preserves_registered_app_execution_aliases(name: str) -> None:
    if sys.platform == "win32":
        alias = Path(os.environ["LOCALAPPDATA"]) / "Microsoft" / "WindowsApps" / name
        if not alias.is_file() or alias.lstat().st_reparse_tag != 0x8000001B:
            pytest.skip(f"{name} is not registered as an App Execution Alias")
        expected = subprocess.run(
            [str(alias), "--version"],
            capture_output=True,
            timeout=30,
            creationflags=subprocess.CREATE_NO_WINDOW,
        )
        actual = process.run([str(alias), "--version"], capture_output=True, timeout=30)
        assert actual.returncode == expected.returncode == 0
        assert actual.stdout == expected.stdout
        assert actual.stderr == expected.stderr


@pytest.mark.skipif(sys.platform != "win32", reason="Git for Windows installation")
def test_run_preserves_git_system_config_lookup() -> None:
    command = ["git", "config", "--system", "--list", "--name-only"]
    environment = {
        key: value
        for key, value in os.environ.items()
        if not key.startswith("GIT_CONFIG_")
    }
    expected = subprocess.run(command, capture_output=True, env=environment, timeout=30)
    actual = process.run(command, capture_output=True, env=environment, timeout=30)
    assert actual.returncode == expected.returncode == 0, actual.stderr
    assert actual.stdout == expected.stdout
    assert actual.stderr == expected.stderr


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
        except (FileNotFoundError, ProcessLookupError):
            return False
        return True


def _wait_for_record(path: Path) -> dict[str, object]:
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        if path.is_file():
            record: dict[str, object] = json.loads(path.read_text(encoding="utf-8"))
            return record
        time.sleep(0.01)
    pytest.fail(f"background process did not write {path}")


def _wait_for_exit(pid: int) -> None:
    deadline = time.monotonic() + 5
    while _alive(pid) and time.monotonic() < deadline:
        time.sleep(0.01)
    assert not _alive(pid)


def _stop_background(pid: int) -> None:
    if _alive(pid):
        process.run(
            ["taskkill", "/PID", str(pid), "/T", "/F"], capture_output=True, timeout=30
        )
        _wait_for_exit(pid)


def _background_command(
    report: Path, exit_code: int, delay: float, preserve: bool
) -> list[str]:
    command = [
        str(BACKGROUND_ROOT),
        str(BACKGROUND_CHILD),
        str(report),
        "60",
        str(exit_code),
        str(delay),
    ]
    options = ["--preserve-descendants"] if preserve else []
    return [*options, "--", sys.executable, *command]


def _batch_background_command(
    report: Path, exit_code: int, delay: float, preserve: bool
) -> list[str]:
    command = [
        str(BATCH_FORWARD),
        sys.executable,
        str(BACKGROUND_ROOT),
        str(BACKGROUND_CHILD),
        str(report),
        "60",
        str(exit_code),
        str(delay),
    ]
    options = ["--preserve-descendants"] if preserve else []
    return [*options, "--", *command]


@pytest.mark.parametrize(
    "failure", [FileNotFoundError, ProcessLookupError, PermissionError]
)
def test_alive_handles_process_exit_during_procfs_read(
    monkeypatch: pytest.MonkeyPatch, failure: type[OSError]
) -> None:
    def probe(pid: int, signal: int) -> None:
        assert pid == 123 and signal == 0

    def read_status(_path: Path, **_kwargs: object) -> str:
        raise failure("process status unavailable")

    monkeypatch.setitem(
        _alive.__globals__, "os", SimpleNamespace(name="posix", kill=probe)
    )
    monkeypatch.setitem(_alive.__globals__, "sys", SimpleNamespace(platform="linux"))
    monkeypatch.setattr(Path, "read_text", read_status)
    if failure is PermissionError:
        with pytest.raises(PermissionError, match="process status unavailable"):
            _alive(123)
    else:
        assert not _alive(123)


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
    with process.start(
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


@pytest.mark.skipif(os.name != "nt", reason="Windows launcher job ownership")
@pytest.mark.parametrize("exit_code", [0, 23])
def test_preserve_descendants_releases_hidden_background_child_after_root_exit(
    native_launcher: Path, tmp_path: Path, exit_code: int
) -> None:
    report = tmp_path / "child.json"
    result = subprocess.run(
        [
            str(native_launcher),
            *_background_command(report, exit_code, 0, preserve=True),
        ],
        timeout=30,
    )
    assert result.returncode == exit_code
    child = _wait_for_record(report)
    pid = child["pid"]
    assert isinstance(pid, int)
    try:
        assert _alive(pid)
        assert child == {"pid": pid, "console": True, "visible": False}
    finally:
        _stop_background(pid)


@pytest.mark.skipif(os.name != "nt", reason="Windows launcher job ownership")
def test_preserve_descendants_cancellation_kills_the_owned_tree(
    native_launcher: Path, tmp_path: Path
) -> None:
    report = tmp_path / "child.json"
    launcher = subprocess.Popen(
        [str(native_launcher), *_background_command(report, 0, 60, preserve=True)],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        child = _wait_for_record(report)
        pid = child["pid"]
        assert isinstance(pid, int)
        launcher.kill()
        launcher.communicate(timeout=30)
        _wait_for_exit(pid)
    finally:
        if launcher.poll() is None:
            launcher.kill()
            launcher.communicate(timeout=30)


@pytest.mark.skipif(os.name != "nt", reason="Windows launcher job ownership")
def test_preserve_descendants_cancellation_does_not_kill_an_unrelated_sentinel(
    native_launcher: Path, tmp_path: Path
) -> None:
    sentinel_report = tmp_path / "sentinel.json"
    sentinel = subprocess.Popen(
        [
            str(native_launcher),
            *_background_command(sentinel_report, 0, 60, preserve=False),
        ],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    report = tmp_path / "child.json"
    launcher = subprocess.Popen(
        [str(native_launcher), *_background_command(report, 0, 60, preserve=True)],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        sentinel_child = _wait_for_record(sentinel_report)
        child = _wait_for_record(report)
        sentinel_pid = sentinel_child["pid"]
        pid = child["pid"]
        assert isinstance(sentinel_pid, int)
        assert isinstance(pid, int)
        launcher.kill()
        launcher.communicate(timeout=30)
        _wait_for_exit(pid)
        assert _alive(sentinel_pid)
    finally:
        if launcher.poll() is None:
            launcher.kill()
            launcher.communicate(timeout=30)
        if sentinel.poll() is None:
            sentinel.kill()
            sentinel.communicate(timeout=30)


@pytest.mark.skipif(os.name != "nt", reason="Windows native batch dispatch")
def test_batch_broker_preserves_hidden_children_and_cancellation_owns_them(
    native_launcher: Path, tmp_path: Path
) -> None:
    report = tmp_path / "child.json"
    normal = subprocess.run(
        [str(native_launcher), *_batch_background_command(report, 0, 0, preserve=True)],
        timeout=30,
    )
    assert normal.returncode == 0
    child = _wait_for_record(report)
    pid = child["pid"]
    assert isinstance(pid, int)
    try:
        assert child == {"pid": pid, "console": True, "visible": False}
        assert _alive(pid)
    finally:
        _stop_background(pid)
    report = tmp_path / "cancelled.json"
    launcher = subprocess.Popen(
        [
            str(native_launcher),
            *_batch_background_command(report, 0, 60, preserve=True),
        ],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        child = _wait_for_record(report)
        pid = child["pid"]
        assert isinstance(pid, int)
        launcher.kill()
        launcher.communicate(timeout=30)
        _wait_for_exit(pid)
    finally:
        if launcher.poll() is None:
            launcher.kill()
            launcher.communicate(timeout=30)


@pytest.mark.skipif(os.name != "nt", reason="Windows native batch dispatch")
def test_run_timeout_kills_batch_descendant(tmp_path: Path) -> None:
    report = tmp_path / "child.json"
    command = [
        str(BATCH_FORWARD),
        sys.executable,
        str(BACKGROUND_ROOT),
        str(BACKGROUND_CHILD),
        str(report),
        "60",
        "0",
        "60",
    ]
    with pytest.raises(subprocess.TimeoutExpired) as caught:
        process.run(command, capture_output=True, timeout=1)
    assert caught.value.cmd == command
    child = _wait_for_record(report)
    pid = child["pid"]
    assert isinstance(pid, int)
    _wait_for_exit(pid)


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
    _wait_for_exit(pid)


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

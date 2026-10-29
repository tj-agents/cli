import locale
import os
import shutil
import signal
import subprocess
import sys
from collections.abc import Sequence
from contextlib import suppress
from typing import Any


def spawn(
    command: Sequence[str | os.PathLike[str]], **options: Any
) -> subprocess.Popen[Any]:
    if not command:
        raise ValueError("an executable is required")
    if options.pop("shell", False):
        raise ValueError("hidden processes require an explicit executable")
    if "executable" in options:
        raise ValueError("the executable belongs in the command")
    launcher = shutil.which("tj_hook")
    if launcher is None:
        raise FileNotFoundError("tj_hook is not installed on PATH")
    if sys.platform == "win32":
        flags = options.pop("creationflags", 0)
        if flags & (subprocess.CREATE_NEW_CONSOLE | subprocess.DETACHED_PROCESS):
            raise ValueError(
                "hidden processes cannot request a visible or detached console"
            )
        options["creationflags"] = flags | subprocess.CREATE_NO_WINDOW
    return subprocess.Popen([launcher, "--", *command], **options)


def run(
    command: Sequence[str | os.PathLike[str]],
    *,
    capture_output: bool = False,
    timeout: float | None = None,
    check: bool = False,
    **options: Any,
) -> subprocess.CompletedProcess[Any]:
    input_data = options.pop("input", None)
    if input_data is not None:
        if "stdin" in options:
            raise ValueError("stdin and input arguments may not both be used")
        options["stdin"] = subprocess.PIPE
    if capture_output:
        if "stdout" in options or "stderr" in options:
            raise ValueError("stdout and stderr cannot be used with capture_output")
        options["stdout"] = options["stderr"] = subprocess.PIPE
    if sys.platform != "win32":
        options["start_new_session"] = True
    with spawn(command, **options) as process:
        try:
            stdout, stderr = process.communicate(input_data, timeout=timeout)
        except subprocess.TimeoutExpired as error:
            _kill_tree(process)
            stdout, stderr = process.communicate()
            error.cmd = command
            encoding = options.get("encoding") or locale.getpreferredencoding(False)
            errors = options.get("errors") or "strict"
            error.output = _timeout_output(stdout, encoding, errors)
            error.stderr = _timeout_output(stderr, encoding, errors)
            raise
        except BaseException:
            _kill_tree(process)
            process.communicate()
            raise
        result = subprocess.CompletedProcess(
            command, process.returncode, stdout, stderr
        )
        if check:
            result.check_returncode()
        return result


def _kill_tree(process: subprocess.Popen[Any]) -> None:
    if sys.platform == "win32":
        process.kill()
    else:
        with suppress(ProcessLookupError):
            os.killpg(process.pid, signal.SIGKILL)


def _timeout_output(value: object, encoding: str, errors: str) -> bytes | None:
    if isinstance(value, str):
        return value.encode(encoding, errors)
    if isinstance(value, bytes):
        return value
    return None

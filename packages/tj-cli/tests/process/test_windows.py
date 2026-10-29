import ctypes
import json
import subprocess
import sys
import time
from ctypes import wintypes

import pytest

from tj_cli import process


@pytest.mark.skipif(sys.platform != "win32", reason="Windows window events")
def test_launcher_console_is_never_shown() -> None:
    if sys.platform == "win32":
        user = ctypes.WinDLL("user32", use_last_error=True)
        callback_type = ctypes.WINFUNCTYPE(
            None,
            wintypes.HANDLE,
            wintypes.DWORD,
            wintypes.HWND,
            wintypes.LONG,
            wintypes.LONG,
            wintypes.DWORD,
            wintypes.DWORD,
        )
        shown: set[int] = set()

        def on_show(
            hook: int,
            event: int,
            window: int,
            object_id: int,
            child_id: int,
            thread_id: int,
            timestamp: int,
        ) -> None:
            if object_id == 0 and child_id == 0:
                shown.add(window)

        callback = callback_type(on_show)
        user.SetWinEventHook.argtypes = [
            wintypes.DWORD,
            wintypes.DWORD,
            wintypes.HMODULE,
            callback_type,
            wintypes.DWORD,
            wintypes.DWORD,
            wintypes.DWORD,
        ]
        user.SetWinEventHook.restype = wintypes.HANDLE
        user.UnhookWinEvent.argtypes = [wintypes.HANDLE]
        user.PeekMessageW.argtypes = [
            ctypes.POINTER(wintypes.MSG),
            wintypes.HWND,
            wintypes.UINT,
            wintypes.UINT,
            wintypes.UINT,
        ]
        user.DispatchMessageW.argtypes = [ctypes.POINTER(wintypes.MSG)]
        user.DispatchMessageW.restype = ctypes.c_ssize_t
        hook = user.SetWinEventHook(0x8002, 0x8002, None, callback, 0, 0, 0)
        assert hook
        probe = (
            "import ctypes,json; k=ctypes.WinDLL('kernel32'); "
            "k.GetConsoleWindow.restype=ctypes.c_void_p; "
            "print(json.dumps(k.GetConsoleWindow()),flush=True)"
        )
        code = (
            f"exec({probe!r}); import subprocess,sys; "
            f"subprocess.run([sys.executable,'-c',{probe!r}],check=True)"
        )
        try:
            with process.spawn(
                [sys.executable, "-u", "-c", code],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            ) as launched:
                deadline = time.monotonic() + 30
                while True:
                    message = wintypes.MSG()
                    while user.PeekMessageW(ctypes.byref(message), None, 0, 0, 1):
                        user.DispatchMessageW(ctypes.byref(message))
                    if launched.poll() is not None:
                        break
                    if time.monotonic() > deadline:
                        launched.kill()
                        pytest.fail("console probe did not finish")
                    time.sleep(0.005)
                stdout, stderr = launched.communicate(timeout=5)
                assert launched.returncode == 0, stderr
        finally:
            user.UnhookWinEvent(hook)
        windows = [json.loads(line) for line in stdout.splitlines()]
        assert len(windows) == 2 and windows[0] and windows[0] == windows[1]
        assert not shown.intersection(windows)

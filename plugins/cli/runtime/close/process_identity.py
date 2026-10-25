"""Query and compare exact process identities (Python 3.9+)."""

from __future__ import annotations

import collections
import os
from pathlib import Path
import subprocess
import sys

ProcessInfo = collections.namedtuple("ProcessInfo", "pid ppid started_at")
TOLERANCE_SECONDS = 0.01


class Refusal(RuntimeError):
    pass


def _linux_boot_time() -> float:
    for line in Path("/proc/stat").read_text(encoding="utf-8").splitlines():
        if line.startswith("btime "):
            return float(line.split()[1])
    raise OSError("/proc/stat has no boot time")


def _linux_process(pid: int) -> ProcessInfo | None:
    try:
        raw = Path("/proc") / str(pid) / "stat"
        text = raw.read_text(encoding="utf-8")
    except FileNotFoundError:
        return None
    end = text.rfind(")")
    if end < 0:
        raise Refusal("malformed Linux process identity")
    fields = text[end + 2:].split()
    try:
        ppid = int(fields[1])
        ticks = int(fields[19])
        rate = os.sysconf("SC_CLK_TCK")
    except (IndexError, ValueError, OSError) as error:
        raise Refusal("malformed Linux process identity") from error
    return ProcessInfo(pid, ppid, _linux_boot_time() + ticks / rate)


def _windows_process(pid: int) -> ProcessInfo | None:
    import ctypes
    from ctypes import wintypes

    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    kernel32.OpenProcess.restype = wintypes.HANDLE
    kernel32.GetProcessTimes.argtypes = [
        wintypes.HANDLE, ctypes.POINTER(wintypes.FILETIME), ctypes.POINTER(wintypes.FILETIME),
        ctypes.POINTER(wintypes.FILETIME), ctypes.POINTER(wintypes.FILETIME),
    ]
    kernel32.GetProcessTimes.restype = wintypes.BOOL
    kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
    kernel32.CloseHandle.restype = wintypes.BOOL
    kernel32.CreateToolhelp32Snapshot.argtypes = [wintypes.DWORD, wintypes.DWORD]
    kernel32.CreateToolhelp32Snapshot.restype = wintypes.HANDLE
    handle = kernel32.OpenProcess(0x1000, False, pid)
    if not handle:
        return None
    try:
        creation = wintypes.FILETIME()
        exited = wintypes.FILETIME()
        kernel = wintypes.FILETIME()
        user = wintypes.FILETIME()
        if not kernel32.GetProcessTimes(
            handle, ctypes.byref(creation), ctypes.byref(exited), ctypes.byref(kernel), ctypes.byref(user)
        ):
            return None
        ticks = (creation.dwHighDateTime << 32) | creation.dwLowDateTime
        if not ticks:
            return None
        started = ticks / 10_000_000 - 11_644_473_600
    finally:
        kernel32.CloseHandle(handle)

    class PROCESSENTRY32W(ctypes.Structure):
        _fields_ = [("dwSize", wintypes.DWORD), ("cntUsage", wintypes.DWORD),
                    ("th32ProcessID", wintypes.DWORD), ("th32DefaultHeapID", ctypes.c_void_p),
                    ("th32ModuleID", wintypes.DWORD), ("cntThreads", wintypes.DWORD),
                    ("th32ParentProcessID", wintypes.DWORD), ("pcPriClassBase", ctypes.c_long),
                    ("dwFlags", wintypes.DWORD), ("szExeFile", ctypes.c_wchar * 260)]

    snapshot = kernel32.CreateToolhelp32Snapshot(0x00000002, 0)
    if not snapshot or ctypes.c_ssize_t(snapshot).value == -1:
        return None
    entry = PROCESSENTRY32W()
    entry.dwSize = ctypes.sizeof(entry)
    kernel32.Process32FirstW.argtypes = [wintypes.HANDLE, ctypes.POINTER(PROCESSENTRY32W)]
    kernel32.Process32FirstW.restype = wintypes.BOOL
    kernel32.Process32NextW.argtypes = [wintypes.HANDLE, ctypes.POINTER(PROCESSENTRY32W)]
    kernel32.Process32NextW.restype = wintypes.BOOL
    try:
        if not kernel32.Process32FirstW(snapshot, ctypes.byref(entry)):
            return None
        while True:
            if int(entry.th32ProcessID) == pid:
                return ProcessInfo(pid, int(entry.th32ParentProcessID), started)
            if not kernel32.Process32NextW(snapshot, ctypes.byref(entry)):
                break
    finally:
        kernel32.CloseHandle(snapshot)
    return None


def process_info(pid: object) -> ProcessInfo | None:
    if not isinstance(pid, int) or pid <= 0:
        return None
    if os.name == "nt":
        return _windows_process(pid)
    if sys.platform.startswith("linux"):
        return _linux_process(pid)
    return None


def valid_identity(value: object) -> tuple[int, float]:
    if not isinstance(value, dict):
        raise Refusal("process identity must be an object")
    pid, started = value.get("pid"), value.get("started_at")
    if not isinstance(pid, int) or pid <= 0 or not isinstance(started, (int, float)) or started <= 0:
        raise Refusal("process identity needs positive pid and started_at")
    return pid, float(started)


def matches(identity: object) -> bool:
    pid, started = valid_identity(identity)
    actual = process_info(pid)
    return actual is not None and abs(actual.started_at - started) <= TOLERANCE_SECONDS


def require_live(identity: object, message: str = "process identity changed") -> ProcessInfo:
    pid, _started = valid_identity(identity)
    actual = process_info(pid)
    if actual is None or not matches(identity):
        raise Refusal(message)
    return actual


def require_exited(identity: object) -> None:
    if matches(identity):
        raise Refusal("exact frontend did not exit")


def descends_from(frontend: object, root: object) -> bool:
    current = require_live(frontend, "frontend identity changed")
    root_pid, root_started = valid_identity(root)
    if not matches(root):
        return False
    seen: set[int] = set()
    while current.pid not in seen:
        if current.pid == root_pid and abs(current.started_at - root_started) <= TOLERANCE_SECONDS:
            return True
        seen.add(current.pid)
        parent = process_info(current.ppid)
        if parent is None:
            return False
        current = parent
    return False

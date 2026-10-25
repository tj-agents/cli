"""Close exact terminal objects after proving their explicit process binding."""

from __future__ import annotations

import json
import os
from pathlib import Path
import shutil
import subprocess
import time

import process_identity

CONFIRM_ATTEMPTS = 10
CONFIRM_DELAY_SECONDS = 0.1


def _run(command: list[str]) -> subprocess.CompletedProcess[str]:
    try:
        return subprocess.run(command, stdin=subprocess.DEVNULL, capture_output=True, text=True,
                              encoding="utf-8", timeout=5, check=False)
    except (OSError, subprocess.TimeoutExpired) as error:
        raise process_identity.Refusal("terminal control command failed: " + str(error)) from error


def _nonempty(value: object) -> str | None:
    return value.strip() if isinstance(value, str) and value.strip() else None


def _tmux_command(identity: dict, *arguments: str) -> list[str]:
    command = ["tmux"]
    socket = _nonempty(identity.get("socket"))
    if socket:
        command.extend(["-S", socket])
    return [*command, *arguments]


def _tmux(identity: dict) -> list[dict]:
    result = _run(_tmux_command(identity, "list-panes", "-a", "-F", "#{pane_id}\t#{pane_pid}"))
    if result.returncode:
        raise process_identity.Refusal((result.stderr or result.stdout or "tmux inventory failed").strip())
    items = []
    for line in result.stdout.splitlines():
        pane, separator, pid = line.partition("\t")
        if separator and pane and pid.isdigit():
            item = {"kind": "tmux", "pane_id": pane, "pid": int(pid)}
            if _nonempty(identity.get("socket")):
                item["socket"] = identity["socket"]
            items.append(item)
    return items


def _kitty(identity: dict) -> list[dict]:
    listen = _nonempty(identity.get("listen_on"))
    if not listen:
        raise process_identity.Refusal("kitty target needs listen_on")
    result = _run(["kitty", "@", "--to", listen, "ls"])
    if result.returncode:
        raise process_identity.Refusal((result.stderr or result.stdout or "kitty inventory failed").strip())
    try:
        windows = json.loads(result.stdout)
    except ValueError as error:
        raise process_identity.Refusal("kitty returned malformed inventory") from error
    if not isinstance(windows, list):
        raise process_identity.Refusal("kitty returned malformed inventory")
    items = []
    for os_window in windows:
        if not isinstance(os_window, dict):
            continue
        for tab in os_window.get("tabs", []):
            if not isinstance(tab, dict):
                continue
            for window in tab.get("windows", []):
                if not isinstance(window, dict) or not isinstance(window.get("id"), int):
                    continue
                roots = [window.get("pid")]
                roots.extend(item.get("pid") for item in window.get("foreground_processes", []) if isinstance(item, dict))
                items.append({"kind": "kitty", "listen_on": listen, "window_id": str(window["id"]),
                              "root_pids": [pid for pid in roots if isinstance(pid, int) and pid > 0]})
    return items


def _windows(identity: dict) -> list[dict]:
    if os.name != "nt":
        raise process_identity.Refusal("Windows Terminal control is available only on Windows")
    adapter = Path(__file__).with_name("windows_terminal_uia.ps1")
    if not adapter.is_file():
        raise process_identity.Refusal("Windows Terminal adapter is unavailable")
    result = _run(["powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", str(adapter), "-Json"])
    if result.returncode:
        raise process_identity.Refusal((result.stderr or result.stdout or "Windows Terminal inventory failed").strip())
    try:
        values = json.loads(result.stdout or "[]")
    except ValueError as error:
        raise process_identity.Refusal("Windows Terminal returned malformed inventory") from error
    if not isinstance(values, list):
        raise process_identity.Refusal("Windows Terminal returned malformed inventory")
    return [{"kind": "windows-terminal", "automation_id": value.get("automationId"), "pid": value.get("processId")}
            for value in values if isinstance(value, dict) and _nonempty(value.get("automationId"))
            and isinstance(value.get("processId"), int)]


def _inventory(identity: dict) -> list[dict]:
    kind = identity.get("kind")
    if kind == "tmux":
        return _tmux(identity)
    if kind == "kitty":
        return _kitty(identity)
    if kind == "windows-terminal":
        return _windows(identity)
    if kind == "konsole":
        raise process_identity.Refusal("Konsole cannot verify exact tab disappearance")
    raise process_identity.Refusal("unsupported terminal identity")


def _matches(item: dict, identity: dict) -> bool:
    kind = identity.get("kind")
    if item.get("kind") != kind:
        return False
    if kind == "tmux":
        return item.get("pane_id") == identity.get("pane_id") and item.get("socket") == identity.get("socket")
    if kind == "kitty":
        return item.get("listen_on") == identity.get("listen_on") and item.get("window_id") == str(identity.get("window_id"))
    if kind == "windows-terminal":
        return item.get("automation_id") == identity.get("automation_id")
    return False


def _one(identity: dict) -> dict:
    matches = [item for item in _inventory(identity) if _matches(item, identity)]
    if len(matches) != 1:
        raise process_identity.Refusal("exact terminal target is absent or ambiguous")
    return matches[0]


def _roots(item: dict) -> list[int]:
    if item["kind"] == "kitty":
        return item["root_pids"]
    return [item["pid"]]


def verify_binding(target: dict) -> None:
    frontend = target.get("frontend")
    terminal = target.get("terminal")
    if not isinstance(terminal, dict):
        raise process_identity.Refusal("target needs a terminal object")
    binding = terminal.get("binding")
    process_identity.require_live(frontend, "frontend identity changed")
    item = _one(terminal)
    if not any(process_identity.matches({"pid": pid, "started_at": binding.get("started_at")})
               and pid == binding.get("pid") for pid in _roots(item)):
        raise process_identity.Refusal("terminal binding changed")
    if not process_identity.descends_from(frontend, binding):
        raise process_identity.Refusal("frontend is not owned by the exact terminal binding")


def close(target: dict) -> None:
    terminal = target["terminal"]
    verify_binding(target)
    identity = terminal
    kind = identity["kind"]
    if kind == "tmux":
        result = _run(_tmux_command(identity, "kill-pane", "-t", identity["pane_id"]))
    elif kind == "kitty":
        result = _run(["kitty", "@", "--to", identity["listen_on"], "close-window", "--match", "id:" + str(identity["window_id"])])
    elif kind == "windows-terminal":
        result = _run(["powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File",
                       str(Path(__file__).with_name("windows_terminal_uia.ps1")), "-TabId", identity["automation_id"]])
    else:
        raise process_identity.Refusal("unsupported terminal identity")
    if result.returncode:
        raise process_identity.Refusal((result.stderr or result.stdout or "terminal refused close").strip())
    for _attempt in range(CONFIRM_ATTEMPTS):
        if not any(_matches(item, identity) for item in _inventory(identity)):
            return
        time.sleep(CONFIRM_DELAY_SECONDS)
    raise process_identity.Refusal("terminal did not confirm exact tab disappearance")

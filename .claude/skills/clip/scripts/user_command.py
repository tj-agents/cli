#!/usr/bin/env python3
"""Build every `!` command Tommy runs, copy it with copy_draft's plain-only path and record that it was issued.

`issue --spec <file|->` reads a JSON spec `{"shell"?, "unset"?, "steps": [{"argv": [...], "capture"?}]}`,
prints the `!` line and the clipboard receipt. `verify` reads a reply on stdin and exits 1 listing every
`!` command in it that this builder did not issue. `setup [--check]` installs this builder and copy_draft
into one stable directory other plugins call.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shlex
import shutil
import sys
import tempfile
import time
from dataclasses import dataclass
from enum import StrEnum
from pathlib import Path, PureWindowsPath

import copy_draft

RECORD_LIMIT = 200
NAME = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
BARE_WORD = re.compile(r"[A-Za-z0-9_@%+=:,./-]+")
COMMAND_START = re.compile(r"! ?[A-Za-z0-9_.~/\\&\"']")
INLINE_CODE = re.compile(r"(`+)(.+?)\1")
LINE_MARKER = re.compile(r"^\s*(?:>\s*)*(?:(?:[-*+]|\d+[.)])\s+)?")
FENCE = re.compile(r"^\s*(`{3,}|~{3,})\s*([\w+-]*)")
SHELL_FENCES = {"", "text", "sh", "bash", "shell", "console", "powershell", "pwsh", "ps1", "ps"}
INSTALLED = ("user_command.py", "copy_draft.py")


class UserCommandError(ValueError):
    pass


class Shell(StrEnum):
    BASH = "bash"
    POWERSHELL = "powershell"


@dataclass(frozen=True)
class Step:
    argv: tuple[str, ...]
    capture: str | None = None


@dataclass(frozen=True)
class Spec:
    shell: Shell
    unset: tuple[str, ...]
    steps: tuple[Step, ...]


def state_directory(environ: dict[str, str] | None = None) -> Path:
    environ = os.environ if environ is None else environ
    return Path(environ.get("AGENT_STATE_DIRECTORY") or Path.home() / ".agents-state")


def install_directory(environ: dict[str, str] | None = None) -> Path:
    return state_directory(environ) / "bin" / "cli"


def record_path(environ: dict[str, str] | None = None) -> Path:
    return state_directory(environ) / "cli" / "user-commands.jsonl"


def default_shell(environ: dict[str, str] | None = None, windows: bool = os.name == "nt") -> Shell:
    environ = os.environ if environ is None else environ
    if environ.get("CLAUDECODE") == "1":
        settings = Path(environ.get("CLAUDE_CONFIG_DIR") or Path.home() / ".claude") / "settings.json"
        try:
            configured = json.loads(settings.read_text(encoding="utf-8-sig")).get("defaultShell")
        except (OSError, ValueError, AttributeError):
            configured = None
        return Shell.POWERSHELL if configured == "powershell" else Shell.BASH
    if windows and environ.get("CODEX_THREAD_ID"):
        return Shell.POWERSHELL
    return Shell.BASH


def single_line(value: object, field: str) -> str:
    if not isinstance(value, str) or not value or any(character in value for character in "\r\n\x00"):
        raise UserCommandError(f"{field} must be a non-empty single-line string")
    return value


def environment_name(value: object, field: str) -> str:
    if not isinstance(value, str) or not NAME.fullmatch(value):
        raise UserCommandError(f"{field} must be an environment variable name")
    return value


def parse_spec(data: object, environ: dict[str, str] | None = None) -> Spec:
    if not isinstance(data, dict) or set(data) - {"shell", "unset", "steps"}:
        raise UserCommandError("spec must be an object with shell, unset and steps")
    shell = data.get("shell")
    if shell is None:
        resolved = default_shell(environ)
    elif shell in tuple(Shell):
        resolved = Shell(shell)
    else:
        raise UserCommandError("shell must be bash or powershell")
    unset = data.get("unset", [])
    if not isinstance(unset, list):
        raise UserCommandError("unset must be a list")
    steps = data.get("steps")
    if not isinstance(steps, list) or not steps:
        raise UserCommandError("steps must be a non-empty list")
    parsed = []
    for index, step in enumerate(steps):
        if not isinstance(step, dict) or set(step) - {"argv", "capture"}:
            raise UserCommandError(f"steps[{index}] must be an object with argv and an optional capture")
        argv = step.get("argv")
        if not isinstance(argv, list) or not argv:
            raise UserCommandError(f"steps[{index}].argv must be a non-empty list")
        capture = step.get("capture")
        parsed.append(Step(
            tuple(single_line(word, f"steps[{index}].argv") for word in argv),
            None if capture is None else environment_name(capture, f"steps[{index}].capture"),
        ))
    return Spec(resolved, tuple(environment_name(name, "unset") for name in unset), tuple(parsed))


def program(word: str, windows: bool) -> str:
    path = PureWindowsPath(word)
    return path.as_posix() if windows and path.drive and path.root else word


def bash_word(word: str) -> str:
    if BARE_WORD.fullmatch(word):
        return word
    if not any(character in word for character in "\"$`\\!'"):
        return f'"{word}"'
    return shlex.quote(word)


def powershell_word(word: str) -> str:
    return "'" + word.replace("'", "''") + "'"


def render(spec: Spec, windows: bool = os.name == "nt", powershell: str | None = None) -> str:
    if spec.shell is Shell.BASH:
        statements = []
        for step in spec.steps:
            words = " ".join(bash_word(word) for word in (program(step.argv[0], windows), *step.argv[1:]))
            if step.capture:
                statements += [f"{step.capture}=$({words}) || exit", f"export {step.capture}"]
            else:
                statements.append(f"{words} || exit")
        clear = "".join(f"-u {name} " for name in spec.unset)
        prefix = f"env {clear}" if clear else ""
        return f"!{prefix}bash -c {shlex.quote('; '.join(statements))}"
    statements = []
    if spec.unset:
        names = ",".join(f"Env:{name}" for name in spec.unset)
        statements.append(f"Remove-Item {names} -ErrorAction SilentlyContinue")
    for step in spec.steps:
        words = "& " + " ".join(powershell_word(word) for word in step.argv)
        statements.append(f"$env:{step.capture} = {words}" if step.capture else words)
        statements.append("if ($LASTEXITCODE) { exit $LASTEXITCODE }")
    executable = powershell or ("pwsh" if shutil.which("pwsh") else "powershell")
    return f"!{executable} -NoProfile -NonInteractive -Command {{ {'; '.join(statements)} }}"


def digest(command: str) -> str:
    return hashlib.sha256(command.removeprefix("!").strip().encode("utf-8")).hexdigest()


def issued(environ: dict[str, str] | None = None) -> set[str]:
    try:
        lines = record_path(environ).read_text(encoding="utf-8").splitlines()
    except OSError:
        return set()
    found = set()
    for line in lines:
        try:
            entry = json.loads(line)
        except ValueError:
            continue
        if isinstance(entry, dict) and isinstance(entry.get("sha256"), str):
            found.add(entry["sha256"])
    return found


def record(command: str, environ: dict[str, str] | None = None) -> None:
    path = record_path(environ)
    path.parent.mkdir(parents=True, exist_ok=True)
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except OSError:
        lines = []
    lines.append(json.dumps({"sha256": digest(command), "issued": int(time.time())}))
    descriptor, temporary = tempfile.mkstemp(dir=path.parent, prefix=".user-commands-")
    with os.fdopen(descriptor, "w", encoding="utf-8", newline="\n") as stream:
        stream.write("\n".join(lines[-RECORD_LIMIT:]) + "\n")
    os.replace(temporary, path)


def copy(command: str) -> str:
    if copy_draft.IS_WINDOWS:
        return copy_draft.copy_windows(command, True)
    return copy_draft.copy_posix(command, True)


def issue(spec: Spec, environ: dict[str, str] | None = None, copier=copy) -> tuple[str, str]:
    command = render(spec)
    receipt = copier(command)
    record(command, environ)
    return command, receipt


def commands(reply: str) -> list[str]:
    found = []
    fence = None
    for line in reply.splitlines():
        stripped = line.strip()
        if fence is None:
            opened = FENCE.match(line)
            if opened:
                fence = (opened[1], opened[2].lower())
                continue
        elif stripped.startswith(fence[0]) and not stripped.strip(fence[0][0]):
            fence = None
            continue
        else:
            if fence[1] in SHELL_FENCES and COMMAND_START.match(stripped):
                found.append(stripped)
            continue
        stripped = LINE_MARKER.sub("", line, count=1).strip()
        if COMMAND_START.match(stripped):
            found.append(stripped)
            continue
        found += [span.strip() for _, span in INLINE_CODE.findall(line) if COMMAND_START.match(span.strip())]
    return found


def unissued(reply: str, environ: dict[str, str] | None = None) -> list[str]:
    known = issued(environ)
    return [command for command in commands(reply) if digest(command) not in known]


def installed_matches(directory: Path, source: Path) -> bool:
    try:
        return all((directory / name).read_bytes() == (source / name).read_bytes() for name in INSTALLED)
    except OSError:
        return False


def setup(check: bool, environ: dict[str, str] | None = None) -> None:
    source = Path(__file__).resolve().parent
    directory = install_directory(environ)
    if check:
        if not installed_matches(directory, source):
            raise UserCommandError(f"the user command builder in {directory} is missing or stale; run setup")
        return
    directory.mkdir(parents=True, exist_ok=True)
    for name in INSTALLED:
        shutil.copyfile(source / name, directory / name)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    commands_parser = parser.add_subparsers(dest="command", required=True)
    commands_parser.add_parser("issue").add_argument("--spec", required=True)
    commands_parser.add_parser("verify")
    setup_parser = commands_parser.add_parser("setup")
    setup_parser.add_argument("--check", action="store_true")
    setup_parser.add_argument("--onboard", action="store_true")
    arguments = parser.parse_args(argv)
    sys.stdout.reconfigure(encoding="utf-8")
    try:
        if arguments.command == "setup":
            setup(arguments.check)
            print("user_command: ready" if arguments.check else f"user_command: installed in {install_directory()}")
            return 0
        if arguments.command == "verify":
            missing = unissued(sys.stdin.buffer.read().decode("utf-8", errors="replace"))
            for command in missing:
                print(command)
            return 1 if missing else 0
        source = sys.stdin.buffer.read() if arguments.spec == "-" else Path(arguments.spec).read_bytes()
        raw = source.decode("utf-8-sig")
        command, receipt = issue(parse_spec(json.loads(raw)))
    except (UserCommandError, ValueError, OSError, copy_draft.ClipboardError) as error:
        print(f"user_command: {error}", file=sys.stderr)
        return 2
    print(command)
    print(receipt)
    return 0


if __name__ == "__main__":
    sys.exit(main())

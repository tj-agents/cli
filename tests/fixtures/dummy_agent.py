import json
import os
import signal
import subprocess
import sys
import time
from pathlib import Path

WINDOWS = os.name == "nt"


def note(line: str) -> None:
    with open(os.environ["DUMMY_LOG"], "a", encoding="utf-8") as log:
        log.write(line + "\n")


def graceful(*_: object) -> None:
    marker = Path(os.environ["DUMMY_LOG"]).with_name("dirty-on-stop")
    if marker.exists():
        Path(marker.read_text(encoding="utf-8")).write_text(
            "unfinished", encoding="utf-8"
        )
    note("graceful")
    sys.exit(0)


SHELLS: list[subprocess.Popen[bytes]] = []


def shell(start: bool) -> None:
    """Leave a shell running under this agent, as a background task does, or end it."""
    if start:
        command = (
            ["powershell", "-NoProfile", "-Command", "Start-Sleep 600"]
            if WINDOWS
            else ["sh", "-c", "sleep 600; true"]
        )
        SHELLS.append(subprocess.Popen(command))
    while not start and SHELLS:
        SHELLS.pop().kill()


def run_commands() -> None:
    """Run the tj commands a test queued, as this agent's own descendant, each exactly once."""
    commands = Path(os.environ["DUMMY_LOG"]).with_name("commands")
    ran = commands.with_name("commands.ran")
    if not commands.exists():
        return
    done = int(ran.read_text(encoding="utf-8") or 0) if ran.exists() else 0
    lines = commands.read_text(encoding="utf-8").splitlines()
    for number, line in enumerate(lines[done:], start=done):
        command = json.loads(line)
        if command[0] == "shell":
            shell(command[1] == "start")
            ran.write_text(str(number + 1), encoding="utf-8")
            note(f"ran {number} 0 ")
            continue
        result = subprocess.run(
            [os.environ["DUMMY_TJ"], *command], capture_output=True, text=True
        )
        ran.write_text(str(number + 1), encoding="utf-8")
        note(f"ran {number} {result.returncode} {result.stderr.strip()}")


def read_keys() -> None:
    typed = ""
    if WINDOWS:
        import ctypes
        import msvcrt

        kernel = ctypes.windll.kernel32
        console = kernel.CreateFileW("CONIN$", 0xC0000000, 3, None, 3, 0, None)
        kernel.SetConsoleMode(console, 0)
        note("raw")
        while True:
            while msvcrt.kbhit():
                key = msvcrt.getwch()
                if key == "\x03":
                    graceful()
                if key == "\r":
                    note(f"typed {typed}")
                    typed = ""
                else:
                    typed += key
            run_commands()
            time.sleep(0.05)
    import select
    import tty

    tty.setraw(sys.stdin.fileno())
    note("raw")
    while True:
        if select.select([sys.stdin], [], [], 0.05)[0]:
            key = os.read(sys.stdin.fileno(), 1).decode()
            if key == "\x03":
                graceful()
            if key == "\r":
                note(f"typed {typed}")
                typed = ""
            else:
                typed += key
        run_commands()


def main() -> None:
    mode = sys.argv[1]
    if mode == "grandchild":
        note(f"grandchild {os.getpid()}")
        while True:
            time.sleep(1)
    note(
        f"agent {os.environ['TJ_RUN_LAUNCH_ID']} {os.environ['AGENT_CLI_TAB_TITLE']} {os.getcwd()}"
    )
    note(f"wrapper {os.environ['TJ_RUN_PID']} {os.environ['TJ_RUN_STARTED_AT']}")
    note(f"pid {os.getpid()}")
    note(f"args {' '.join(sys.argv[2:])}")
    subprocess.Popen([sys.executable, __file__, "grandchild"])
    if mode == "exit":
        time.sleep(1)
        sys.exit(3)
    if mode == "keys":
        read_keys()
    stops = (
        [signal.SIGBREAK, signal.SIGINT] if WINDOWS else [signal.SIGINT, signal.SIGTERM]
    )
    for stop in stops:
        signal.signal(stop, graceful if mode == "signal" else signal.SIG_IGN)
    while True:
        run_commands()
        time.sleep(0.1)


main()

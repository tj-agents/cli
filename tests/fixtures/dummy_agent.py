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


def read_keys() -> None:
    if WINDOWS:
        import ctypes
        import msvcrt

        kernel = ctypes.windll.kernel32
        console = kernel.CreateFileW("CONIN$", 0xC0000000, 3, None, 3, 0, None)
        kernel.SetConsoleMode(console, 0)
        note("raw")
        while True:
            if msvcrt.kbhit() and msvcrt.getwch() == "\x03":
                graceful()
            time.sleep(0.05)
    import tty

    tty.setraw(sys.stdin.fileno())
    note("raw")
    while os.read(sys.stdin.fileno(), 1) != b"\x03":
        pass
    graceful()


def main() -> None:
    mode = sys.argv[1]
    if mode == "grandchild":
        note(f"grandchild {os.getpid()}")
        while True:
            time.sleep(1)
    note(
        f"agent {os.environ['TJ_RUN_LAUNCH_ID']} {os.environ['AGENT_CLI_TAB_TITLE']} {os.getcwd()}"
    )
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
        time.sleep(0.1)


main()

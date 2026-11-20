import os
import subprocess
import sys
import time
from pathlib import Path


def main() -> None:
    folder = Path(sys.argv[1])
    role, mode = sys.argv[2:4]
    if sys.platform != "win32" and role == "child":
        # Escape the original group: the dedicated helper must still adopt and reap us.
        os.setsid()
    (folder / f"{role}.pid").write_text(str(os.getpid()), encoding="utf-8")
    if role in {"root", "child"}:
        next_role = "child" if role == "root" else "grandchild"
        subprocess.Popen([sys.executable, __file__, str(folder), next_role, mode])
    while not (folder / "stop").exists():
        with (folder / f"{role}.writer").open("ab") as writer:
            writer.write(b"writing\n")
        if (
            mode == "root-exit"
            and role == "root"
            and (folder / "grandchild.writer").exists()
            and (folder / "grandchild.writer").stat().st_size > 0
        ):
            return
        time.sleep(0.02)


if __name__ == "__main__":
    main()

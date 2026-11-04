import ctypes
import json
import os
import sys
import time
from pathlib import Path

kernel = ctypes.WinDLL("kernel32")
user = ctypes.WinDLL("user32")
kernel.GetConsoleWindow.restype = ctypes.c_void_p
user.IsWindowVisible.argtypes = [ctypes.c_void_p]
console = kernel.GetConsoleWindow()
report = Path(sys.argv[1])
pending = report.with_suffix(".pending")
pending.write_text(
    json.dumps(
        {
            "pid": os.getpid(),
            "console": bool(console),
            "visible": bool(user.IsWindowVisible(console)),
        }
    ),
    encoding="utf-8",
)
pending.replace(report)
time.sleep(float(sys.argv[2]))

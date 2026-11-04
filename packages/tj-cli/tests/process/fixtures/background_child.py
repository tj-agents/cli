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
Path(sys.argv[1]).write_text(
    json.dumps(
        {
            "pid": os.getpid(),
            "console": bool(console),
            "visible": bool(user.IsWindowVisible(console)),
        }
    ),
    encoding="utf-8",
)
time.sleep(float(sys.argv[2]))

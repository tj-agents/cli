"""This machine's operating system and processor, read without a WMI query.

`platform.system()` and `platform.machine()` query WMI on Windows, which hangs while
the WMI service is unhealthy; a hook that asks then outlives its host's timeout.
These return the same names from the interpreter and the process environment.
"""

from __future__ import annotations

import os
import sys


def system() -> str:
    """`platform.system()`'s name: "Windows", "Linux", "Darwin" or the uname sysname."""
    if sys.platform == "win32":
        name = "Windows"
    elif sys.platform.startswith("linux"):
        name = "Linux"
    elif sys.platform == "darwin":
        name = "Darwin"
    else:
        name = os.uname().sysname
    return name


def machine() -> str:
    """`platform.machine()`'s name, such as "AMD64", "ARM64", "x86_64" or "aarch64"."""
    if sys.platform == "win32":
        name = os.environ.get("PROCESSOR_ARCHITEW6432") or os.environ.get(
            "PROCESSOR_ARCHITECTURE", ""
        )
    else:
        name = os.uname().machine
    return name

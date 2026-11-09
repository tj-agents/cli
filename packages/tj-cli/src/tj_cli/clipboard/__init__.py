"""Copy text onto the system clipboard as plain text, verified off the live clipboard
afterwards.

CF_UNICODETEXT on Windows, `wl-copy`/`xclip`/`xsel` on Linux and `pbcopy` on macOS. The
POSIX backend takes `run`/`which`/`environ` as arguments so callers and tests can inject
stubs without ever touching a real clipboard; the Windows backend talks to the live
clipboard directly and takes none.
"""

import shutil
import subprocess
import sys
from collections.abc import Mapping

from tj_cli.clipboard._errors import ClipboardError
from tj_cli.clipboard._posix import RunFunction, WhichFunction, copy_posix
from tj_cli.clipboard._text import normalise_text, read_text_file
from tj_cli.clipboard._windows import copy_windows

__all__ = ["ClipboardError", "copy", "normalise_text", "read_text_file"]


def copy(
    text: str,
    *,
    environ: Mapping[str, str] | None = None,
    which: WhichFunction = shutil.which,
    run: RunFunction = subprocess.run,
) -> str:
    """Copy `text` through this platform's backend and return "Copied N chars."."""
    if sys.platform == "win32":
        result = copy_windows(text)
    else:
        result = copy_posix(text, environ=environ, which=which, run=run)
    return result

#!/usr/bin/env python3
"""Copy a drafted file onto the system clipboard as plain text, verifying off the live clipboard afterwards.

Run as `python3 copy_draft.py --path <file>` (`python` on Windows); the shebang and exec bit are not relied
on. The text is copied exactly as written: CF_UNICODETEXT on Windows, `wl-copy`/`xclip`/`xsel` on Linux and
`pbcopy` on macOS, then read back and compared. Rendered Teams messages are `teams:message`'s job.

The clipboard backends take `run`/`which`/`environ` as arguments so tests can inject stubs without ever
touching a real clipboard.
"""

import argparse
import ctypes
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from collections.abc import Callable, Mapping, Sequence
from pathlib import Path
from typing import IO, Any, NamedTuple

IS_WINDOWS = os.name == 'nt'

RunFunction = Callable[..., 'subprocess.CompletedProcess[str]']
WhichFunction = Callable[[str], str | None]

GMEM_MOVEABLE = 0x0002
CF_UNICODETEXT = 13
HWND_MESSAGE = -3


class ClipboardError(Exception):
    """A copy or verification step failed; the message is the whole report."""


_TRAILING_WHITESPACE = re.compile(r'\s+\Z')


# --- draft file -----------------------------------------------------------------------------------


def normalise_draft(raw: str) -> str:
    """CRLF -> LF, then whitespace trimmed off only the very end of the text (not per line)."""
    text = raw.replace('\r\n', '\n')
    return _TRAILING_WHITESPACE.sub('', text)


def read_draft_file(path: str) -> str:
    """The normalised draft text, or a ClipboardError naming a missing, non-UTF-8 or empty draft.

    Read as `utf-8-sig` rather than `utf-8` so a leading BOM -- common from Windows editors -- is
    dropped instead of landing on the clipboard as a stray character.
    """
    resolved = Path(path)
    if not resolved.is_file():
        raise ClipboardError(f'Draft file not found: {path}')
    try:
        raw = resolved.read_text(encoding='utf-8-sig')
    except UnicodeDecodeError:
        raise ClipboardError(f'Draft file is not valid UTF-8: {path}') from None
    text = normalise_draft(raw)
    if not text.strip():
        raise ClipboardError(f'Draft file is empty: {path}')
    return text


# --- POSIX (Linux, macOS) backend ---------------------------------------------------------------


class PosixTool(NamedTuple):
    label: str
    copy_argv: list[str]
    paste_argv: list[str]
    env_overrides: dict[str, str]


_WL_COPY = PosixTool('wl-copy', ['wl-copy'], ['wl-paste', '--no-newline'], {})
_XCLIP = PosixTool('xclip', ['xclip', '-selection', 'clipboard'], ['xclip', '-selection', 'clipboard', '-o'], {})
_XSEL = PosixTool('xsel', ['xsel', '--clipboard', '--input'], ['xsel', '--clipboard', '--output'], {})
_PBCOPY = PosixTool(
    'pbcopy', ['pbcopy'], ['pbpaste'],
    # pbcopy/pbpaste pick their text encoding from the locale; forcing a UTF-8 one here is what keeps
    # non-ASCII draft text (accents, emoji) intact rather than silently mangled on some machines.
    {'LANG': 'en_US.UTF-8', 'LC_CTYPE': 'en_US.UTF-8'},
)


def posix_clipboard_tool(environ: Mapping[str, str], which: WhichFunction) -> PosixTool:
    """The PosixTool this environment selects, or a ClipboardError naming what to install.

    Wayland is checked first because a Wayland session under XWayland can still have DISPLAY set; without
    wl-copy/wl-paste on PATH, such a session falls back to the X11 tools rather than failing outright.
    Either way, nothing usable in a Wayland session is one error, naming wl-clipboard first (the native
    tool for the session actually running), and xclip/xsel only when DISPLAY is set, the one case they
    could be used.
    """
    wayland = bool(environ.get('WAYLAND_DISPLAY'))
    x11 = bool(environ.get('DISPLAY'))

    if wayland:
        if which('wl-copy') and which('wl-paste'):
            return _WL_COPY
        if x11:
            if which('xclip'):
                return _XCLIP
            if which('xsel'):
                return _XSEL
        fallback = ', or xclip/xsel as an XWayland fallback' if x11 else ''
        raise ClipboardError(
            f'No clipboard tool was found on PATH. Install wl-clipboard (provides wl-copy/wl-paste){fallback}.'
        )

    if x11:
        if which('xclip'):
            return _XCLIP
        if which('xsel'):
            return _XSEL
        raise ClipboardError('No X11 clipboard tool was found on PATH. Install xclip or xsel.')

    if sys.platform == 'darwin':
        if which('pbcopy') and which('pbpaste'):
            return _PBCOPY
        raise ClipboardError('pbcopy/pbpaste was not found on PATH.')

    raise ClipboardError(
        'No supported clipboard session was detected: neither WAYLAND_DISPLAY nor DISPLAY is set, and this is not macOS.'
    )


def _clipboard_failure(label: str, returncode: int, stderr: str | None) -> ClipboardError:
    detail = (stderr or '').strip()
    message = f'{label} failed (exit {returncode})'
    return ClipboardError(f'{message}: {detail}' if detail else f'{message}.')


_TRANSIENT_READBACK_PATTERNS = ('nothing is copied', 'no suitable type', 'target not available', 'no selection')


def _is_transient_readback_failure(stderr: str | None) -> bool:
    """Whether a nonzero read-back exit names a known "owner not ready yet" shape worth retrying.

    wl-paste says "Nothing is copied" when no owner has claimed the selection yet; xclip says "No suitable
    type" or "target not available" for the same instant; xsel says "no selection". Any other nonzero
    exit is treated as a real failure -- retrying an error this has no reason to believe is transient
    would just spend the whole retry budget repeating it before reporting the same thing anyway.
    """
    lowered = (stderr or '').lower()
    return any(pattern in lowered for pattern in _TRANSIENT_READBACK_PATTERNS)


def _run_clipboard_process(run: RunFunction, argv: list[str], label: str, env: Mapping[str, str] | None = None,
                            timeout: float = 10, input_text: str | None = None, capture_stdout: bool = False,
                            may_fork: bool = False) -> tuple[int, str, str]:
    """Runs one clipboard tool invocation; returns (returncode, stdout_text, stderr_text).

    Never raises for a nonzero exit -- that is left to the caller, since a read-back's nonzero exit can be
    routine while a just-started owner is still taking over (see `_is_transient_readback_failure`), not
    necessarily a real failure. Only a process that could not even be started or answer within the timeout
    becomes a ClipboardError here.

    stdout is discarded (DEVNULL) unless `capture_stdout` -- a copy command's stdout is never read.

    stderr is captured differently depending on `may_fork`. A copy command (`may_fork=True`) -- wl-copy,
    xclip, xsel -- forks a long-lived background process that goes on serving the selection after this
    call returns, and that fork inherits this call's stdio; a PIPE would stay open (and communicate() keep
    waiting on it) until that fork also exits, so a copy that actually succeeded immediately would hang
    until the timeout. A private temporary file has no such problem, so the failing tool's own stderr can
    still be reported. A read-back command never forks, so its stderr is captured the ordinary way, via
    PIPE, alongside its stdout.

    Both streams are decoded explicitly as UTF-8 with decoding errors replaced, rather than left to the
    process's locale -- the locale is exactly what forcing LANG/LC_CTYPE for pbcopy/pbpaste routes around.
    """
    kwargs: dict[str, Any] = dict(timeout=timeout, encoding='utf-8', errors='replace')
    if input_text is None:
        kwargs['stdin'] = subprocess.DEVNULL
    else:
        kwargs['input'] = input_text
    kwargs['stdout'] = subprocess.PIPE if capture_stdout else subprocess.DEVNULL
    if env is not None:
        kwargs['env'] = env

    stderr_file: IO[str] | None = (
        tempfile.TemporaryFile(mode='w+', encoding='utf-8', errors='replace') if may_fork else None
    )
    try:
        kwargs['stderr'] = stderr_file if stderr_file is not None else subprocess.PIPE
        try:
            result = run(argv, **kwargs)
        except subprocess.TimeoutExpired:
            raise ClipboardError(f'{label} did not answer within {timeout} seconds.') from None
        except OSError as exc:
            raise ClipboardError(f'{label} could not be run: {exc}') from None
        if stderr_file is not None:
            stderr_file.seek(0)
            stderr_text = stderr_file.read()
        else:
            stderr_text = result.stderr or ''
    finally:
        if stderr_file is not None:
            stderr_file.close()

    stdout_text = result.stdout if capture_stdout else ''
    return result.returncode, (stdout_text or ''), stderr_text


def _run_copy(run: RunFunction, argv: list[str], text: str, label: str, env: Mapping[str, str] | None = None,
              timeout: float = 10) -> None:
    returncode, _, stderr = _run_clipboard_process(
        run, argv, label, env=env, timeout=timeout, input_text=text, capture_stdout=False, may_fork=True,
    )
    if returncode != 0:
        raise _clipboard_failure(label, returncode, stderr)


def _read_back_with_retry(run: RunFunction, argv: list[str], label: str, is_ok: Callable[[str], bool],
                          env: Mapping[str, str] | None = None, attempts: int = 5,
                          total_timeout: float = 1.0) -> str:
    """Reads back up to `attempts` times, pausing between tries, until `is_ok` accepts the output.

    A nonzero exit is retried only when `_is_transient_readback_failure` recognises its stderr as the
    owner-not-ready-yet shape; any other nonzero exit fails immediately with that attempt's own stderr.
    If every retried attempt keeps exiting nonzero (transiently), the last one's tool failure is raised,
    carrying its stderr. If at least one attempt exits zero but its content is never accepted by `is_ok`,
    the last such zero-exit output is returned instead, so the caller raises its own content-mismatch
    message -- a read-back that worked but did not match is a different failure from the tool never
    answering at all.
    """
    delay = total_timeout / attempts
    returncode, stderr = 1, ''
    last_zero_exit_output: str | None = None
    for attempt in range(attempts):
        returncode, output, stderr = _run_clipboard_process(run, argv, label, env=env, capture_stdout=True)
        if returncode == 0:
            if is_ok(output):
                return output
            last_zero_exit_output = output
        elif not _is_transient_readback_failure(stderr):
            raise _clipboard_failure(label, returncode, stderr)
        if attempt < attempts - 1:
            time.sleep(delay)
    if last_zero_exit_output is None:
        raise _clipboard_failure(label, returncode, stderr)
    return last_zero_exit_output


def copy_posix(text: str, environ: Mapping[str, str] | None = None,
               which: WhichFunction = shutil.which, run: RunFunction = subprocess.run) -> str:
    """Copy `text` through the clipboard tool this environment selects, and verify it round-trips."""
    environ = os.environ if environ is None else environ
    tool = posix_clipboard_tool(environ, which)
    env = {**environ, **tool.env_overrides} if tool.env_overrides else None
    _run_copy(run, tool.copy_argv, text, f'{tool.label} copy', env=env)

    def plain_ok(output: str) -> bool:
        return _TRAILING_WHITESPACE.sub('', output) == text

    landed = _read_back_with_retry(run, tool.paste_argv, f'{tool.label} paste', plain_ok, env=env)
    if not plain_ok(landed):
        raise ClipboardError(f'Clipboard verification failed: {tool.label} did not read back what was copied.')
    return f'Copied {len(text)} chars.'


# --- Windows backend -----------------------------------------------------------------------------


def _windows_api() -> tuple[Any, Any]:
    # Imported and bound lazily: ctypes.windll only exists on Windows, and this module must still import
    # cleanly on Linux/macOS so the pure functions and the POSIX backend stay testable there.
    import ctypes.wintypes as wintypes

    windll: Any = getattr(ctypes, 'windll')
    user32 = windll.user32
    kernel32 = windll.kernel32

    user32.CreateWindowExW.argtypes = [
        wintypes.DWORD, wintypes.LPCWSTR, wintypes.LPCWSTR, wintypes.DWORD,
        ctypes.c_int, ctypes.c_int, ctypes.c_int, ctypes.c_int,
        wintypes.HWND, wintypes.HANDLE, wintypes.HANDLE, wintypes.LPVOID,
    ]
    user32.CreateWindowExW.restype = wintypes.HWND
    user32.DestroyWindow.argtypes = [wintypes.HWND]
    user32.DestroyWindow.restype = wintypes.BOOL

    user32.OpenClipboard.argtypes = [wintypes.HWND]
    user32.OpenClipboard.restype = wintypes.BOOL
    user32.CloseClipboard.argtypes = []
    user32.CloseClipboard.restype = wintypes.BOOL
    user32.EmptyClipboard.argtypes = []
    user32.EmptyClipboard.restype = wintypes.BOOL
    user32.GetClipboardData.argtypes = [wintypes.UINT]
    user32.GetClipboardData.restype = wintypes.HANDLE
    user32.SetClipboardData.argtypes = [wintypes.UINT, wintypes.HANDLE]
    user32.SetClipboardData.restype = wintypes.HANDLE
    user32.IsClipboardFormatAvailable.argtypes = [wintypes.UINT]
    user32.IsClipboardFormatAvailable.restype = wintypes.BOOL

    # HANDLE/HGLOBAL are explicitly pointer-sized (wintypes.HANDLE is c_void_p) rather than a 32-bit DWORD,
    # so these calls stay correct on 64-bit Windows.
    kernel32.GlobalAlloc.argtypes = [wintypes.UINT, ctypes.c_size_t]
    kernel32.GlobalAlloc.restype = wintypes.HANDLE
    kernel32.GlobalLock.argtypes = [wintypes.HANDLE]
    kernel32.GlobalLock.restype = ctypes.c_void_p
    kernel32.GlobalUnlock.argtypes = [wintypes.HANDLE]
    kernel32.GlobalUnlock.restype = wintypes.BOOL
    kernel32.GlobalSize.argtypes = [wintypes.HANDLE]
    kernel32.GlobalSize.restype = ctypes.c_size_t
    kernel32.GlobalFree.argtypes = [wintypes.HANDLE]
    kernel32.GlobalFree.restype = wintypes.HANDLE

    return user32, kernel32


def _create_message_window(user32: Any) -> Any:
    """A hidden message-only window, so SetClipboardData has a real owner to attach the data to.

    OpenClipboard(NULL) leaves the clipboard with no owning window at all; EmptyClipboard followed by
    SetClipboardData with no owner is exactly the shape that has been seen failing SetClipboardData, so a
    throwaway `STATIC`-class window parented to HWND_MESSAGE stands in as the owner instead. Raises before
    the caller ever opens or empties the clipboard, rather than falling back to OpenClipboard(NULL): a copy
    that cannot get a real owner is better refused than silently landed without the ownership semantics the
    rest of this backend was written to rely on.
    """
    import ctypes.wintypes as wintypes

    hwnd = user32.CreateWindowExW(0, 'STATIC', '', 0, 0, 0, 0, 0, wintypes.HWND(HWND_MESSAGE), None, None, None)
    if not hwnd:
        raise ClipboardError('CreateWindowExW failed while preparing a clipboard owner window.')
    return hwnd


def _open_clipboard(user32: Any, hwnd: Any, retries: int = 10, delay: float = 0.05) -> None:
    # Another process (often the previous clipboard owner tearing down) can hold the clipboard for a
    # moment; a short retry loop rides that out instead of failing a copy that would succeed a beat later.
    for _ in range(retries):
        if user32.OpenClipboard(hwnd):
            return
        time.sleep(delay)
    raise ClipboardError('Could not open the clipboard; another process held it.')


def _set_clipboard_bytes(user32: Any, kernel32: Any, clipboard_format: int, data: bytes) -> None:
    size = len(data)
    handle = kernel32.GlobalAlloc(GMEM_MOVEABLE, ctypes.c_size_t(size))
    if not handle:
        raise ClipboardError('GlobalAlloc failed while preparing clipboard data.')
    pointer = kernel32.GlobalLock(handle)
    if not pointer:
        kernel32.GlobalFree(handle)
        raise ClipboardError('GlobalLock failed while preparing clipboard data.')
    try:
        ctypes.memmove(pointer, data, size)
    finally:
        kernel32.GlobalUnlock(handle)
    # Ownership passes to the system only once SetClipboardData succeeds; on failure this call still owns
    # the handle and must free it, or it leaks.
    if not user32.SetClipboardData(clipboard_format, handle):
        kernel32.GlobalFree(handle)
        raise ClipboardError('SetClipboardData failed.')


def _read_windows_clipboard(user32: Any, kernel32: Any, hwnd: Any) -> str | None:
    _open_clipboard(user32, hwnd)
    try:
        if not user32.IsClipboardFormatAvailable(CF_UNICODETEXT):
            return None
        handle = user32.GetClipboardData(CF_UNICODETEXT)
        if not handle:
            return None
        pointer = kernel32.GlobalLock(handle)
        if not pointer:
            return None
        try:
            return ctypes.wstring_at(pointer)
        finally:
            kernel32.GlobalUnlock(handle)
    finally:
        user32.CloseClipboard()


def copy_windows(text: str) -> str:
    """Copy `text` on Windows as CF_UNICODETEXT via ctypes, then verify it off the live clipboard."""
    user32, kernel32 = _windows_api()
    # Raises before the clipboard is ever opened or emptied if no owner window can be created.
    hwnd = _create_message_window(user32)
    try:
        _open_clipboard(user32, hwnd)
        try:
            if not user32.EmptyClipboard():
                raise ClipboardError('EmptyClipboard failed.')
            _set_clipboard_bytes(user32, kernel32, CF_UNICODETEXT, text.encode('utf-16-le') + b'\x00\x00')
        finally:
            user32.CloseClipboard()
        landed = _read_windows_clipboard(user32, kernel32, hwnd)
    finally:
        user32.DestroyWindow(hwnd)
    if _TRAILING_WHITESPACE.sub('', landed or '') != text:
        raise ClipboardError('Clipboard verification failed: the text did not round-trip.')
    return f'Copied {len(text)} chars.'


# --- CLI -------------------------------------------------------------------------------------------


def parse_args(argv: Sequence[str] | None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description='Copy a drafted file onto the system clipboard as plain text.')
    parser.add_argument('--path', required=True, help='Path to the UTF-8 draft file to copy.')
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        text = read_draft_file(str(args.path))
        print(copy_windows(text) if IS_WINDOWS else copy_posix(text))
        return 0
    except ClipboardError as exc:
        print(str(exc), file=sys.stderr)
        return 1


if __name__ == '__main__':
    sys.exit(main())

"""tj_cli.clipboard._windows: exercises the real Windows clipboard via ctypes. Windows
only -- never runs, and never touches a real clipboard, on this (Linux) machine.
"""

import os
from typing import Any
from unittest import mock

import pytest

from tj_cli.clipboard import ClipboardError
from tj_cli.clipboard._windows import (
    _create_message_window,
    _read_windows_clipboard,
    _windows_api,
    copy_windows,
)

pytestmark = pytest.mark.skipif(
    os.name != "nt", reason="Windows native clipboard via ctypes"
)


def _read_clipboard(user32: Any, kernel32: Any) -> str | None:
    hwnd = _create_message_window(user32)
    try:
        return _read_windows_clipboard(user32, kernel32, hwnd)
    finally:
        user32.DestroyWindow(hwnd)


def test_the_literal_text_round_trips_through_the_real_clipboard() -> None:
    text = "a literal `command` to run\n\nsecond paragraph [link](https://example.com)"
    assert copy_windows(text) == f"Copied {len(text)} chars."

    user32, kernel32 = _windows_api()
    assert _read_clipboard(user32, kernel32) == text


def test_a_message_window_failure_is_raised_before_the_clipboard_is_touched() -> None:
    user32, _kernel32 = _windows_api()
    with (
        mock.patch.object(user32, "CreateWindowExW", return_value=0),
        mock.patch.object(user32, "OpenClipboard") as open_clipboard,
        pytest.raises(ClipboardError, match="CreateWindowExW failed"),
    ):
        copy_windows("hello")
    open_clipboard.assert_not_called()

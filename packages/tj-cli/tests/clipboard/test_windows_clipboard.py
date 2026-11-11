# ruff: noqa: N802 -- these fakes match user32/kernel32's own PascalCase method names,
# which copy_windows calls by that exact name; renaming them would break the fake.
"""tj_cli.clipboard._windows: the Windows backend driven entirely through a fake
`windows_api`, so this never touches a real clipboard and runs on every platform, not
only Windows.
"""

import ctypes

import pytest

from tj_cli.clipboard import ClipboardError
from tj_cli.clipboard._windows import CF_UNICODETEXT, copy_windows


class FakeUser32:
    """A fake `user32`: a hidden window is just an incrementing handle, and the
    clipboard is a plain dict from format to a kernel32 memory handle -- enough
    surface for `copy_windows` without ever calling a real Win32 API.
    """

    def __init__(self) -> None:
        self._clipboard: dict[int, int] = {}
        self._next_hwnd = 1
        self.open_clipboard_calls = 0

    def CreateWindowExW(self, *_args: object, **_kwargs: object) -> int:
        hwnd = self._next_hwnd
        self._next_hwnd += 1
        return hwnd

    def DestroyWindow(self, _hwnd: int) -> bool:
        return True

    def OpenClipboard(self, _hwnd: int) -> bool:
        self.open_clipboard_calls += 1
        return True

    def CloseClipboard(self) -> bool:
        return True

    def EmptyClipboard(self) -> bool:
        self._clipboard.clear()
        return True

    def SetClipboardData(self, clipboard_format: int, handle: int) -> int:
        self._clipboard[clipboard_format] = handle
        return handle

    def GetClipboardData(self, clipboard_format: int) -> int:
        return self._clipboard.get(clipboard_format, 0)

    def IsClipboardFormatAvailable(self, clipboard_format: int) -> bool:
        return clipboard_format in self._clipboard


class FailingCreateWindowUser32(FakeUser32):
    """A FakeUser32 whose CreateWindowExW fails, the one path `copy_windows` must refuse
    before ever touching the clipboard.
    """

    def CreateWindowExW(self, *_args: object, **_kwargs: object) -> int:
        return 0


class FakeKernel32:
    """A fake `kernel32`: `GlobalAlloc` hands out a real ctypes buffer and uses its
    address as the handle, so the genuine `ctypes.memmove`/`ctypes.string_at` calls
    in `_set_clipboard_bytes`/`_read_windows_clipboard` round-trip real bytes without
    any actual Win32 call.
    """

    def __init__(self) -> None:
        self._buffers: dict[int, ctypes.Array[ctypes.c_char]] = {}

    def GlobalAlloc(self, _flags: int, size: ctypes.c_size_t) -> int:
        buffer = ctypes.create_string_buffer(size.value)
        handle = ctypes.addressof(buffer)
        self._buffers[handle] = buffer
        return handle

    def GlobalLock(self, handle: int) -> int:
        return handle if handle in self._buffers else 0

    def GlobalUnlock(self, _handle: int) -> bool:
        return True

    def GlobalSize(self, handle: int) -> int:
        return len(self._buffers[handle])

    def GlobalFree(self, handle: int) -> int:
        self._buffers.pop(handle, None)
        return 0


def test_the_literal_text_round_trips_through_a_fake_clipboard() -> None:
    user32, kernel32 = FakeUser32(), FakeKernel32()
    text = "a literal `command` to run\n\nsecond paragraph [link](https://example.com)"

    message = copy_windows(text, windows_api=lambda: (user32, kernel32))

    assert message == f"Copied {len(text)} chars."
    assert user32.IsClipboardFormatAvailable(CF_UNICODETEXT)


def test_a_message_window_failure_is_raised_before_the_clipboard_is_touched() -> None:
    user32, kernel32 = FailingCreateWindowUser32(), FakeKernel32()

    with pytest.raises(ClipboardError, match="CreateWindowExW failed"):
        copy_windows("hello", windows_api=lambda: (user32, kernel32))

    assert user32.open_clipboard_calls == 0

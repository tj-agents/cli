"""tj_cli.clipboard.copy: picks a backend by platform. Never touches a real clipboard --
the POSIX backend receives a fake `run`/`which`, and the Windows backend is replaced
outright.
"""

import sys
from unittest import mock

import pytest

import tj_cli.clipboard as clipboard_package
from tj_cli.clipboard import copy


def test_copy_dispatches_to_the_posix_backend_off_windows(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(sys, "platform", "linux")
    with mock.patch.object(
        clipboard_package, "copy_posix", return_value="Copied 5 chars."
    ) as posix:
        assert (
            copy("hello", environ={"DISPLAY": ":0"}, which=lambda _name: None)
            == "Copied 5 chars."
        )
    posix.assert_called_once()


def test_copy_dispatches_to_the_windows_backend_on_windows(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(sys, "platform", "win32")
    with mock.patch.object(
        clipboard_package, "copy_windows", return_value="Copied 5 chars."
    ) as windows:
        assert copy("hello") == "Copied 5 chars."
    windows.assert_called_once_with("hello")

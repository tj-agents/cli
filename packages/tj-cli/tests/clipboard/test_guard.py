"""tj_cli.clipboard._guard: refuses the real backend whenever PYTEST_CURRENT_TEST is set
-- which it always is here, so calling a backend with its real default proves the
refusal rather than ever touching a real clipboard.
"""

import pytest

from tj_cli.clipboard import ClipboardError, copy
from tj_cli.clipboard._guard import refuse_real_backend_in_tests
from tj_cli.clipboard._posix import copy_posix
from tj_cli.clipboard._windows import copy_windows

_MATCH = "Refusing to use the real system clipboard"


def test_refuse_real_backend_in_tests_raises_for_a_real_backend() -> None:
    with pytest.raises(ClipboardError, match="PYTEST_CURRENT_TEST"):
        refuse_real_backend_in_tests(True)


def test_refuse_real_backend_in_tests_is_a_noop_for_a_fake_backend() -> None:
    refuse_real_backend_in_tests(False)


def test_copy_posix_refuses_its_real_default_run() -> None:
    with pytest.raises(ClipboardError, match=_MATCH):
        copy_posix("hello")


def test_copy_windows_refuses_its_real_default_backend() -> None:
    with pytest.raises(ClipboardError, match=_MATCH):
        copy_windows("hello")


def test_copy_refuses_the_real_backend_on_either_platform() -> None:
    with pytest.raises(ClipboardError, match=_MATCH):
        copy("hello")

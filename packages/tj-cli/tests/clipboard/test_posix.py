"""tj_cli.clipboard._posix: POSIX backend selection and argv. Never writes to this
machine's real clipboard, and never touches the primary selection either: this
machine has a live Wayland session, so these tests inject a fake
`run`/`which`/`environ` rather than calling wl-copy/wl-paste/xclip/xsel/pbcopy for
real.
"""

import subprocess
import sys
import time
from collections.abc import Callable
from dataclasses import dataclass

import pytest

from tj_cli.clipboard import ClipboardError
from tj_cli.clipboard._posix import (
    _is_transient_readback_failure,
    copy_posix,
    posix_clipboard_tool,
)


@pytest.fixture(autouse=True)
def _no_real_sleep(monkeypatch: pytest.MonkeyPatch) -> None:
    # The retry loop sleeps for real between attempts; stubbed out so a mismatch test
    # does not cost up to a second of wall-clock time for nothing.
    monkeypatch.setattr(time, "sleep", lambda _seconds: None)


def which_only(*names: str) -> Callable[[str], str | None]:
    allowed = set(names)
    return lambda name: f"/usr/bin/{name}" if name in allowed else None


@dataclass(frozen=True, slots=True)
class FakeResult:
    returncode: int = 0
    stdout: str = ""
    stderr: str = ""


class FakeRun:
    """A fake `subprocess.run`: records every call, and -- like the real thing -- writes
    stderr into whatever file-like object the call passed as `stderr=` (a copy
    command's tempfile); when `stderr=` is `subprocess.PIPE` instead (a read-back
    command), it is left alone and the `FakeResult.stderr` reaches the caller as
    `result.stderr`, exactly as a real PIPE capture would.
    """

    def __init__(self, results: list[FakeResult]) -> None:
        self._results = results
        self.calls: list[tuple[list[str], dict[str, object]]] = []

    def __call__(self, argv: list[str], **kwargs: object) -> FakeResult:
        self.calls.append((list(argv), kwargs))
        result = self._results[len(self.calls) - 1]
        sink = kwargs.get("stderr")
        if sink is not None and hasattr(sink, "write"):
            sink.write(result.stderr)
        return result


# --- posix_clipboard_tool selection ---------------------------------------------------


def test_wayland_session_selects_wl_copy() -> None:
    tool = posix_clipboard_tool(
        {"WAYLAND_DISPLAY": "wayland-0"}, which_only("wl-copy", "wl-paste")
    )
    assert tool.label == "wl-copy"
    assert tool.copy_argv == ["wl-copy"]
    assert tool.paste_argv == ["wl-paste", "--no-newline"]
    assert tool.env_overrides == {}


def test_wayland_without_wl_clipboard_or_display_names_the_package() -> None:
    with pytest.raises(ClipboardError, match="wl-clipboard") as raised:
        posix_clipboard_tool({"WAYLAND_DISPLAY": "wayland-0"}, which_only())
    # Without DISPLAY the X11 tools are never selected, so recommending them would
    # mislead.
    assert "xclip" not in str(raised.value)


def test_wayland_without_wl_copy_falls_back_to_xclip_with_display_set() -> None:
    # XWayland means a Wayland session can still have DISPLAY set; missing wl-copy is
    # not fatal there.
    tool = posix_clipboard_tool(
        {"WAYLAND_DISPLAY": "wayland-0", "DISPLAY": ":0"}, which_only("xclip")
    )
    assert tool.label == "xclip"


def test_wayland_without_wl_copy_falls_back_to_xsel_with_display_set() -> None:
    tool = posix_clipboard_tool(
        {"WAYLAND_DISPLAY": "wayland-0", "DISPLAY": ":0"}, which_only("xsel")
    )
    assert tool.label == "xsel"


def test_wayland_and_x11_with_nothing_available_recommends_wl_clipboard() -> None:
    # Wayland is the session actually running, so it is named first even though DISPLAY
    # (XWayland) is also set; xclip/xsel are named only as that fallback. One check, one
    # message, either way.
    with pytest.raises(ClipboardError, match=r"wl-clipboard.*xclip"):
        posix_clipboard_tool(
            {"WAYLAND_DISPLAY": "wayland-0", "DISPLAY": ":0"}, which_only()
        )


def test_x11_session_prefers_xclip() -> None:
    tool = posix_clipboard_tool({"DISPLAY": ":0"}, which_only("xclip", "xsel"))
    assert tool.label == "xclip"
    assert tool.copy_argv == ["xclip", "-selection", "clipboard"]
    assert tool.paste_argv == ["xclip", "-selection", "clipboard", "-o"]


def test_x11_session_falls_back_to_xsel() -> None:
    tool = posix_clipboard_tool({"DISPLAY": ":0"}, which_only("xsel"))
    assert tool.label == "xsel"
    assert tool.copy_argv == ["xsel", "--clipboard", "--input"]
    assert tool.paste_argv == ["xsel", "--clipboard", "--output"]


def test_x11_session_without_either_tool_is_an_error() -> None:
    with pytest.raises(ClipboardError, match="xclip or xsel"):
        posix_clipboard_tool({"DISPLAY": ":0"}, which_only())


def test_darwin_selects_pbcopy(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(sys, "platform", "darwin")
    tool = posix_clipboard_tool({}, which_only("pbcopy", "pbpaste"))
    assert tool.label == "pbcopy"
    assert tool.copy_argv == ["pbcopy"]
    assert tool.paste_argv == ["pbpaste"]
    assert tool.env_overrides == {"LANG": "en_US.UTF-8", "LC_CTYPE": "en_US.UTF-8"}


def test_no_session_detected_is_an_error(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(sys, "platform", "linux")
    with pytest.raises(ClipboardError, match="No supported clipboard session"):
        posix_clipboard_tool({}, which_only("wl-copy", "xclip", "pbcopy"))


# --- _is_transient_readback_failure -------------------------------------------------


@pytest.mark.parametrize(
    "stderr",
    [
        "Nothing is copied",
        "NOTHING IS COPIED",
        "No suitable type",
        "target not available",
        "no selection",
    ],
)
def test_known_transient_shapes_are_recognised_case_insensitively(stderr: str) -> None:
    assert _is_transient_readback_failure(stderr) is True


def test_an_unrelated_error_is_not_transient() -> None:
    assert _is_transient_readback_failure("Permission denied") is False


@pytest.mark.parametrize("stderr", ["", None])
def test_empty_or_none_stderr_is_not_transient(stderr: str | None) -> None:
    assert _is_transient_readback_failure(stderr) is False


# --- copy_posix: argv/stdin/env it sends -- a fake `run`, never a real clipboard ---


def test_pbcopy_and_pbpaste_run_with_a_utf8_locale_env(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(sys, "platform", "darwin")
    run = FakeRun([FakeResult(stdout=""), FakeResult(stdout="café")])
    copy_posix(
        "café",
        environ={"PATH": "/usr/bin"},
        which=which_only("pbcopy", "pbpaste"),
        run=run,
    )
    copy_env = run.calls[0][1]["env"]
    paste_env = run.calls[1][1]["env"]
    assert isinstance(copy_env, dict)
    assert isinstance(paste_env, dict)
    assert copy_env["LANG"] == "en_US.UTF-8"
    assert copy_env["LC_CTYPE"] == "en_US.UTF-8"
    assert copy_env["PATH"] == "/usr/bin"
    assert paste_env["LANG"] == "en_US.UTF-8"


def test_wl_copy_copies_the_literal_text() -> None:
    run = FakeRun([FakeResult(stdout=""), FakeResult(stdout="run `az login` now")])
    message = copy_posix(
        "run `az login` now",
        environ={"WAYLAND_DISPLAY": "wayland-0"},
        which=which_only("wl-copy", "wl-paste"),
        run=run,
    )
    copy_argv, copy_kwargs = run.calls[0]
    assert copy_argv == ["wl-copy"]
    assert copy_kwargs["input"] == "run `az login` now"
    assert "Copied" in message


def test_backticks_travel_intact() -> None:
    run = FakeRun([FakeResult(stdout=""), FakeResult(stdout="run `az login` now")])
    copy_posix(
        "run `az login` now",
        environ={"DISPLAY": ":0"},
        which=which_only("xclip"),
        run=run,
    )
    assert run.calls[0][1]["input"] == "run `az login` now"


def test_trailing_whitespace_in_readback_is_stripped_before_comparing() -> None:
    run = FakeRun([FakeResult(stdout=""), FakeResult(stdout="hello world   \n")])
    message = copy_posix(
        "hello world",
        environ={"WAYLAND_DISPLAY": "wayland-0"},
        which=which_only("wl-copy", "wl-paste"),
        run=run,
    )
    assert "Copied 11 chars" in message


def test_the_plain_readback_is_retried_past_a_stale_selection_before_matching() -> None:
    run = FakeRun(
        [FakeResult(stdout=""), FakeResult(stdout=""), FakeResult(stdout="hello world")]
    )
    message = copy_posix(
        "hello world", environ={"DISPLAY": ":0"}, which=which_only("xclip"), run=run
    )
    assert len(run.calls) == 3
    assert "Copied" in message


def test_readback_retries_past_a_transient_nonzero_exit_before_succeeding() -> None:
    run = FakeRun(
        [
            FakeResult(stdout=""),
            FakeResult(
                returncode=1, stderr="Nothing is copied"
            ),  # wl-paste with no owner answering yet
            FakeResult(stdout="hello world"),
        ]
    )
    message = copy_posix(
        "hello world",
        environ={"WAYLAND_DISPLAY": "wayland-0"},
        which=which_only("wl-copy", "wl-paste"),
        run=run,
    )
    assert len(run.calls) == 3
    assert "Copied" in message


def test_readback_retries_past_a_transient_exit_case_insensitively() -> None:
    run = FakeRun(
        [
            FakeResult(stdout=""),
            FakeResult(returncode=1, stderr="NOTHING IS COPIED"),
            FakeResult(stdout="hello world"),
        ]
    )
    message = copy_posix(
        "hello world",
        environ={"WAYLAND_DISPLAY": "wayland-0"},
        which=which_only("wl-copy", "wl-paste"),
        run=run,
    )
    assert len(run.calls) == 3
    assert "Copied" in message


def test_a_non_transient_nonzero_readback_fails_immediately_without_retrying() -> None:
    run = FakeRun(
        [FakeResult(stdout=""), FakeResult(returncode=1, stderr="Permission denied")]
    )
    with pytest.raises(ClipboardError, match="Permission denied"):
        copy_posix(
            "hello world",
            environ={"WAYLAND_DISPLAY": "wayland-0"},
            which=which_only("wl-copy", "wl-paste"),
            run=run,
        )
    assert (
        len(run.calls) == 2
    )  # the copy, and exactly one read-back attempt -- no retry


def test_a_plain_readback_mismatch_is_an_error_after_retries_are_exhausted() -> None:
    run = FakeRun([FakeResult(stdout="")] + [FakeResult(stdout="something else")] * 5)
    with pytest.raises(ClipboardError, match="did not read back"):
        copy_posix(
            "hello world",
            environ={"WAYLAND_DISPLAY": "wayland-0"},
            which=which_only("wl-copy", "wl-paste"),
            run=run,
        )


def test_a_readback_that_always_exits_nonzero_reports_the_last_stderr() -> None:
    run = FakeRun(
        [FakeResult(stdout="")]
        + [FakeResult(returncode=1, stderr="Nothing is copied")] * 5
    )
    with pytest.raises(ClipboardError, match="Nothing is copied"):
        copy_posix(
            "hello world",
            environ={"WAYLAND_DISPLAY": "wayland-0"},
            which=which_only("wl-copy", "wl-paste"),
            run=run,
        )


def test_a_wrong_content_attempt_wins_over_later_transient_failures() -> None:
    # One attempt actually read back the wrong content; the rest merely failed to
    # answer at all.
    # The content mismatch is the more informative -- and more correct -- failure to
    # report.
    run = FakeRun(
        [
            FakeResult(stdout=""),
            FakeResult(stdout="something else"),
            FakeResult(returncode=1, stderr="Nothing is copied"),
            FakeResult(returncode=1, stderr="Nothing is copied"),
            FakeResult(returncode=1, stderr="Nothing is copied"),
            FakeResult(returncode=1, stderr="Nothing is copied"),
        ]
    )
    with pytest.raises(ClipboardError, match="did not read back what was copied"):
        copy_posix(
            "hello world",
            environ={"WAYLAND_DISPLAY": "wayland-0"},
            which=which_only("wl-copy", "wl-paste"),
            run=run,
        )


def test_copy_command_discards_stdout_captures_stderr_via_tempfile_not_pipe() -> None:
    run = FakeRun([FakeResult(stdout=""), FakeResult(stdout="hello world")])
    copy_posix(
        "hello world",
        environ={"WAYLAND_DISPLAY": "wayland-0"},
        which=which_only("wl-copy", "wl-paste"),
        run=run,
    )
    copy_kwargs = run.calls[0][1]
    assert copy_kwargs["stdout"] == subprocess.DEVNULL
    assert copy_kwargs["stderr"] not in (subprocess.DEVNULL, subprocess.PIPE)
    assert hasattr(copy_kwargs["stderr"], "write")
    assert "stdin" not in copy_kwargs
    assert copy_kwargs["encoding"] == "utf-8"
    assert copy_kwargs["errors"] == "replace"


def test_readback_command_captures_stdout_and_stderr_via_pipes_not_a_tempfile() -> None:
    # A read-back command never forks a persistent server, so there is no hang risk to
    # route
    # around with a tempfile the way the copy command's stderr is.
    run = FakeRun([FakeResult(stdout=""), FakeResult(stdout="hello world")])
    copy_posix(
        "hello world",
        environ={"WAYLAND_DISPLAY": "wayland-0"},
        which=which_only("wl-copy", "wl-paste"),
        run=run,
    )
    paste_kwargs = run.calls[1][1]
    assert paste_kwargs["stdout"] == subprocess.PIPE
    assert paste_kwargs["stderr"] == subprocess.PIPE
    assert paste_kwargs["stdin"] == subprocess.DEVNULL
    assert paste_kwargs["encoding"] == "utf-8"
    assert paste_kwargs["errors"] == "replace"


def test_a_nonzero_exit_from_the_copy_tool_is_an_error_including_its_stderr() -> None:
    run = FakeRun([FakeResult(returncode=1, stderr="no selection owner")])
    with pytest.raises(
        ClipboardError, match=r"wl-copy copy failed.*no selection owner"
    ):
        copy_posix(
            "hello world",
            environ={"WAYLAND_DISPLAY": "wayland-0"},
            which=which_only("wl-copy", "wl-paste"),
            run=run,
        )


def test_a_timeout_from_the_copy_tool_is_an_error() -> None:
    def run(argv: list[str], **kwargs: object) -> FakeResult:
        raise subprocess.TimeoutExpired(cmd=argv, timeout=10)

    with pytest.raises(ClipboardError, match="did not answer within"):
        copy_posix(
            "hello world",
            environ={"WAYLAND_DISPLAY": "wayland-0"},
            which=which_only("wl-copy", "wl-paste"),
            run=run,
        )


def test_no_clipboard_tool_found_is_an_error_naming_the_package() -> None:
    with pytest.raises(ClipboardError, match="wl-clipboard"):
        copy_posix(
            "hello world",
            environ={"WAYLAND_DISPLAY": "wayland-0"},
            which=which_only(),
            run=FakeRun([]),
        )

"""copy_draft.py: draft reading, POSIX backend selection/argv, and (Windows-only) a real ctypes round trip.

Never writes to this machine's real clipboard, and never touches the primary selection either: this
machine has a live Wayland session, so the POSIX backend tests inject a fake `run`/`which`/`environ`
rather than calling wl-copy/wl-paste/xclip/xsel/pbcopy for real.
"""
import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / '.agents/cli/utility/clip/scripts/copy_draft.py'
SPEC = importlib.util.spec_from_file_location('copy_draft', SCRIPT)
COPY_DRAFT = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = COPY_DRAFT
SPEC.loader.exec_module(COPY_DRAFT)


class NormaliseDraftTests(unittest.TestCase):
    def test_crlf_becomes_lf(self):
        self.assertEqual(COPY_DRAFT.normalise_draft('a\r\nb\r\n'), 'a\nb')

    def test_trailing_whitespace_is_trimmed_from_the_very_end_only(self):
        # The blank line between paragraphs is NOT trailing whitespace and must survive; only whitespace
        # at the absolute end of the text is removed, matching copy-draft.ps1's non-multiline '\s+$'.
        self.assertEqual(COPY_DRAFT.normalise_draft('line1\n\nline2\n   '), 'line1\n\nline2')

    def test_a_blank_text_normalises_to_empty(self):
        self.assertEqual(COPY_DRAFT.normalise_draft('   \n\n  '), '')


class ReadDraftFileTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='copy draft ')
        self.addCleanup(self.temp.cleanup)

    def test_a_missing_file_is_an_error(self):
        missing = str(Path(self.temp.name) / 'nope.txt')
        with self.assertRaisesRegex(COPY_DRAFT.ClipboardError, 'not found'):
            COPY_DRAFT.read_draft_file(missing)

    def test_an_empty_file_is_an_error(self):
        path = Path(self.temp.name) / 'empty.txt'
        path.write_text('   \n\n  ', encoding='utf-8')
        with self.assertRaisesRegex(COPY_DRAFT.ClipboardError, 'empty'):
            COPY_DRAFT.read_draft_file(str(path))

    def test_a_real_file_is_read_as_utf8_and_normalised(self):
        path = Path(self.temp.name) / 'draft.txt'
        path.write_bytes('café é\r\nsecond line\r\n'.encode('utf-8'))
        self.assertEqual(COPY_DRAFT.read_draft_file(str(path)), 'café é\nsecond line')

    def test_a_leading_bom_is_dropped(self):
        path = Path(self.temp.name) / 'bom.txt'
        path.write_bytes(b'\xef\xbb\xbf' + 'hello\nworld'.encode('utf-8'))
        self.assertEqual(COPY_DRAFT.read_draft_file(str(path)), 'hello\nworld')

    def test_non_utf8_bytes_are_a_clear_one_line_error(self):
        path = Path(self.temp.name) / 'latin1.txt'
        path.write_bytes('café'.encode('latin-1'))  # 0xe9 with no continuation byte is invalid UTF-8.
        with self.assertRaisesRegex(COPY_DRAFT.ClipboardError, 'not valid UTF-8'):
            COPY_DRAFT.read_draft_file(str(path))














def which_only(*names):
    allowed = set(names)
    return lambda name: f'/usr/bin/{name}' if name in allowed else None


class PosixClipboardToolTests(unittest.TestCase):
    def test_wayland_session_selects_wl_copy(self):
        tool = COPY_DRAFT.posix_clipboard_tool({'WAYLAND_DISPLAY': 'wayland-0'}, which_only('wl-copy', 'wl-paste'))
        self.assertEqual(tool.label, 'wl-copy')
        self.assertEqual(tool.copy_argv, ['wl-copy'])
        self.assertEqual(tool.paste_argv, ['wl-paste', '--no-newline'])
        self.assertEqual(tool.env_overrides, {})

    def test_wayland_session_without_wl_clipboard_and_no_display_is_an_error_naming_the_package(self):
        with self.assertRaisesRegex(COPY_DRAFT.ClipboardError, 'wl-clipboard') as raised:
            COPY_DRAFT.posix_clipboard_tool({'WAYLAND_DISPLAY': 'wayland-0'}, which_only())
        # Without DISPLAY the X11 tools are never selected, so recommending them would mislead.
        self.assertNotIn('xclip', str(raised.exception))

    def test_wayland_session_without_wl_copy_falls_back_to_xclip_when_display_is_also_set(self):
        # XWayland means a Wayland session can still have DISPLAY set; missing wl-copy is not fatal there.
        tool = COPY_DRAFT.posix_clipboard_tool(
            {'WAYLAND_DISPLAY': 'wayland-0', 'DISPLAY': ':0'}, which_only('xclip')
        )
        self.assertEqual(tool.label, 'xclip')

    def test_wayland_session_without_wl_copy_falls_back_to_xsel_when_display_is_also_set(self):
        tool = COPY_DRAFT.posix_clipboard_tool(
            {'WAYLAND_DISPLAY': 'wayland-0', 'DISPLAY': ':0'}, which_only('xsel')
        )
        self.assertEqual(tool.label, 'xsel')

    def test_wayland_and_x11_session_with_nothing_available_recommends_wl_clipboard_first(self):
        # Wayland is the session actually running, so it is named first even though DISPLAY (XWayland) is
        # also set; xclip/xsel are named only as that fallback. One check, one message, either way.
        with self.assertRaisesRegex(COPY_DRAFT.ClipboardError, r'wl-clipboard.*xclip'):
            COPY_DRAFT.posix_clipboard_tool({'WAYLAND_DISPLAY': 'wayland-0', 'DISPLAY': ':0'}, which_only())

    def test_x11_session_prefers_xclip(self):
        tool = COPY_DRAFT.posix_clipboard_tool({'DISPLAY': ':0'}, which_only('xclip', 'xsel'))
        self.assertEqual(tool.label, 'xclip')
        self.assertEqual(tool.copy_argv, ['xclip', '-selection', 'clipboard'])
        self.assertEqual(tool.paste_argv, ['xclip', '-selection', 'clipboard', '-o'])

    def test_x11_session_falls_back_to_xsel(self):
        tool = COPY_DRAFT.posix_clipboard_tool({'DISPLAY': ':0'}, which_only('xsel'))
        self.assertEqual(tool.label, 'xsel')
        self.assertEqual(tool.copy_argv, ['xsel', '--clipboard', '--input'])
        self.assertEqual(tool.paste_argv, ['xsel', '--clipboard', '--output'])

    def test_x11_session_without_either_tool_is_an_error(self):
        with self.assertRaisesRegex(COPY_DRAFT.ClipboardError, 'xclip or xsel'):
            COPY_DRAFT.posix_clipboard_tool({'DISPLAY': ':0'}, which_only())

    def test_darwin_selects_pbcopy(self):
        real_platform = COPY_DRAFT.sys.platform
        COPY_DRAFT.sys.platform = 'darwin'
        try:
            tool = COPY_DRAFT.posix_clipboard_tool({}, which_only('pbcopy', 'pbpaste'))
        finally:
            COPY_DRAFT.sys.platform = real_platform
        self.assertEqual(tool.label, 'pbcopy')
        self.assertEqual(tool.copy_argv, ['pbcopy'])
        self.assertEqual(tool.paste_argv, ['pbpaste'])
        self.assertEqual(tool.env_overrides, {'LANG': 'en_US.UTF-8', 'LC_CTYPE': 'en_US.UTF-8'})

    def test_no_session_detected_is_an_error(self):
        real_platform = COPY_DRAFT.sys.platform
        COPY_DRAFT.sys.platform = 'linux'
        try:
            with self.assertRaisesRegex(COPY_DRAFT.ClipboardError, 'No supported clipboard session'):
                COPY_DRAFT.posix_clipboard_tool({}, which_only('wl-copy', 'xclip', 'pbcopy'))
        finally:
            COPY_DRAFT.sys.platform = real_platform


class IsTransientReadbackFailureTests(unittest.TestCase):
    def test_known_transient_shapes_are_recognised_case_insensitively(self):
        for stderr in ('Nothing is copied', 'NOTHING IS COPIED', 'No suitable type', 'target not available',
                       'no selection'):
            with self.subTest(stderr=stderr):
                self.assertTrue(COPY_DRAFT._is_transient_readback_failure(stderr))

    def test_an_unrelated_error_is_not_transient(self):
        self.assertFalse(COPY_DRAFT._is_transient_readback_failure('Permission denied'))

    def test_empty_or_none_stderr_is_not_transient(self):
        self.assertFalse(COPY_DRAFT._is_transient_readback_failure(''))
        self.assertFalse(COPY_DRAFT._is_transient_readback_failure(None))


class FakeResult:
    def __init__(self, returncode=0, stdout='', stderr=''):
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr


def make_run(results):
    """A fake `subprocess.run`: records every call, and -- like the real thing -- writes stderr into
    whatever file-like object the call passed as `stderr=` (a copy command's tempfile); when `stderr=`
    is `subprocess.PIPE` instead (a read-back command), it is left alone and the `FakeResult.stderr`
    reaches the caller as `result.stderr`, exactly as a real PIPE capture would."""
    calls = []

    def run(argv, **kwargs):
        calls.append((list(argv), kwargs))
        result = results[len(calls) - 1]
        sink = kwargs.get('stderr')
        if sink is not None and hasattr(sink, 'write'):
            sink.write(result.stderr)
        return result

    run.calls = calls
    return run


class CopyPosixTests(unittest.TestCase):
    """Exercises the exact argv/stdin/env copy_posix sends, with a fake `run` -- never a real clipboard."""

    def setUp(self):
        # The retry loop sleeps for real between attempts; stubbed out so a mismatch test does not cost
        # up to a second of wall-clock time for nothing.
        patcher = mock.patch.object(COPY_DRAFT.time, 'sleep', return_value=None)
        patcher.start()
        self.addCleanup(patcher.stop)

    def test_pbcopy_and_pbpaste_run_with_a_utf8_locale_env(self):
        real_platform = COPY_DRAFT.sys.platform
        COPY_DRAFT.sys.platform = 'darwin'
        try:
            run = make_run([FakeResult(stdout=''), FakeResult(stdout='café')])
            COPY_DRAFT.copy_posix(
                'café', environ={'PATH': '/usr/bin'},
                which=which_only('pbcopy', 'pbpaste'), run=run,
            )
        finally:
            COPY_DRAFT.sys.platform = real_platform
        copy_env = run.calls[0][1]['env']
        paste_env = run.calls[1][1]['env']
        self.assertEqual(copy_env['LANG'], 'en_US.UTF-8')
        self.assertEqual(copy_env['LC_CTYPE'], 'en_US.UTF-8')
        self.assertEqual(copy_env['PATH'], '/usr/bin')
        self.assertEqual(paste_env['LANG'], 'en_US.UTF-8')

    # --- the literal text is copied and read back ---

    def test_wl_copy_copies_the_literal_text(self):
        run = make_run([FakeResult(stdout=''), FakeResult(stdout='run `az login` now')])
        message = COPY_DRAFT.copy_posix(
            'run `az login` now', 
            environ={'WAYLAND_DISPLAY': 'wayland-0'}, which=which_only('wl-copy', 'wl-paste'), run=run,
        )
        copy_argv, copy_kwargs = run.calls[0]
        self.assertEqual(copy_argv, ['wl-copy'])
        self.assertEqual(copy_kwargs['input'], 'run `az login` now')
        self.assertIn('Copied', message)

    def test_backticks_travel_intact(self):
        run = make_run([FakeResult(stdout=''), FakeResult(stdout='run `az login` now')])
        COPY_DRAFT.copy_posix(
            'run `az login` now', 
            environ={'DISPLAY': ':0'}, which=which_only('xclip'), run=run,
        )
        copy_kwargs = run.calls[0][1]
        self.assertEqual(copy_kwargs['input'], 'run `az login` now')

    def test_trailing_whitespace_in_the_plain_readback_is_stripped_before_comparing(self):
        run = make_run([FakeResult(stdout=''), FakeResult(stdout='hello world   \n')])
        message = COPY_DRAFT.copy_posix(
            'hello world', 
            environ={'WAYLAND_DISPLAY': 'wayland-0'}, which=which_only('wl-copy', 'wl-paste'), run=run,
        )
        self.assertIn('Copied 11 chars', message)

    def test_the_plain_readback_is_retried_past_a_stale_selection_before_matching(self):
        run = make_run([FakeResult(stdout=''), FakeResult(stdout=''), FakeResult(stdout='hello world')])
        message = COPY_DRAFT.copy_posix(
            'hello world', 
            environ={'DISPLAY': ':0'}, which=which_only('xclip'), run=run,
        )
        self.assertEqual(len(run.calls), 3)
        self.assertIn('Copied', message)

    def test_the_plain_readback_retries_past_a_transient_nonzero_exit_before_succeeding(self):
        run = make_run([
            FakeResult(stdout=''),
            FakeResult(returncode=1, stderr='Nothing is copied'),  # wl-paste with no owner answering yet
            FakeResult(stdout='hello world'),
        ])
        message = COPY_DRAFT.copy_posix(
            'hello world', 
            environ={'WAYLAND_DISPLAY': 'wayland-0'}, which=which_only('wl-copy', 'wl-paste'), run=run,
        )
        self.assertEqual(len(run.calls), 3)
        self.assertIn('Copied', message)

    def test_the_plain_readback_retries_past_a_transient_nonzero_exit_case_insensitively(self):
        run = make_run([
            FakeResult(stdout=''),
            FakeResult(returncode=1, stderr='NOTHING IS COPIED'),
            FakeResult(stdout='hello world'),
        ])
        message = COPY_DRAFT.copy_posix(
            'hello world', 
            environ={'WAYLAND_DISPLAY': 'wayland-0'}, which=which_only('wl-copy', 'wl-paste'), run=run,
        )
        self.assertEqual(len(run.calls), 3)
        self.assertIn('Copied', message)

    def test_a_non_transient_nonzero_readback_fails_immediately_without_retrying(self):
        run = make_run([FakeResult(stdout=''), FakeResult(returncode=1, stderr='Permission denied')])
        with self.assertRaisesRegex(COPY_DRAFT.ClipboardError, 'Permission denied'):
            COPY_DRAFT.copy_posix(
                'hello world', 
                environ={'WAYLAND_DISPLAY': 'wayland-0'}, which=which_only('wl-copy', 'wl-paste'), run=run,
            )
        self.assertEqual(len(run.calls), 2)  # the copy, and exactly one read-back attempt -- no retry

    def test_a_plain_readback_mismatch_is_an_error_after_retries_are_exhausted(self):
        run = make_run([FakeResult(stdout='')] + [FakeResult(stdout='something else')] * 5)
        with self.assertRaisesRegex(COPY_DRAFT.ClipboardError, 'did not read back'):
            COPY_DRAFT.copy_posix(
                'hello world', 
                environ={'WAYLAND_DISPLAY': 'wayland-0'}, which=which_only('wl-copy', 'wl-paste'), run=run,
            )

    def test_a_readback_that_always_exits_nonzero_reports_the_last_stderr(self):
        run = make_run([FakeResult(stdout='')] + [FakeResult(returncode=1, stderr='Nothing is copied')] * 5)
        with self.assertRaisesRegex(COPY_DRAFT.ClipboardError, 'Nothing is copied'):
            COPY_DRAFT.copy_posix(
                'hello world', 
                environ={'WAYLAND_DISPLAY': 'wayland-0'}, which=which_only('wl-copy', 'wl-paste'), run=run,
            )

    def test_a_zero_exit_wrong_content_attempt_wins_over_later_transient_failures_in_the_final_error(self):
        # One attempt actually read back the wrong content; the rest merely failed to answer at all. The
        # content mismatch is the more informative -- and more correct -- failure to report.
        run = make_run([
            FakeResult(stdout=''),
            FakeResult(stdout='something else'),
            FakeResult(returncode=1, stderr='Nothing is copied'),
            FakeResult(returncode=1, stderr='Nothing is copied'),
            FakeResult(returncode=1, stderr='Nothing is copied'),
            FakeResult(returncode=1, stderr='Nothing is copied'),
        ])
        with self.assertRaisesRegex(COPY_DRAFT.ClipboardError, 'did not read back what was copied'):
            COPY_DRAFT.copy_posix(
                'hello world', 
                environ={'WAYLAND_DISPLAY': 'wayland-0'}, which=which_only('wl-copy', 'wl-paste'), run=run,
            )

    # --- the copy command's own stdio, and failures of it ---

    def test_the_copy_command_discards_stdout_and_captures_stderr_via_a_tempfile_not_a_pipe(self):
        run = make_run([FakeResult(stdout=''), FakeResult(stdout='hello world')])
        COPY_DRAFT.copy_posix(
            'hello world', 
            environ={'WAYLAND_DISPLAY': 'wayland-0'}, which=which_only('wl-copy', 'wl-paste'), run=run,
        )
        copy_kwargs = run.calls[0][1]
        self.assertEqual(copy_kwargs['stdout'], subprocess.DEVNULL)
        self.assertNotIn(copy_kwargs['stderr'], (subprocess.DEVNULL, subprocess.PIPE))
        self.assertTrue(hasattr(copy_kwargs['stderr'], 'write'))
        self.assertNotIn('stdin', copy_kwargs)
        self.assertEqual(copy_kwargs['encoding'], 'utf-8')
        self.assertEqual(copy_kwargs['errors'], 'replace')

    def test_the_readback_command_captures_stdout_and_stderr_via_pipes_not_a_tempfile(self):
        # A read-back command never forks a persistent server, so there is no hang risk to route around
        # with a tempfile the way the copy command's stderr is.
        run = make_run([FakeResult(stdout=''), FakeResult(stdout='hello world')])
        COPY_DRAFT.copy_posix(
            'hello world', 
            environ={'WAYLAND_DISPLAY': 'wayland-0'}, which=which_only('wl-copy', 'wl-paste'), run=run,
        )
        paste_kwargs = run.calls[1][1]
        self.assertEqual(paste_kwargs['stdout'], subprocess.PIPE)
        self.assertEqual(paste_kwargs['stderr'], subprocess.PIPE)
        self.assertEqual(paste_kwargs['stdin'], subprocess.DEVNULL)
        self.assertEqual(paste_kwargs['encoding'], 'utf-8')
        self.assertEqual(paste_kwargs['errors'], 'replace')

    def test_a_nonzero_exit_from_the_copy_tool_is_an_error_including_its_stderr(self):
        run = make_run([FakeResult(returncode=1, stderr='no selection owner')])
        with self.assertRaisesRegex(COPY_DRAFT.ClipboardError, 'wl-copy copy failed.*no selection owner'):
            COPY_DRAFT.copy_posix(
                'hello world', 
                environ={'WAYLAND_DISPLAY': 'wayland-0'}, which=which_only('wl-copy', 'wl-paste'), run=run,
            )

    def test_a_timeout_from_the_copy_tool_is_an_error(self):
        def run(argv, **kwargs):
            raise subprocess.TimeoutExpired(cmd=argv, timeout=10)

        with self.assertRaisesRegex(COPY_DRAFT.ClipboardError, 'did not answer within'):
            COPY_DRAFT.copy_posix(
                'hello world', 
                environ={'WAYLAND_DISPLAY': 'wayland-0'}, which=which_only('wl-copy', 'wl-paste'), run=run,
            )

    def test_no_clipboard_tool_found_is_an_error_naming_the_package(self):
        with self.assertRaisesRegex(COPY_DRAFT.ClipboardError, 'wl-clipboard'):
            COPY_DRAFT.copy_posix(
                'hello world', 
                environ={'WAYLAND_DISPLAY': 'wayland-0'}, which=which_only(), run=make_run([]),
            )



@unittest.skipUnless(os.name == 'nt', 'Exercises the real Windows clipboard via ctypes; Windows only.')
class WindowsCtypesRoundTripTests(unittest.TestCase):
    """Runs only on Windows. Writes to the real clipboard there -- never on this (Linux) machine."""

    def _read_clipboard(self, user32, kernel32):
        hwnd = COPY_DRAFT._create_message_window(user32)
        try:
            return COPY_DRAFT._read_windows_clipboard(user32, kernel32, hwnd)
        finally:
            user32.DestroyWindow(hwnd)

    def test_the_literal_text_round_trips_through_the_real_clipboard(self):
        text = 'a literal `command` to run\n\nsecond paragraph [link](https://example.com)'
        self.assertEqual(COPY_DRAFT.copy_windows(text), f'Copied {len(text)} chars.')

        user32, kernel32 = COPY_DRAFT._windows_api()
        self.assertEqual(self._read_clipboard(user32, kernel32), text)

    def test_a_message_window_creation_failure_is_raised_before_the_clipboard_is_touched(self):
        user32, _ = COPY_DRAFT._windows_api()
        with mock.patch.object(user32, 'CreateWindowExW', return_value=0):
            with mock.patch.object(user32, 'OpenClipboard') as open_clipboard:
                with self.assertRaisesRegex(COPY_DRAFT.ClipboardError, 'CreateWindowExW failed'):
                    COPY_DRAFT.copy_windows('hello')
                open_clipboard.assert_not_called()


if __name__ == '__main__':
    unittest.main()

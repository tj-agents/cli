import importlib.util
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / '.agents/cli/utility/teams-message/scripts/teams_message.py'
SPEC = importlib.util.spec_from_file_location('teams_message', SCRIPT)
TEAMS_MESSAGE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = TEAMS_MESSAGE
SPEC.loader.exec_module(TEAMS_MESSAGE)


def messages(text):
    return [(problem.line, problem.message) for problem in TEAMS_MESSAGE.check_message(text)]


class CheckMessageTests(unittest.TestCase):
    def test_a_message_with_link_text_and_code_spans_passes(self):
        text = "I've been working on [these two pages](https://example.com/pr/412), the `OrderSummary` grid changed."
        self.assertEqual(messages(text), [])

    def test_a_raw_url_is_rejected(self):
        self.assertEqual(
            messages('PR is in https://example.com/pr/412 now'),
            [(1, 'raw URL; write it as [link text](url)')],
        )

    def test_link_text_that_is_the_url_is_rejected(self):
        self.assertEqual(
            messages('[https://example.com/pr/412](https://example.com/pr/412) is in'),
            [(1, 'link text is a raw URL; describe the target instead')],
        )

    def test_urls_inside_code_spans_and_command_fences_are_allowed(self):
        text = 'Run this against `https://example.com`:\n```\ncurl https://example.com/health\n```\nThanks.'
        self.assertEqual(messages(text), [])

    def test_headings_lists_quotes_bold_and_em_dashes_are_reported_with_their_lines(self):
        text = '# Update\n- one\n2. two\n> quoted\nthis is **bold**\nwait — no'
        self.assertEqual(
            messages(text),
            [(1, 'heading'), (2, 'list item'), (3, 'list item'), (4, 'blockquote'), (5, 'bold'), (6, 'em dash')],
        )

    def test_a_fenced_whole_message_is_rejected(self):
        self.assertEqual(
            messages('```\nhello there\n```'),
            [(1, 'the whole message is fenced; fence only a command')],
        )

    def test_an_unclosed_fence_is_rejected(self):
        self.assertEqual(messages('Run:\n```\nmake test'), [(2, 'unclosed code fence')])

    def test_an_empty_message_is_rejected(self):
        self.assertEqual(messages('  \n '), [(1, 'empty message')])


class MainTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)

    def draft(self, text):
        path = Path(self.temp.name) / 'draft.md'
        path.write_text(text, encoding='utf-8')
        return str(path)

    def test_the_clip_script_resolves_beside_this_skill(self):
        self.assertTrue(TEAMS_MESSAGE.CLIP_SCRIPT.is_file())

    def test_a_rejected_draft_never_reaches_the_clipboard(self):
        with mock.patch.object(TEAMS_MESSAGE.subprocess, 'run') as run:
            self.assertEqual(TEAMS_MESSAGE.main(['--path', self.draft('see https://example.com')]), 1)
        run.assert_not_called()

    def test_a_clean_draft_is_copied_through_clip_and_returns_its_exit_code(self):
        path = self.draft('see [the page](https://example.com)')
        with mock.patch.object(TEAMS_MESSAGE.subprocess, 'run') as run:
            run.return_value.returncode = 0
            self.assertEqual(TEAMS_MESSAGE.main(['--path', path]), 0)
        argv = run.call_args.args[0]
        self.assertEqual(argv[1:], ['-B', str(TEAMS_MESSAGE.CLIP_SCRIPT), '--path', path])


if __name__ == '__main__':
    unittest.main()

#!/usr/bin/env python3
"""Check a drafted Teams message against the Teams message rules, then copy it with `cli:clip`.

Run as `python3 teams_message.py --path <file>` (`python` on Windows). A draft that breaks a rule is
reported line by line and never reaches the clipboard.
"""

import argparse
import re
import subprocess
import sys
from collections.abc import Sequence
from pathlib import Path
from typing import NamedTuple

CLIP_SCRIPT = Path(__file__).resolve().parents[2] / 'clip' / 'scripts' / 'copy_draft.py'

_FENCE = re.compile(r'^\s*```')
_CODE_SPAN = re.compile(r'`[^`\n]+`')
_LINK = re.compile(r'\[(?P<label>[^\]\n]+)\]\((?P<url>https?://[^\s)]+)\)')
_URL = re.compile(r'https?://\S+')
_HEADING = re.compile(r'^\s{0,3}#{1,6}\s')
_LIST_ITEM = re.compile(r'^\s*(?:[-*+]|\d+[.)])\s')
_BLOCKQUOTE = re.compile(r'^\s*>')
_BOLD = re.compile(r'\*\*|__')
_EM_DASH = '—'


class Problem(NamedTuple):
    line: int
    message: str


def _prose_problems(number: int, line: str) -> list[Problem]:
    problems: list[Problem] = []
    prose = _CODE_SPAN.sub('', line)
    for link in _LINK.finditer(prose):
        if _URL.match(link['label'].strip()):
            problems.append(Problem(number, 'link text is a raw URL; describe the target instead'))
    if _URL.search(_LINK.sub('', prose)):
        problems.append(Problem(number, 'raw URL; write it as [link text](url)'))
    if _HEADING.match(prose):
        problems.append(Problem(number, 'heading'))
    if _LIST_ITEM.match(prose):
        problems.append(Problem(number, 'list item'))
    if _BLOCKQUOTE.match(prose):
        problems.append(Problem(number, 'blockquote'))
    if _BOLD.search(prose):
        problems.append(Problem(number, 'bold'))
    if _EM_DASH in prose:
        problems.append(Problem(number, 'em dash'))
    return problems


def check_message(text: str) -> list[Problem]:
    lines = text.split('\n')
    if not text.strip():
        return [Problem(1, 'empty message')]
    if len(lines) > 1 and _FENCE.match(lines[0]) and lines[-1].strip() == '```':
        return [Problem(1, 'the whole message is fenced; fence only a command')]
    problems: list[Problem] = []
    fence_start: int | None = None
    for number, line in enumerate(lines, start=1):
        if _FENCE.match(line):
            fence_start = number if fence_start is None else None
        elif fence_start is None:
            problems.extend(_prose_problems(number, line))
    if fence_start is not None:
        problems.append(Problem(fence_start, 'unclosed code fence'))
    return problems


def parse_args(argv: Sequence[str] | None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description='Check a Teams message draft, then copy it to the clipboard.')
    parser.add_argument('--path', required=True, help='Path to the UTF-8 draft file.')
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None) -> int:
    path = str(parse_args(argv).path)
    try:
        text = Path(path).read_text(encoding='utf-8-sig').replace('\r\n', '\n').rstrip()
    except (OSError, UnicodeDecodeError) as exc:
        print(f'Cannot read draft {path}: {exc}', file=sys.stderr)
        return 1
    problems = check_message(text)
    for problem in problems:
        print(f'{path}:{problem.line}: {problem.message}', file=sys.stderr)
    if problems:
        return 1
    return subprocess.run([sys.executable, '-B', str(CLIP_SCRIPT), '--path', path], check=False).returncode


if __name__ == '__main__':
    sys.exit(main())

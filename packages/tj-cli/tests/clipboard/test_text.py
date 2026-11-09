from pathlib import Path

import pytest

from tj_cli.clipboard import ClipboardError
from tj_cli.clipboard._text import normalise_text, read_text_file


def test_normalise_text_converts_crlf_to_lf() -> None:
    assert normalise_text("a\r\nb\r\n") == "a\nb"


def test_normalise_text_trims_trailing_whitespace_only_from_the_very_end() -> None:
    # The blank line between paragraphs is NOT trailing whitespace and must survive;
    # only whitespace at the absolute end of the text is removed, matching copy-
    # draft.ps1's non-multiline '\s+$'.
    assert normalise_text("line1\n\nline2\n   ") == "line1\n\nline2"


def test_normalise_text_of_blank_text_is_empty() -> None:
    assert normalise_text("   \n\n  ") == ""


def test_read_text_file_rejects_a_missing_path(tmp_path: Path) -> None:
    missing = tmp_path / "nope.txt"
    with pytest.raises(ClipboardError, match="not found"):
        read_text_file(str(missing))


def test_read_text_file_rejects_a_blank_file(tmp_path: Path) -> None:
    path = tmp_path / "empty.txt"
    path.write_text("   \n\n  ", encoding="utf-8")
    with pytest.raises(ClipboardError, match="empty"):
        read_text_file(str(path))


def test_read_text_file_reads_utf8_and_normalises(tmp_path: Path) -> None:
    path = tmp_path / "draft.txt"
    path.write_bytes("café é\r\nsecond line\r\n".encode())
    assert read_text_file(str(path)) == "café é\nsecond line"


def test_read_text_file_drops_a_leading_bom(tmp_path: Path) -> None:
    path = tmp_path / "bom.txt"
    path.write_bytes(b"\xef\xbb\xbf" + b"hello\nworld")
    assert read_text_file(str(path)) == "hello\nworld"


def test_read_text_file_rejects_non_utf8_bytes(tmp_path: Path) -> None:
    path = tmp_path / "latin1.txt"
    path.write_bytes(
        "café".encode("latin-1")
    )  # 0xe9 with no continuation byte is invalid UTF-8.
    with pytest.raises(ClipboardError, match="not valid UTF-8"):
        read_text_file(str(path))

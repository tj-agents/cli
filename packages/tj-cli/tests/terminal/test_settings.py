import datetime
import json
from pathlib import Path

import pytest

from tj_cli.terminal import settings


def test_preview_preserves_jsonc_and_creates_no_backup(tmp_path: Path) -> None:
    path = tmp_path / "settings.json"
    original = '{\n  // keep\n  "profiles": {"defaults": {}}\n}\n'
    path.write_text(original, encoding="utf-8")

    message, backup = settings.configure(path, "always", preview=True)

    assert message.startswith("Would set closeOnExit")
    assert backup is None
    assert path.read_text(encoding="utf-8") == original
    assert list(tmp_path.glob("*.bak-*")) == []


@pytest.mark.parametrize("newline", [b"\r\n", b"\n", b"\r"])
def test_write_preserves_line_endings_and_backup_bytes(
    tmp_path: Path, newline: bytes
) -> None:
    path = tmp_path / "settings.json"
    original = (
        b"{" + newline + b'  "profiles": {"defaults": {}}' + newline + b"}" + newline
    )
    path.write_bytes(original)
    moment = datetime.datetime(2026, 10, 10, 12, 0, 1)

    _message, backup = settings.configure(path, "always", now=moment)

    assert backup is not None
    assert backup.name == "settings.json.bak-20261010120001"
    assert backup.read_bytes() == original
    updated = path.read_bytes()
    assert b'"closeOnExit": "always"' in updated
    without_line_endings = updated.replace(newline, b"")
    assert b"\n" not in without_line_endings
    assert b"\r" not in without_line_endings


@pytest.mark.parametrize(
    "original",
    [
        '{"profiles":{"defaults":{"closeOnExit":"always"}}}',
        '{"profiles":{"list":[{"closeOnExit":"never"}],'
        '"defaults":{"closeOnExit":"always"}}}',
    ],
)
def test_existing_value_is_unchanged_without_backup(
    tmp_path: Path, original: str
) -> None:
    path = tmp_path / "settings.json"
    path.write_text(original, encoding="utf-8")

    message, backup = settings.configure(path, "always")

    assert message == "closeOnExit is already 'always'."
    assert backup is None
    assert path.read_text(encoding="utf-8") == original
    assert list(tmp_path.glob("*.bak-*")) == []


def test_empty_defaults_receives_valid_json() -> None:
    updated, changed = settings.update_close_on_exit(
        '{"profiles":{"defaults":{}}}', "always"
    )

    assert changed
    assert json.loads(updated) == {"profiles": {"defaults": {"closeOnExit": "always"}}}


def test_update_changes_only_active_profile_defaults_value() -> None:
    original = (
        '{"closeOnExit":"never","defaults":{"closeOnExit":"graceful"},'
        '"profiles":{"list":[{"closeOnExit":"never"}],"defaults":{'
        '/* "closeOnExit":"never" */"nested":{"closeOnExit":"automatic"},'
        '"closeOnExit" : "graceful"}}}'
    )

    updated, changed = settings.update_close_on_exit(original, "always")

    assert changed
    assert updated == original.replace(
        '"closeOnExit" : "graceful"', '"closeOnExit" : "always"'
    )
    parsed = json.loads(updated.replace('/* "closeOnExit":"never" */', ""))
    assert parsed["closeOnExit"] == "never"
    assert parsed["defaults"]["closeOnExit"] == "graceful"
    assert parsed["profiles"]["list"][0]["closeOnExit"] == "never"
    assert parsed["profiles"]["defaults"]["nested"]["closeOnExit"] == "automatic"
    assert parsed["profiles"]["defaults"]["closeOnExit"] == "always"


@pytest.mark.parametrize(
    ("body", "has_members", "indent"),
    [
        ("", False, "            "),
        (" \t ", False, "            "),
        ("/* } { [ ] */", False, "            "),
        ('\n// "closeOnExit": "never", }\n', False, "            "),
        ('\n    // }\n    "font": {"face": "A}B"}\n', True, "    "),
        ('/* "closeOnExit": "never" */"font":{}', True, "            "),
        (
            '"nested":{"closeOnExit":"never","items":[{"brace":"}"},],},',
            True,
            "            ",
        ),
        (r'"label":"a//}/* \"quoted\"","nested":{"font":{}}', True, "            "),
    ],
)
@pytest.mark.parametrize("profile_list", ["", '"list":[{"closeOnExit":"never"}],'])
def test_insert_preserves_jsonc_body_and_overrides(
    body: str, has_members: bool, indent: str, profile_list: str
) -> None:
    prefix = '{"profiles":{' + profile_list + '"defaults":{'
    suffix = "}}}"
    original = prefix + body + suffix
    insertion = "\n" + indent + '"closeOnExit": "always"' + ("," if has_members else "")

    updated, changed = settings.update_close_on_exit(original, "always")

    assert changed
    assert updated == prefix + insertion + body + suffix
    assert settings.update_close_on_exit(updated, "always") == (updated, False)


def test_comment_brace_does_not_hide_following_defaults_member() -> None:
    original = '{"profiles":{"defaults":{\n // }\n "font":{"face":"A}B"}\n}}}'

    updated, changed = settings.update_close_on_exit(original, "always")

    assert changed
    assert json.loads(updated.replace("// }", "")) == {
        "profiles": {"defaults": {"closeOnExit": "always", "font": {"face": "A}B"}}}
    }


def test_update_preserves_nested_objects_trailing_commas_and_windows_bytes(
    tmp_path: Path,
) -> None:
    path = tmp_path / "settings.json"
    original = (
        b'\xef\xbb\xbf{\r\n"profiles":{"list":[{"closeOnExit":"never"},],'
        b'"defaults":{/* } */"font":{"face":"a}b"},'
        b'"closeOnExit" : "graceful",},},}\r\n'
    )
    path.write_bytes(original)

    _message, backup = settings.configure(path, "always")

    assert backup is not None
    assert backup.read_bytes() == original
    assert path.read_bytes() == original.replace(
        b'"closeOnExit" : "graceful"', b'"closeOnExit" : "always"'
    )


def test_commented_value_does_not_make_insert_a_noop(tmp_path: Path) -> None:
    path = tmp_path / "settings.json"
    original = '{"profiles":{"defaults":{// "closeOnExit": "always"\n}}}'
    path.write_text(original, encoding="utf-8")

    message, backup = settings.configure(path, "always")

    assert message.startswith("closeOnExit set")
    assert backup is not None
    assert backup.read_text(encoding="utf-8") == original
    assert path.read_text(encoding="utf-8") == (
        '{"profiles":{"defaults":{\n            "closeOnExit": "always"'
        '// "closeOnExit": "always"\n}}}'
    )


def test_escaped_property_names_and_value_follow_json_semantics() -> None:
    original = r'{"profi\u006ces":{"def\u0061ults":{"close\u004fnExit":"alw\u0061ys"}}}'

    assert settings.update_close_on_exit(original, "always") == (original, False)
    assert settings.update_close_on_exit(original, "never") == (
        original.replace(r'"alw\u0061ys"', '"never"'),
        True,
    )


@pytest.mark.parametrize(
    "original",
    [
        '{"profiles":{"defaults":{',
        '{"profiles":{"defaults":{"x":[1,2}}}}',
        '{"profiles":{"defaults":{}}} garbage',
        '{"profiles":{"defaults":{/* unterminated}}}',
        '{"profiles":{"defaults":{"x":"unterminated}}}',
        '{"profiles":{"defaults":{"x":NaN}}}',
        '{"profiles":{"defaults":{"x":Infinity}}}',
        '{"profiles":{"defaults":{,}}}',
        '{"profiles":{"defaults":{"x":[,]}}}',
        '{"profiles":{"defaults":{"x":,}}}',
        '{"profiles":{"defaults":{,,}}}',
        '{"profiles":{"defaults":{"x":1 "y":2}}}',
        '{"profiles":{"defaults":{}, "defaults":{}}}',
        r'{"profiles":{},"profi\u006ces":{"defaults":{}}}',
        '{"profiles":{"defaults":{"closeOnExit":"always","closeOnExit":"never"}}}',
        '{"profiles":{"defaults":{"nested":{"x":1,"x":2}}}}',
        '{"profiles":{"defaults":{"closeOnExit":true}}}',
        '{"profiles":{"defaults":[]}}',
        '{"profiles":[]}',
        '{"defaults":{},"nested":{"profiles":{"defaults":{}}}}',
        '["profiles",{"defaults":{}}]',
    ],
)
@pytest.mark.parametrize("preview", [False, True])
def test_malformed_or_ambiguous_settings_refuse_without_write_or_backup(
    tmp_path: Path, original: str, preview: bool
) -> None:
    path = tmp_path / "settings.json"
    original_bytes = original.encode("utf-8")
    path.write_bytes(original_bytes)

    with pytest.raises(RuntimeError):
        settings.configure(path, "always", preview=preview)

    assert path.read_bytes() == original_bytes
    assert list(tmp_path.glob("*.bak-*")) == []


def test_unsupported_value_refuses_without_write_or_backup(tmp_path: Path) -> None:
    path = tmp_path / "settings.json"
    original = b'{"profiles":{"defaults":{}}}'
    path.write_bytes(original)

    with pytest.raises(RuntimeError, match="Unsupported closeOnExit"):
        settings.configure(path, 'always"')

    assert path.read_bytes() == original
    assert list(tmp_path.glob("*.bak-*")) == []


def test_missing_defaults_refuses_without_writing(tmp_path: Path) -> None:
    path = tmp_path / "settings.json"
    original = '{"profiles":{}}'
    path.write_text(original, encoding="utf-8")

    with pytest.raises(RuntimeError, match=r"No profiles\.defaults block"):
        settings.configure(path, "always")

    assert path.read_text(encoding="utf-8") == original
    assert list(tmp_path.glob("*.bak-*")) == []


def test_linux_noop_and_explicit_disposable_settings(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    monkeypatch.setattr(settings, "is_windows", lambda: False)

    assert settings.main([]) == 0
    assert "Linux is a no-op" in capsys.readouterr().out
    path = tmp_path / "settings.json"
    original = '{"profiles":{"defaults":{}}}'
    path.write_text(original, encoding="utf-8")

    assert settings.main(["--settings-path", str(path), "--preview"]) == 0
    assert path.read_text(encoding="utf-8") == original


def test_default_settings_path_requires_local_app_data() -> None:
    with pytest.raises(RuntimeError, match="LOCALAPPDATA is not set"):
        settings.default_settings_path({})

    path = settings.default_settings_path({"LOCALAPPDATA": "C:/temporary"})

    assert path.as_posix() == (
        "C:/temporary/Packages/Microsoft.WindowsTerminal_8wekyb3d8bbwe/"
        "LocalState/settings.json"
    )

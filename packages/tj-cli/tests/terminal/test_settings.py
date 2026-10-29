import datetime
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


def test_write_preserves_windows_line_endings_and_backup_bytes(tmp_path: Path) -> None:
    path = tmp_path / "settings.json"
    original = b'{\r\n  "profiles": {"defaults": {}}\r\n}\r\n'
    path.write_bytes(original)
    moment = datetime.datetime(2026, 10, 10, 12, 0, 1)

    _message, backup = settings.configure(path, "always", now=moment)

    assert backup is not None
    assert backup.name == "settings.json.bak-20261010120001"
    assert backup.read_bytes() == original
    updated = path.read_bytes()
    assert b'"closeOnExit": "always"' in updated
    assert b"\n" not in updated.replace(b"\r\n", b"")


def test_existing_value_is_unchanged_without_backup(tmp_path: Path) -> None:
    path = tmp_path / "settings.json"
    original = '{"profiles":{"defaults":{"closeOnExit":"always"}}}'
    path.write_text(original, encoding="utf-8")

    message, backup = settings.configure(path, "always")

    assert message == "closeOnExit is already 'always'."
    assert backup is None
    assert path.read_text(encoding="utf-8") == original
    assert list(tmp_path.glob("*.bak-*")) == []


def test_empty_defaults_receives_valid_json() -> None:
    import json

    updated, changed = settings.update_close_on_exit(
        '{"profiles":{"defaults":{}}}', "always"
    )

    assert changed
    assert json.loads(updated) == {"profiles": {"defaults": {"closeOnExit": "always"}}}


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

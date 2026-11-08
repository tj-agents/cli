import io
import platform
import urllib.request
from pathlib import Path

import pytest

from tj_cli import worktrunk_install


def test_a_download_with_the_wrong_digest_installs_nothing(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    monkeypatch.setattr(platform, "system", lambda: "Linux")
    monkeypatch.setattr(platform, "machine", lambda: "x86_64")
    monkeypatch.setattr(
        urllib.request, "urlopen", lambda url, timeout: io.BytesIO(b"tampered")
    )
    assert worktrunk_install.main(["--bin-dir", str(tmp_path)]) == 1
    assert "failed SHA-256 verification" in capsys.readouterr().err
    assert list(tmp_path.iterdir()) == []


def test_check_reports_a_missing_binary_without_downloading(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    def unexpected(*args: object, **kwargs: object) -> None:
        pytest.fail("check mode must not download")

    monkeypatch.setattr(urllib.request, "urlopen", unexpected)
    assert worktrunk_install.main(["--bin-dir", str(tmp_path), "--check"]) == 1
    assert "missing    git-wt" in capsys.readouterr().out

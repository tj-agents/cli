"""copy_draft.py: the thin CLI around tj_cli.clipboard -- draft reading, exit codes, and resolving
tj_cli from each generated plugin layout. Backend behaviour itself is tested in
packages/tj-cli/tests/clipboard/; this module never touches a real clipboard."""

import importlib.util
import subprocess
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / ".agents/cli/utility/clip/scripts/copy_draft.py"

_SPEC = importlib.util.spec_from_file_location("copy_draft", SCRIPT)
assert _SPEC is not None and _SPEC.loader is not None
copy_draft = importlib.util.module_from_spec(_SPEC)
sys.modules[_SPEC.name] = copy_draft
_SPEC.loader.exec_module(copy_draft)


def test_main_copies_a_valid_draft_and_prints_the_backend_result(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    draft = tmp_path / "draft.txt"
    draft.write_text("hello world", encoding="utf-8")
    monkeypatch.setattr(copy_draft, "copy", lambda text: f"Copied {len(text)} chars.")

    assert copy_draft.main(["--path", str(draft)]) == 0
    assert capsys.readouterr().out == "Copied 11 chars.\n"


def test_main_reports_a_missing_draft_without_ever_calling_copy(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    missing = tmp_path / "nope.txt"
    monkeypatch.setattr(
        copy_draft, "copy", lambda text: pytest.fail("copy() must not run for a missing draft")
    )

    assert copy_draft.main(["--path", str(missing)]) == 1
    assert "not found" in capsys.readouterr().err


def test_main_reports_a_backend_failure(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    draft = tmp_path / "draft.txt"
    draft.write_text("hello world", encoding="utf-8")

    def _fail(text: str) -> str:
        raise copy_draft.ClipboardError("no clipboard tool was found")

    monkeypatch.setattr(copy_draft, "copy", _fail)

    assert copy_draft.main(["--path", str(draft)]) == 1
    assert "no clipboard tool was found" in capsys.readouterr().err


def _write_stub_tj_cli(runtime_dir: Path) -> None:
    """A tj_cli stand-in that never touches a real clipboard, just enough to prove the thin CLI found
    and imported it from the given runtime bundle."""
    package = runtime_dir / "tj_cli"
    package.mkdir(parents=True)
    (package / "__init__.py").write_text("", encoding="utf-8")
    (package / "clipboard.py").write_text(
        "from pathlib import Path\n\n\n"
        "class ClipboardError(RuntimeError):\n"
        "    pass\n\n\n"
        "def read_text_file(path):\n"
        "    return Path(path).read_text(encoding='utf-8')\n\n\n"
        "def copy(text, **_kwargs):\n"
        "    return f'Copied {len(text)} chars.'\n",
        encoding="utf-8",
    )


@pytest.mark.parametrize(
    "script_relative", [".agents/cli/utility/clip/scripts", "skills/clip/scripts"]
)
def test_the_script_resolves_tj_cli_from_each_shipped_plugin_layout(
    tmp_path: Path, script_relative: str
) -> None:
    script_dir = tmp_path / script_relative
    script_dir.mkdir(parents=True)
    script = script_dir / "copy_draft.py"
    script.write_bytes(SCRIPT.read_bytes())
    _write_stub_tj_cli(tmp_path / ".agents/cli/runtime")
    draft = tmp_path / "draft.txt"
    draft.write_text("hello world", encoding="utf-8")

    # -S skips site-packages entirely, so this interpreter's own tj-cli dev install cannot mask whether
    # the script's own fallback resolution actually found the shipped runtime bundle below.
    result = subprocess.run(
        [sys.executable, "-S", str(script), "--path", str(draft)],
        capture_output=True,
        text=True,
        timeout=30,
    )

    assert result.returncode == 0, result.stderr
    assert result.stdout == "Copied 11 chars.\n"


def test_the_script_fails_clearly_when_no_tj_cli_is_reachable(tmp_path: Path) -> None:
    script_dir = tmp_path / ".agents/cli/utility/clip/scripts"
    script_dir.mkdir(parents=True)
    script = script_dir / "copy_draft.py"
    script.write_bytes(SCRIPT.read_bytes())
    draft = tmp_path / "draft.txt"
    draft.write_text("hello world", encoding="utf-8")

    result = subprocess.run(
        [sys.executable, "-S", str(script), "--path", str(draft)],
        capture_output=True,
        text=True,
        timeout=30,
    )

    assert result.returncode == 1
    assert "tj_cli" in result.stderr

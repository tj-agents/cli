import json
import subprocess
from pathlib import Path
from types import MappingProxyType

import pytest

from tj_cli import tool_receipts


@pytest.mark.parametrize(
    "contents",
    [
        b"{",
        b"[]",
        b"\xff",
        b'{"identity":"old","hashes":{}}',
        b'{"schema":1,"version":"1","hashes":{}}',
        b'{"schema":1,"version":"1","hashes":{"app":false}}',
    ],
)
def test_malformed_receipt_is_pending_without_running_processes(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    contents: bytes,
) -> None:
    receipt = tmp_path / "receipt.json"
    receipt.write_bytes(contents)
    monkeypatch.setattr(
        subprocess, "run", lambda *a, **kw: pytest.fail("file-only check")
    )
    assert tool_receipts.read(receipt) is None
    assert tool_receipts.pending(receipt, "1") is not None


def test_receipt_detects_version_missing_and_changed_files(tmp_path: Path) -> None:
    executable = tmp_path / "app"
    executable.write_bytes(b"first")
    executable.chmod(0o755)
    path = tmp_path / "receipt.json"
    receipt = tool_receipts.Receipt(
        "1", MappingProxyType({executable.name: tool_receipts.sha256(executable)})
    )
    tool_receipts.write(path, receipt)
    assert tool_receipts.read(path) == receipt
    assert json.loads(path.read_text(encoding="utf-8"))["schema"] == 1
    assert tool_receipts.pending(path, "1", names=iter(["app"])) is None
    assert tool_receipts.pending(path, "2") is not None
    assert tool_receipts.pending(path, "1", names=("other",)) is not None
    executable.write_bytes(b"second")
    assert tool_receipts.pending(path, "1") is not None
    executable.unlink()
    assert tool_receipts.pending(path, "1") is not None


def test_receipt_checks_absolute_application_path(tmp_path: Path) -> None:
    application = tmp_path / "desktop app"
    application.write_bytes(b"app")
    application.chmod(0o755)
    receipt = tmp_path / "bin" / "receipt.json"
    tool_receipts.write(
        receipt,
        tool_receipts.Receipt(
            "1", {str(application.resolve()): tool_receipts.sha256(application)}
        ),
    )
    assert tool_receipts.pending(receipt, "1") is None
    application.unlink()
    assert tool_receipts.pending(receipt, "1") is not None

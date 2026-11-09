#!/usr/bin/env python3
"""Thin CLI: read a drafted file and copy it onto the system clipboard via tj_cli.clipboard.

Run as `python3 copy_draft.py --path <file>` (`python` on Windows); the shebang and exec bit are not
relied on. tj_cli ships either as the installed `tj-cli` distribution (a `uv run` dev checkout) or, in
this skill's generated plugin bundle, at `<plugin root>/.agents/cli/runtime/tj_cli`;
`_shipped_runtime_roots` below is the one place that knows about the second case.
"""

import argparse
import sys
from collections.abc import Sequence
from pathlib import Path


def _shipped_runtime_roots(script: Path) -> tuple[Path, ...]:
    """Where tj_cli ships inside this skill's two generated plugin layouts, each rooted at the plugin root.

    `<root>/.agents/cli/utility/clip/scripts/copy_draft.py` and `<root>/skills/clip/scripts/copy_draft.py`
    are the only two locations this script is shipped at, and `<root>/.agents/cli/runtime` is this
    plugin's own runtime bundle in both. Never searched beyond these two fixed, documented locations --
    a sibling checkout or another installed copy elsewhere on the machine is not a substitute.
    """
    parents = script.parents
    candidates = (parents[5] if len(parents) > 5 else None, parents[3] if len(parents) > 3 else None)
    return tuple(root / ".agents" / "cli" / "runtime" for root in candidates if root is not None)


def _ensure_tj_cli_importable() -> None:
    try:
        import tj_cli.clipboard  # noqa: F401
    except ModuleNotFoundError:
        pass
    else:
        return
    script = Path(__file__).resolve()
    runtimes = _shipped_runtime_roots(script)
    for runtime in runtimes:
        if (runtime / "tj_cli" / "__init__.py").is_file():
            sys.path.insert(0, str(runtime))
            return
    raise SystemExit(
        "tj_cli was not found: neither installed nor shipped at this script's runtime bundle "
        f"({', '.join(str(runtime) for runtime in runtimes)})."
    )


_ensure_tj_cli_importable()

from tj_cli.clipboard import ClipboardError, copy, read_text_file  # noqa: E402


def parse_args(argv: Sequence[str] | None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Copy a drafted file onto the system clipboard as plain text.")
    parser.add_argument("--path", required=True, help="Path to the UTF-8 draft file to copy.")
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        text = read_text_file(str(args.path))
        print(copy(text))
        return 0
    except ClipboardError as exc:
        print(str(exc), file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())

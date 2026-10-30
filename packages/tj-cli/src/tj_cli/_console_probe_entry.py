import sys
from pathlib import Path


def main() -> int:
    boundary = Path(__file__).resolve().parent.parent
    if not (boundary / "tj_cli" / "__init__.py").is_file():
        raise RuntimeError("declared tj_cli import boundary is unavailable")
    sys.path.insert(0, str(boundary))

    from tj_cli.terminal.console_probe import main as probe

    return probe()


if __name__ == "__main__":
    raise SystemExit(main())

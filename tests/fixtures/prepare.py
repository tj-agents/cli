import json
import sys
from pathlib import Path


def main() -> int:
    directory = Path(__file__).parent
    arguments = json.load(sys.stdin.buffer)
    (directory / "prepare-input.json").write_text(
        json.dumps(arguments), encoding="utf-8"
    )
    if (directory / "prepare-failure").is_file():
        print("simulated preparation failure", file=sys.stderr)
        return 9
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

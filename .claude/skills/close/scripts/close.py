from pathlib import Path
import sys


for boundary in Path(__file__).resolve().parents:
    if (boundary / ".agents/plugins/payloads.json").is_file() and (boundary / "pyproject.toml").is_file():
        package_root = boundary / "packages/tj-cli/src"
        break
    if (boundary / ".claude-plugin/plugin.json").is_file() or (boundary / ".codex-plugin/plugin.json").is_file():
        package_root = boundary / "python"
        break
else:
    raise RuntimeError("CLI command is outside a declared source or plugin boundary")

if not (package_root / "tj_cli/__init__.py").is_file():
    raise RuntimeError("CLI command is missing its declared Python bundle")
sys.path.insert(0, str(package_root))

from tj_cli.close import main


if __name__ == "__main__":
    raise SystemExit(main())

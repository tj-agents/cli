import json
import sys
from pathlib import Path

Path(sys.argv[1]).write_text(
    json.dumps(sys.argv[2:], ensure_ascii=False), encoding="utf-8"
)

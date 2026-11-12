"""A codex stand-in: `app-server proxy` logs each request and answers it with an empty result."""

import json
import os
import sys

log = os.environ["FAKE_CODEX_LOG"]


def append(entry: dict) -> None:
    with open(log, "a", encoding="utf-8") as file:
        file.write(json.dumps(entry) + "\n")


def proxy() -> None:
    for line in sys.stdin:
        request = json.loads(line)
        append({"proxy": request.get("method"), "params": request.get("params")})
        if "id" in request:
            sys.stdout.write(json.dumps({"id": request["id"], "result": {}}) + "\n")
            sys.stdout.flush()


if sys.argv[1:3] == ["app-server", "proxy"]:
    proxy()
else:
    sys.exit(f"fake codex: unexpected {sys.argv[1:]}")

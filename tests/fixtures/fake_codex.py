"""A codex stand-in: `queue` logs its arguments; `app-server proxy` plays a scripted app-server."""

import json
import os
import sys

log = os.environ["FAKE_CODEX_LOG"]


def append(entry: dict) -> None:
    with open(log, "a", encoding="utf-8") as file:
        file.write(json.dumps(entry) + "\n")


def send(message: dict) -> None:
    sys.stdout.write(json.dumps(message) + "\n")
    sys.stdout.flush()


def proxy() -> None:
    loaded = [thread for thread in os.environ.get("FAKE_LOADED", "").split(",") if thread]
    status = os.environ.get("FAKE_STATUS", "active")
    events = json.loads(os.environ.get("FAKE_EVENTS", "[]"))
    for line in sys.stdin:
        request = json.loads(line)
        method = request.get("method")
        append({"proxy": method, "params": request.get("params")})
        if "id" not in request:
            continue
        result: dict = {}
        if method == "thread/loaded/list":
            result = {"data": loaded}
        elif method == "thread/resume":
            result = {"thread": {"id": request["params"]["threadId"], "status": {"type": status}}}
        send({"id": request["id"], "result": result})
        if method == "thread/resume":
            for event in events:
                send(event)


if sys.argv[1:3] == ["app-server", "proxy"]:
    proxy()
elif sys.argv[1] == "queue":
    arguments = sys.argv[2:]
    append({"queue": dict(zip(arguments[::2], arguments[1::2]))})
    if os.environ.get("FAKE_QUEUE_FAIL"):
        sys.stderr.write("No active session found\n")
        sys.exit(1)
else:
    sys.exit(f"fake codex: unexpected {sys.argv[1:]}")

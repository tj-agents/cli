import json
import sys
from pathlib import Path
from typing import TypedDict


class Claim(TypedDict):
    path: str


class Data(TypedDict, total=False):
    worktrees: list[Claim]
    released: bool


class Reply(TypedDict):
    ok: bool
    data: Data


def main() -> None:
    root = Path(__file__).parent
    command, *arguments = sys.argv[1:]
    with (root / "agent-store.log").open("a", encoding="utf-8") as log:
        log.write(" ".join(sys.argv[1:]) + "\n")
    reply_path = root / f"agent-store-{command}.json"
    reply: Reply = (
        json.loads(reply_path.read_text(encoding="utf-8"))
        if reply_path.exists()
        else {"ok": True, "data": {}}
    )
    released_path = root / "released.json"
    released: list[str] = (
        json.loads(released_path.read_text(encoding="utf-8"))
        if released_path.exists()
        else []
    )
    if command == "release-worktree" and reply["ok"]:
        released.append(arguments[arguments.index("--path") + 1])
        released_path.write_text(json.dumps(released), encoding="utf-8")
        reply = {"ok": True, "data": {"released": True}}
    if command == "worktrees" and reply["ok"]:
        reply["data"]["worktrees"] = [
            row for row in reply["data"]["worktrees"] if row["path"] not in released
        ]
    print(json.dumps(reply))


if __name__ == "__main__":
    main()

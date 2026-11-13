import json
import sys
from pathlib import Path
from typing import TypedDict


class Claim(TypedDict):
    path: str


class Data(TypedDict, total=False):
    worktrees: list[Claim]
    released: bool
    launch: object
    session: object


class Reply(TypedDict):
    ok: bool
    data: Data


def value(arguments: list[str], name: str) -> str:
    return arguments[arguments.index(name) + 1]


def prebound_reply(
    root: Path, command: str, arguments: list[str], reply: Reply
) -> Reply | None:
    configuration = root / "agent-store-prebind.json"
    if not configuration.is_file():
        return None
    configuration_data = json.loads(configuration.read_text(encoding="utf-8"))
    session = configuration_data["session"]
    if not isinstance(session, str):
        raise ValueError("prebound session must be text")
    launch_file = root / "agent-store-prebind-launch.json"
    bound_file = root / "agent-store-prebind-bound"
    if command == "launch":
        launch = {
            "launch_id": value(arguments, "--launch-id"),
            "host": value(arguments, "--host"),
            "title": value(arguments, "--title"),
            "cwd": value(arguments, "--cwd"),
            "wrapper": {
                "pid": int(value(arguments, "--wrapper-pid")),
                "started_at": value(arguments, "--wrapper-started-at"),
            },
        }
        launch_file.write_text(json.dumps(launch), encoding="utf-8")
        return {"ok": True, "data": {}}
    if command == "register":
        if not reply["ok"]:
            return reply
        bound_file.touch()
        return {"ok": True, "data": {}}
    if command != "show":
        return None
    launch = json.loads(launch_file.read_text(encoding="utf-8"))
    handle = f"codex:{session}"
    bound = bound_file.exists()
    launch.update({"session": handle if bound else None, "state": "active"})
    native = (
        {
            "session": handle,
            "native_id": session,
            "host": "codex",
            "launch_id": launch["launch_id"],
            "state": "active",
        }
        if bound
        else None
    )
    if configuration_data.get("foreign_native") is True and native is not None:
        native["native_id"] = "123e4567-e89b-12d3-a456-426614174001"
    if arguments[:1] != ["--launch-id"]:
        # Like agent-store, `show <handle>` returns only the session row.
        return {"ok": True, "data": Data(session=native)}
    return {"ok": True, "data": Data(launch=launch, session=native)}


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
    prebound = prebound_reply(root, command, arguments, reply)
    if prebound is not None:
        reply = prebound
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

import json
import sys
from pathlib import Path
from typing import TypedDict


class Claim(TypedDict):
    path: str
    session: str
    launch_id: str | None


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
    source_file = root / "agent-store-prebind-source.json"
    source_id = configuration_data.get("resume_from")
    if command == "show" and source_id and (
        arguments[:2] == ["--launch-id", source_id]
        or (arguments[:1] != ["--launch-id"] and not bound_file.exists())
    ):
        source = json.loads(source_file.read_text(encoding="utf-8"))
        return {"ok": True, "data": source}
    if command == "resume":
        if reply["ok"]:
            bound_file.touch()
        return reply
    if command == "launch":
        launch = {
            "launch_id": value(arguments, "--launch-id"),
            "host": value(arguments, "--host"),
            "title": value(arguments, "--title"),
            "cwd": value(arguments, "--cwd"),
            "item": {"ref": value(arguments, "--item")}
            if "--item" in arguments
            else None,
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
    host = configuration_data.get("host", "codex")
    handle = f"{host}:{session}"
    bound = bound_file.exists()
    launch.update({"session": handle if bound else None, "state": "active"})
    native = (
        {
            "session": handle,
            "native_id": session,
            "host": host,
            "launch_id": launch["launch_id"],
            "wrapper": launch["wrapper"],
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


def record_launch(root: Path, arguments: list[str]) -> None:
    """Persist the reciprocal active binding registered by a managed wrapper."""
    show_path = root / "agent-store-show.json"
    if show_path.exists():
        show = json.loads(show_path.read_text(encoding="utf-8"))
    else:
        show = {"ok": True, "data": {"launch": {}, "session": {}}}
    launch = show["data"]["launch"]
    host = value(arguments, "--host")
    launch_id = value(arguments, "--launch-id")
    session = launch.get("session", f"{host}:sess-1")
    launch.update(
        {
            "launch_id": launch_id,
            "host": host,
            "title": value(arguments, "--title"),
            "cwd": value(arguments, "--cwd"),
            "session": session,
            "state": "active",
            "item": {"ref": value(arguments, "--item")}
            if "--item" in arguments
            else None,
            "parent": value(arguments, "--parent") if "--parent" in arguments else None,
            "wrapper": {
                "pid": int(value(arguments, "--wrapper-pid")),
                "started_at": value(arguments, "--wrapper-started-at"),
            },
        }
    )
    native = show["data"].get("session")
    if not native:
        native_id = session.split(":", maxsplit=1)[1]
        native = {
            "session": session,
            "native_id": native_id,
            "host": host,
            "launch_id": launch_id,
            "worktree": value(arguments, "--worktree"),
            "state": "active",
        }
        show["data"]["session"] = native
    show_path.write_text(json.dumps(show), encoding="utf-8")

    launches_path = root / "agent-store-launches.json"
    launches = json.loads(launches_path.read_text(encoding="utf-8"))
    rows = [
        row for row in launches["data"]["launches"] if row["launch_id"] != launch_id
    ]
    rows.append(
        {
            **launch,
            "worktree": value(arguments, "--worktree"),
            "created_at": value(arguments, "--wrapper-started-at"),
        }
    )
    launches["data"]["launches"] = rows
    launches_path.write_text(json.dumps(launches), encoding="utf-8")


def main() -> None:
    root = Path(__file__).parent
    command, *arguments = sys.argv[1:]
    with (root / "agent-store.log").open("a", encoding="utf-8") as log:
        log.write(" ".join(sys.argv[1:]) + "\n")
    if command == "show" and arguments and ":" in arguments[0]:
        handle = arguments[0].replace(":", "-")
        session_path = root / f"agent-store-show-{handle}.json"
        if session_path.is_file():
            session_reply = json.loads(session_path.read_text(encoding="utf-8"))
            print(
                json.dumps(
                    {
                        "ok": session_reply["ok"],
                        "data": Data(session=session_reply["data"].get("session")),
                    }
                )
            )
            return
    reply_path = root / f"agent-store-{command}.json"
    reply: Reply = (
        json.loads(reply_path.read_text(encoding="utf-8"))
        if reply_path.exists()
        else {"ok": True, "data": {}}
    )
    if "--launch-id" in arguments:
        specific = (
            root / f"agent-store-{command}-{value(arguments, '--launch-id')}.json"
        )
        if specific.exists():
            reply = json.loads(specific.read_text(encoding="utf-8"))
    prebound = prebound_reply(root, command, arguments, reply)
    if prebound is not None:
        reply = prebound
    elif command == "launch" and reply["ok"]:
        record_launch(root, arguments)
    released_path = root / "released.json"
    released: list[Claim] = (
        json.loads(released_path.read_text(encoding="utf-8"))
        if released_path.exists()
        else []
    )
    if command == "release-worktree" and reply["ok"]:
        released.append(
            Claim(
                path=value(arguments, "--path"),
                session=value(arguments, "--session"),
                launch_id=value(arguments, "--launch-id"),
            )
        )
        released_path.write_text(json.dumps(released), encoding="utf-8")
        reply = {"ok": True, "data": {"released": True}}
    if command == "worktrees" and reply["ok"]:
        launch_id = (
            value(arguments, "--launch-id") if "--launch-id" in arguments else None
        )
        released_keys = {
            (claim["path"], claim["session"], claim["launch_id"]) for claim in released
        }
        reply["data"]["worktrees"] = [
            row
            for row in reply["data"]["worktrees"]
            if row["session"] == value(arguments, "--session")
            and row["launch_id"] == launch_id
            and (row["path"], row["session"], row["launch_id"]) not in released_keys
        ]
    print(json.dumps(reply))


if __name__ == "__main__":
    main()

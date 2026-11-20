"""A byte-transparent Codex proxy stand-in backed by a strict WebSocket peer."""

import base64
import hashlib
import json
import os
import struct
import subprocess
import sys
import threading
from dataclasses import dataclass
from enum import IntEnum
from pathlib import Path
from typing import BinaryIO


class Opcode(IntEnum):
    CONTINUATION = 0
    TEXT = 1
    CLOSE = 8
    PING = 9
    PONG = 10


@dataclass(frozen=True, slots=True)
class Frame:
    opcode: Opcode
    payload: bytes


def append(log: Path, entry: dict) -> None:
    with log.open("a", encoding="utf-8") as file:
        file.write(json.dumps(entry) + "\n")


def exact(source: BinaryIO, length: int) -> bytes:
    result = bytearray()
    while len(result) < length:
        chunk = source.read(length - len(result))
        if not chunk:
            raise EOFError("the client closed its pipe")
        result.extend(chunk)
    return bytes(result)


def receive(source: BinaryIO) -> Frame:
    first, second = exact(source, 2)
    if first & 0xF0 != 0x80 or not second & 0x80:
        raise ValueError("expected a final, masked client frame with no extensions")
    length = second & 0x7F
    if length == 126:
        length = int.from_bytes(exact(source, 2), "big")
    elif length == 127:
        length = int.from_bytes(exact(source, 8), "big")
    mask = exact(source, 4)
    payload = exact(source, length)
    return Frame(
        Opcode(first & 0x0F),
        bytes(byte ^ mask[index % 4] for index, byte in enumerate(payload)),
    )


def frame(opcode: Opcode, payload: bytes, *, final: bool = True) -> bytes:
    first = bytes([(0x80 if final else 0) | opcode])
    if len(payload) < 126:
        return first + bytes([len(payload)]) + payload
    if len(payload) <= 0xFFFF:
        return first + b"\x7e" + struct.pack("!H", len(payload)) + payload
    return first + b"\x7f" + struct.pack("!Q", len(payload)) + payload


def write(target: BinaryIO, data: bytes) -> None:
    target.write(data)
    target.flush()


def upgrade(source: BinaryIO, target: BinaryIO, log: Path, scenario: str) -> None:
    if scenario == "silent-upgrade":
        # Ignore even EOF: cleanup must terminate a genuinely stuck proxy.
        threading.Event().wait()
        return
    header = bytearray()
    while not header.endswith(b"\r\n\r\n"):
        header.extend(exact(source, 1))
        if len(header) > 16384:
            raise ValueError("WebSocket upgrade headers are too large")
    lines = header.decode("ascii").split("\r\n")
    headers = {}
    for line in lines[1:]:
        if line:
            name, value = line.split(":", 1)
            headers[name.lower()] = value.strip()
    if (
        lines[0] != "GET / HTTP/1.1"
        or headers["upgrade"].lower() != "websocket"
        or "upgrade" not in headers["connection"].lower()
        or headers["sec-websocket-version"] != "13"
        or len(base64.b64decode(headers["sec-websocket-key"], validate=True)) != 16
    ):
        raise ValueError("invalid WebSocket upgrade request")
    append(log, {"websocket": "upgrade", "host": headers["host"]})
    if scenario == "reject-upgrade":
        write(target, b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
        return
    digest = hashlib.sha1(
        (headers["sec-websocket-key"] + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode(
            "ascii"
        )
    ).digest()
    accept = base64.b64encode(digest).decode("ascii")
    if scenario == "bad-accept":
        accept = "invalid"
    response = f"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n".encode(
        "ascii"
    )
    # Split the HTTP response itself, then coalesce notifications with its last bytes.
    write(target, response[:19])
    write(
        target,
        response[19:]
        + frame(Opcode.TEXT, b'{"method":"test/early"}')
        + frame(Opcode.TEXT, b'{"id":1,"method":"test/server-request"}'),
    )


def reply(source: BinaryIO, target: BinaryIO, log: Path, message: dict) -> None:
    payload = json.dumps(message, ensure_ascii=False).encode("utf-8")
    # Even split UTF-8 code points across continuation frames. A ping before the
    # final fragment must be answered while the JSON message is still incomplete.
    split = payload.index(b"\xc3") + 1 if b"\xc3" in payload else len(payload) // 2
    write(
        target,
        frame(Opcode.TEXT, payload[:split], final=False)
        + frame(Opcode.PING, b"wire-check"),
    )
    pong = receive(source)
    if pong != Frame(Opcode.PONG, b"wire-check"):
        raise ValueError("the client did not answer the interleaved ping")
    append(log, {"websocket": "masked-pong"})
    write(target, frame(Opcode.CONTINUATION, payload[split:]))


def proxy(
    source: BinaryIO,
    target: BinaryIO,
    log: Path,
    scenario: str,
    publication: Path | None,
    shutdown: Path | None,
) -> None:
    if scenario == "stuck-launcher":
        child = subprocess.Popen([sys.executable, __file__, "hold-open"])
        append(log, {"descendant": child.pid})
        child.wait()
        return
    upgrade(source, target, log, scenario)
    if scenario in {"silent-upgrade", "reject-upgrade", "bad-accept"}:
        return
    observer_reads = 0
    while True:
        try:
            message = receive(source)
        except EOFError:
            return
        if message.opcode == Opcode.CLOSE:
            append(log, {"websocket": "masked-close"})
            write(target, frame(Opcode.CLOSE, message.payload))
            return
        if message.opcode != Opcode.TEXT:
            raise ValueError("expected a JSON text frame")
        request = json.loads(message.payload)
        method = request.get("method")
        append(
            log,
            {
                "proxy": method,
                "params": request.get("params"),
                "websocket": "masked-text",
            },
        )
        if method == "initialized":
            write(target, frame(Opcode.TEXT, b'{"method":"test/idle"}'))
        if "id" not in request:
            continue
        if scenario == "silent-initialize" or method == "test/timeout":
            source.read()
            return
        if (
            scenario in {"observer", "observer-disconnect"}
            and method == "thread/loaded/list"
        ):
            observer_reads += 1
            if scenario == "observer-disconnect" and observer_reads > 1:
                return
            if observer_reads == 1:
                assert publication is not None
                publication.write_text("new-revision", encoding="utf-8")
            reply(
                source,
                target,
                log,
                {
                    "id": request["id"],
                    "result": {"data": ["thread"] if observer_reads == 1 else []},
                },
            )
            continue
        if scenario == "observer-live" and method == "thread/loaded/list":
            assert shutdown is not None
            loaded = [] if shutdown.exists() else ["thread"]
            reply(
                source, target, log, {"id": request["id"], "result": {"data": loaded}}
            )
            continue
        if method in {"test/close", "test/close-delayed", "test/close-hold"}:
            write(target, frame(Opcode.CLOSE, b"\x03\xe8"))
            if method == "test/close-delayed":
                # Reproduce a peer scheduled after the client has flushed its reply.
                threading.Event().wait(0.2)
            closed = receive(source)
            if closed != Frame(Opcode.CLOSE, b"\x03\xe8"):
                raise ValueError("the client did not complete the close handshake")
            append(log, {"websocket": "close-acknowledged"})
            if method == "test/close-hold":
                threading.Event().wait()
            return
        if method == "test/abrupt":
            return
        if method == "test/malformed":
            write(target, frame(Opcode.TEXT, b"{invalid json"))
            continue
        if method == "test/error":
            reply(
                source,
                target,
                log,
                {"id": request["id"], "error": {"message": "fixture rejected request"}},
            )
        else:
            result: dict[str, object]
            if method == "initialize":
                result = {
                    "codexHome": os.environ.get("CODEX_HOME", ""),
                    "userAgent": "fixture/0.169.0",
                }
            elif method == "config/batchWrite":
                result = {"status": "ok"}
            elif method == "hooks/list":
                result = {
                    "data": [
                        {
                            "cwd": str(Path.cwd()),
                            "errors": [],
                            "hooks": [],
                            "warnings": [],
                        }
                    ]
                }
            else:
                result = request["params"] if method == "test/echo" else {}
            reply(source, target, log, {"id": request["id"], "result": result})


def main() -> None:
    if sys.argv[1:3] == ["app-server", "daemon"]:
        operation = sys.argv[3]
        append(Path(os.environ["FAKE_CODEX_LOG"]), {"daemon": operation})
        socket = (
            Path(os.environ["CODEX_HOME"])
            / "app-server-control/app-server-control.sock"
        )
        if (
            operation == "start"
            and Path(os.environ["FAKE_CODEX_LOG"]).with_suffix(".fail").exists()
        ):
            sys.exit("fixture daemon start refused")
        if operation == "start":
            socket.parent.mkdir(parents=True, exist_ok=True)
            socket.touch()
        if not socket.exists():
            sys.exit("fixture daemon is offline")
        print(
            json.dumps(
                {
                    "status": "running",
                    "socketPath": str(socket),
                    "backend": "pid",
                    "cliVersion": "0.169.0",
                    "appServerVersion": os.environ.get(
                        "FAKE_DAEMON_VERSION", "0.169.0"
                    ),
                }
            )
        )
        return
    if sys.argv[1:2] == ["--cd"] and sys.argv[-1] == "hold receipt fixture":
        log = Path(os.environ["FAKE_CODEX_LOG"])
        append(log, {"frontend": sys.argv[1:]})
        while not log.with_suffix(".exit").exists():
            threading.Event().wait(0.1)
        return
    if sys.argv[1:] == ["hold-open"]:
        threading.Event().wait()
        return
    if sys.argv[1:3] != ["app-server", "proxy"]:
        sys.exit(f"fake codex: unexpected {sys.argv[1:]}")
    log = Path(os.environ["FAKE_CODEX_LOG"])
    append(log, {"pid": os.getpid()})
    proxy(
        sys.stdin.buffer,
        sys.stdout.buffer,
        log,
        os.environ.get("FAKE_CODEX_SCENARIO", "normal"),
        Path(os.environ["FAKE_OBSERVER_PUBLICATION"])
        if "FAKE_OBSERVER_PUBLICATION" in os.environ
        else None,
        Path(os.environ["FAKE_OBSERVER_SHUTDOWN"])
        if "FAKE_OBSERVER_SHUTDOWN" in os.environ
        else None,
    )


if __name__ == "__main__":
    main()

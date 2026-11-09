"""A Claude session inbox stand-in: appends each connection's bytes to a capture file."""

import os
import socket
import sys

address, capture, ready = sys.argv[1:4]


def record(data: bytes) -> None:
    with open(capture, "ab") as file:
        file.write(data)


def windows() -> None:
    import _winapi

    while True:
        pipe = _winapi.CreateNamedPipe(
            address,
            _winapi.PIPE_ACCESS_DUPLEX,
            _winapi.PIPE_WAIT,
            1,
            65536,
            65536,
            0,
            _winapi.NULL,
        )
        open(ready, "w").close()
        _winapi.ConnectNamedPipe(pipe, False)
        data = b""
        while True:
            try:
                chunk, _ = _winapi.ReadFile(pipe, 65536, False)
            except BrokenPipeError:
                break
            if not chunk:
                break
            data += chunk
        record(data)
        _winapi.CloseHandle(pipe)


def unix() -> None:
    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    server.bind(address)
    server.listen()
    open(ready, "w").close()
    while True:
        connection, _ = server.accept()
        data = b""
        while chunk := connection.recv(65536):
            data += chunk
        record(data)
        connection.close()


windows() if os.name == "nt" else unix()

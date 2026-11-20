"""Run the pinned native Codex integration test with a canned local Responses API."""

import argparse
import http.server
import json
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import uuid
from collections.abc import Callable
from pathlib import Path
from typing import Any, ClassVar

VERSION = "0.169.0"
MODEL = "gpt-6.2-astra"


def write(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


def install(native: Path, root: Path, version: str) -> None:
    package = root / "catalog/plugins/smoke"
    write(
        package / ".codex-plugin/plugin.json",
        json.dumps({"name": "smoke", "version": version, "skills": "./skills/"}),
    )
    write(
        package / "skills/marker/SKILL.md",
        f"---\nname: marker\ndescription: fixture revision {version}\n---\n\n"
        f"Skill contents unique to {version}.\n",
    )
    write(
        package / "hooks/hooks.json",
        json.dumps(
            {
                "hooks": {
                    "PreToolUse": [
                        {
                            "matcher": "NeverAFixtureTool",
                            "hooks": [
                                {
                                    "type": "command",
                                    "command": f"echo fixture-{version}",
                                }
                            ],
                        }
                    ]
                }
            }
        ),
    )
    if version == "1.0.0":
        entry = {
            "name": "smoke",
            "source": {"source": "local", "path": "./plugins/smoke"},
            "category": "Productivity",
            "policy": {"installation": "AVAILABLE", "authentication": "ON_INSTALL"},
        }
        write(
            root / "catalog/.agents/plugins/marketplace.json",
            json.dumps({"name": "fixture", "plugins": [entry]}),
        )
        subprocess.run(
            [native, "plugin", "marketplace", "add", root / "catalog"],
            cwd=root / "project",
            check=True,
            timeout=30,
        )
    subprocess.run(
        [native, "plugin", "add", "smoke@fixture"],
        cwd=root / "project",
        check=True,
        timeout=30,
    )


class Provider(http.server.BaseHTTPRequestHandler):
    bodies: ClassVar[list[dict[str, Any]]] = []

    def do_POST(self) -> None:
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        self.bodies.append(body)
        index = str(len(self.bodies))
        events = [
            {"type": "response.created", "response": {"id": "resp-" + index}},
            {
                "type": "response.output_item.done",
                "item": {
                    "type": "message",
                    "id": "msg-" + index,
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "Fixture complete."}],
                },
            },
            {
                "type": "response.completed",
                "response": {
                    "id": "resp-" + index,
                    "usage": {
                        "input_tokens": 10,
                        "output_tokens": 3,
                        "total_tokens": 13,
                    },
                },
            },
        ]
        payload = "".join(f"data: {json.dumps(event)}\n\n" for event in events).encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)


def tmux_draft(
    native: Path,
    root: Path,
    env: dict[str, str],
    refresh: Callable[[], None],
) -> None:
    thread = (root / "thread-id").read_text(encoding="utf-8")
    tmux = ["tmux", "-S", str(root / "draft.sock"), "-f", "/dev/null"]

    def pane(*args: str) -> str:
        return subprocess.check_output([*tmux, *args], env=env, text=True, timeout=10)

    def displayed(text: str) -> None:
        deadline = time.monotonic() + 30
        while text not in (screen := pane("capture-pane", "-p")):
            if time.monotonic() >= deadline:
                raise AssertionError(f"Missing {text!r} in native TUI: {screen}")
            time.sleep(0.1)

    draft = f"TJ unsent draft {uuid.uuid4().hex}"
    before = len(Provider.bodies)
    try:
        pane(
            "new-session",
            "-d",
            "-x",
            "140",
            "-y",
            "45",
            "-c",
            str(root / "project"),
            str(native),
            "--cd",
            str(root / "project"),
            "resume",
            thread,
            "--model",
            MODEL,
            "-c",
            "model_reasoning_effort=high",
            "--remote",
            "unix://",
        )
        displayed("Fixture complete.")
        pid = pane("display-message", "-p", "#{pane_pid}").strip()
        assert Path(f"/proc/{pid}/exe").samefile(native), pid
        pane("send-keys", "-l", "--", draft)
        displayed(draft)
        subprocess.run(
            [sys.executable, __file__, "--install", "3.0.0"],
            env=env,
            check=True,
            timeout=60,
        )
        refresh()
        assert pane("display-message", "-p", "#{pane_pid}").strip() == pid
        displayed(draft)
        assert len(Provider.bodies) == before, "draft submitted before Enter"
        pane("send-keys", "Enter")
        deadline = time.monotonic() + 30
        while len(Provider.bodies) == before:
            if time.monotonic() >= deadline:
                raise AssertionError(
                    f"Draft never reached loopback: {pane('capture-pane', '-p')}"
                )
            time.sleep(0.1)
        request = Provider.bodies[before]
        assert any(
            part.get("text") == draft
            for item in request["input"]
            if item.get("role") == "user"
            for part in item["content"]
        ), "submitted draft changed"
        assert "fixture revision 3.0.0" in json.dumps(request)
        assert pane("display-message", "-p", "#{pane_pid}").strip() == pid
    finally:
        subprocess.run([*tmux, "kill-server"], env=env, check=False, timeout=10)


def run(native: Path, root: Path, port: int) -> None:
    version = subprocess.check_output(
        [native, "--version"], text=True, timeout=10
    ).strip()
    if version != f"codex-cli {VERSION}":
        raise ValueError(f"Expected codex-cli {VERSION}, found {version}")
    home, project = root / "home", root / "project"
    project.mkdir()
    subprocess.run(["git", "init", "-q", str(project)], check=True)
    write(
        home / "config.toml",
        f'''model = "{MODEL}"
check_for_update_on_startup = false
approval_policy = "never"
sandbox_mode = "read-only"
model_provider = "fixture"
model_reasoning_effort = "high"
[features]
plugins = true
hooks = true
[model_providers.fixture]
name = "Offline fixture"
base_url = "http://127.0.0.1:{port}/v1"
wire_api = "responses"
requires_openai_auth = false
[projects.{json.dumps(str(project))}]
trust_level = "trusted"
''',
    )
    write(project / ".codex/config.toml", '[plugins."smoke@fixture"]\nenabled = true\n')
    env = {
        key: value
        for key, value in os.environ.items()
        if key
        not in {
            "OPENAI_API_KEY",
            "CODEX_API_KEY",
            "CODEX_THREAD_ID",
            "TJ_RUN_LAUNCH_ID",
        }
    }
    env.update(
        CODEX_HOME=str(home),
        TJ_CACHE_DIR=str(root / "cache"),
        TJ_FIXTURE_ROOT=str(root),
        TJ_FIXTURE_CODEX=str(native),
        TJ_FIXTURE_PYTHON=sys.executable,
        HTTP_PROXY="http://127.0.0.1:9",
        HTTPS_PROXY="http://127.0.0.1:9",
        NO_PROXY="127.0.0.1,localhost",
        OTEL_SDK_DISABLED="true",
    )
    env["PATH"] = os.pathsep.join([str(Path("target/debug").resolve()), env["PATH"]])
    # Native maintenance creates the private state directory with the correct ACL.
    subprocess.run(
        [native, "app-server", "daemon", "stop"], env=env, check=True, timeout=30
    )
    write(
        home / "app-server-daemon/settings.json",
        json.dumps({"updater": {"autoUpdateEnabled": False}}),
    )
    subprocess.run(
        [sys.executable, __file__, "--install", "1.0.0"],
        env=env,
        check=True,
        timeout=60,
    )
    try:
        # Cargo's Windows child Job prevents a new native daemon from detaching.
        build = subprocess.check_output(
            [
                "cargo",
                "test",
                "--locked",
                "--bin",
                "tj",
                "--no-run",
                "--message-format=json",
            ],
            text=True,
            timeout=240,
        )
        (driver,) = [
            row["executable"]
            for line in build.splitlines()
            if (row := json.loads(line)).get("executable")
            and row["target"]["name"] == "tj"
        ]

        def check(test: str) -> None:
            subprocess.run(
                [
                    driver,
                    f"run::update::native_tests::{test}",
                    "--exact",
                    "--ignored",
                    "--nocapture",
                ],
                env=env,
                check=True,
                timeout=150,
            )

        check("pinned_native_cold_start")
        check("pinned_native_refresh")
        assert len(Provider.bodies) == 4, len(Provider.bodies)
        assert all(
            body["model"] == MODEL and body["reasoning"]["effort"] == "high"
            for body in Provider.bodies
        )
        for body, marker in zip(
            Provider.bodies,
            [
                "fixture revision 1.0.0",
                "fixture revision 2.0.0",
                "Skill contents unique to 2.0.0.",
                "Same-path fixture edit.",
            ],
            strict=True,
        ):
            assert marker in json.dumps(body), marker
        if sys.platform == "linux":
            tmux_draft(native, root, env, lambda: check("refresh_thread_with_draft"))
    finally:
        subprocess.run(
            [native, "app-server", "daemon", "stop"], env=env, check=True, timeout=60
        )


def main() -> None:
    parser = argparse.ArgumentParser(__doc__)
    parser.add_argument("--native", type=Path)
    parser.add_argument("--install", choices=["1.0.0", "2.0.0", "3.0.0"])
    args = parser.parse_args()
    if args.install:
        install(
            Path(os.environ["TJ_FIXTURE_CODEX"]),
            Path(os.environ["TJ_FIXTURE_ROOT"]),
            args.install,
        )
        return
    with (
        tempfile.TemporaryDirectory(prefix="tjnd-") as scratch,
        http.server.ThreadingHTTPServer(("127.0.0.1", 0), Provider) as provider,
    ):
        root = Path(scratch).resolve()
        native = args.native
        if native is None:
            package = root / "npm"
            subprocess.run(
                [
                    shutil.which("npm") or "npm",
                    "install",
                    "--prefix",
                    str(package),
                    "--ignore-scripts",
                    "--no-audit",
                    "--no-fund",
                    f"@openai/codex@{VERSION}",
                ],
                check=True,
                timeout=120,
            )
            (native,) = package.glob(
                "node_modules/**/vendor/*/bin/codex.exe"
                if os.name == "nt"
                else "node_modules/**/vendor/*/bin/codex"
            )
        worker = threading.Thread(target=provider.serve_forever, daemon=True)
        worker.start()
        try:
            run(native.resolve(strict=True), root, provider.server_port)
        finally:
            provider.shutdown()
            worker.join()


if __name__ == "__main__":
    main()

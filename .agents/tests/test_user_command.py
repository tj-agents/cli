import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCRIPTS = ROOT / ".agents/cli/utility/clip/scripts"
sys.path.insert(0, str(SCRIPTS))
SPEC = importlib.util.spec_from_file_location("user_command", SCRIPTS / "user_command.py")
USER_COMMAND = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = USER_COMMAND
SPEC.loader.exec_module(USER_COMMAND)

PROBE = "import os, sys; print(repr(sys.argv[1:]), os.environ.get('CAPTURED'), os.environ.get('SECRET'))"
AWKWARD = ["a b", "it's", "$HOME", "`x`", '"q"', "back\\slash", "https://github.com/tj-agents/core/pull/1"]


def git_bash() -> str | None:
    if os.name != "nt":
        return shutil.which("bash")
    git = shutil.which("git")
    candidates = [parent / "bin" / "bash.exe" for parent in Path(git).resolve().parents] if git else []
    return next((str(candidate) for candidate in candidates if candidate.is_file()), None)


def spec(shell: str, failing_capture: bool = False) -> object:
    capture = [sys.executable, "-c", "import sys; sys.exit(3)" if failing_capture else "print('captured value')"]
    return USER_COMMAND.parse_spec({
        "shell": shell,
        "unset": ["SECRET"],
        "steps": [{"argv": capture, "capture": "CAPTURED"}, {"argv": [sys.executable, "-c", PROBE, *AWKWARD]}],
    }, {})


class RenderedCommandRuns(unittest.TestCase):
    def run_line(self, interpreter: list[str], line: str, suffix: str = "") -> subprocess.CompletedProcess[str]:
        self.assertTrue(line.startswith("!"))
        with tempfile.TemporaryDirectory() as directory:
            script = Path(directory) / f"command{suffix}"
            script.write_text(line[1:] + ("\nexit $LASTEXITCODE" if suffix == ".ps1" else ""), encoding="utf-8")
            return subprocess.run([*interpreter, str(script)], capture_output=True, text=True, check=False,
                                  env={**os.environ, "SECRET": "leaked"}, timeout=60)

    def test_bash_preserves_arguments_clears_environment_and_captures(self):
        bash = git_bash()
        if not bash:
            self.skipTest("bash is not available")
        result = self.run_line([bash], USER_COMMAND.render(spec("bash")))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), f"{AWKWARD!r} captured value None")

    def test_bash_stops_at_the_first_failed_step(self):
        bash = git_bash()
        if not bash:
            self.skipTest("bash is not available")
        result = self.run_line([bash], USER_COMMAND.render(spec("bash", failing_capture=True)))
        self.assertEqual(result.returncode, 3)
        self.assertEqual(result.stdout, "")

    def test_powershell_preserves_arguments_clears_environment_and_captures(self):
        pwsh = shutil.which("pwsh")
        if not pwsh:
            self.skipTest("pwsh is not available")
        line = USER_COMMAND.render(spec("powershell"), powershell="pwsh")
        result = self.run_line([pwsh, "-NoProfile", "-File"], line, ".ps1")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), f"{AWKWARD!r} captured value None")

    def test_powershell_stops_at_the_first_failed_step(self):
        pwsh = shutil.which("pwsh")
        if not pwsh:
            self.skipTest("pwsh is not available")
        line = USER_COMMAND.render(spec("powershell", failing_capture=True), powershell="pwsh")
        result = self.run_line([pwsh, "-NoProfile", "-File"], line, ".ps1")
        self.assertEqual(result.returncode, 3)
        self.assertEqual(result.stdout.strip(), "")


class SpecValidation(unittest.TestCase):
    def test_rejects_malformed_specs(self):
        for bad in (
            [],
            {"steps": []},
            {"steps": [{"argv": []}]},
            {"steps": [{"argv": ["gh", "a\nb"]}]},
            {"steps": [{"argv": ["gh"], "capture": "1BAD"}]},
            {"steps": [{"argv": ["gh"]}], "unset": ["A B"]},
            {"steps": [{"argv": ["gh"]}], "shell": "cmd"},
            {"steps": [{"argv": ["gh"]}], "extra": True},
        ):
            with self.subTest(bad=bad), self.assertRaises(USER_COMMAND.UserCommandError):
                USER_COMMAND.parse_spec(bad, {})

    def test_windows_program_paths_render_for_bash(self):
        rendered = USER_COMMAND.render(USER_COMMAND.parse_spec(
            {"shell": "bash", "steps": [{"argv": [r"C:\Program Files\GitHub CLI\gh.exe", "pr", "view"]}]}, {}),
            windows=True)
        self.assertEqual(rendered, """!bash -c '"C:/Program Files/GitHub CLI/gh.exe" pr view || exit'""")


class DefaultShell(unittest.TestCase):
    def test_claude_uses_its_configured_default_shell(self):
        with tempfile.TemporaryDirectory() as directory:
            environ = {"CLAUDECODE": "1", "CLAUDE_CONFIG_DIR": directory}
            self.assertIs(USER_COMMAND.default_shell(environ, windows=True), USER_COMMAND.Shell.BASH)
            (Path(directory) / "settings.json").write_text(json.dumps({"defaultShell": "powershell"}),
                                                           encoding="utf-8")
            self.assertIs(USER_COMMAND.default_shell(environ, windows=True), USER_COMMAND.Shell.POWERSHELL)

    def test_codex_uses_powershell_only_on_windows(self):
        environ = {"CODEX_THREAD_ID": "thread"}
        self.assertIs(USER_COMMAND.default_shell(environ, windows=True), USER_COMMAND.Shell.POWERSHELL)
        self.assertIs(USER_COMMAND.default_shell(environ, windows=False), USER_COMMAND.Shell.BASH)


class IssueAndVerify(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.environ = {"AGENT_STATE_DIRECTORY": self.directory.name}
        self.copied = []

    def issue(self, *argv: str) -> str:
        parsed = USER_COMMAND.parse_spec({"shell": "bash", "steps": [{"argv": list(argv)}]}, {})
        command, receipt = USER_COMMAND.issue(parsed, self.environ, copier=lambda text: self.copied.append(text) or "ok")
        self.assertEqual((self.copied[-1], receipt), (command, "ok"))
        return command

    def test_issued_commands_pass_wherever_the_reply_shows_them(self):
        command = self.issue("gh", "pr", "review", "--approve", "https://github.com/tj-agents/cli/pull/21")
        for reply in (
            f"Approve with:\n\n```bash\n{command}\n```\n",
            f"Approve with:\n\n```\n{command}\n```",
            f"Run `{command}` in the prompt.",
            f"- {command}",
            f"> {command}",
        ):
            with self.subTest(reply=reply):
                self.assertEqual(USER_COMMAND.unissued(reply, self.environ), [])

    def test_hand_written_commands_are_reported(self):
        self.issue("gh", "auth", "status")
        reply = "Run `! gh auth login` then:\n\n```sh\n!gh pr review --approve 1\n```\n!git push\n"
        self.assertEqual(USER_COMMAND.unissued(reply, self.environ),
                         ["! gh auth login", "!gh pr review --approve 1", "!git push"])

    def test_text_that_is_not_a_command_is_ignored(self):
        reply = ("![diagram](d.png)\nUse `!=` and `!` here.\n\n```rust\n!self.is_empty()\n```\n"
                 "```python\n!pip install x\n```\nWow!\n")
        self.assertEqual(USER_COMMAND.commands(reply), [])

    def test_record_is_bounded(self):
        for index in range(USER_COMMAND.RECORD_LIMIT + 5):
            USER_COMMAND.record(f"!echo {index}", self.environ)
        lines = USER_COMMAND.record_path(self.environ).read_text(encoding="utf-8").splitlines()
        self.assertEqual(len(lines), USER_COMMAND.RECORD_LIMIT)
        self.assertNotIn(USER_COMMAND.digest("!echo 0"), USER_COMMAND.issued(self.environ))
        self.assertIn(USER_COMMAND.digest("!echo 204"), USER_COMMAND.issued(self.environ))

    def test_verify_entry_point_exits_one_for_unissued_commands(self):
        environment = {**os.environ, **self.environ}
        script = str(SCRIPTS / "user_command.py")
        issued = self.issue("gh", "auth", "status")
        for reply, code, output in ((f"`{issued}`", 0, ""), ("`!gh auth login`", 1, "!gh auth login")):
            result = subprocess.run([sys.executable, "-B", script, "verify"], input=reply.encode(),
                                    capture_output=True, env=environment, check=False, timeout=30)
            self.assertEqual((result.returncode, result.stdout.decode().strip()), (code, output))


class Setup(unittest.TestCase):
    def test_setup_installs_and_check_detects_staleness(self):
        with tempfile.TemporaryDirectory() as directory:
            environ = {"AGENT_STATE_DIRECTORY": directory}
            with self.assertRaises(USER_COMMAND.UserCommandError):
                USER_COMMAND.setup(True, environ)
            USER_COMMAND.setup(False, environ)
            USER_COMMAND.setup(True, environ)
            installed = USER_COMMAND.install_directory(environ) / "copy_draft.py"
            installed.write_text("stale", encoding="utf-8")
            with self.assertRaises(USER_COMMAND.UserCommandError):
                USER_COMMAND.setup(True, environ)


if __name__ == "__main__":
    unittest.main()

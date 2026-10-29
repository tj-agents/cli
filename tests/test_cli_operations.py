import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[1]

from tj_cli import close, closeout, processes


def git(cwd, *arguments):
    result = subprocess.run(
        ["git", "-C", str(cwd), *arguments], capture_output=True, text=True, check=True
    )
    return result.stdout.strip()


class CloseOperationTests(unittest.TestCase):
    @unittest.skipUnless(
        sys.platform.startswith("linux") and hasattr(os, "fork"),
        "requires Linux process state",
    )
    def test_exited_unreaped_frontend_is_not_live(self):
        pid = os.fork()
        if pid == 0:
            os._exit(0)
        try:
            import time

            deadline = time.monotonic() + 3
            while time.monotonic() < deadline:
                raw = (Path("/proc") / str(pid) / "stat").read_text()
                if raw[raw.rfind(")") + 2 :].split()[0] == "Z":
                    break
                time.sleep(0.01)
            else:
                self.fail("disposable child did not exit")
            self.assertIsNone(processes.process_info(pid))
        finally:
            os.waitpid(pid, 0)

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        info = processes.process_info(__import__("os").getpid())
        self.assertIsNotNone(info)
        self.target = {
            "schema_version": 1,
            "frontend": {"pid": info.pid, "started_at": info.started_at},
            "wrapper": {"pid": info.pid, "started_at": info.started_at},
            "terminal": {"kind": "tmux", "pane_id": "%1"},
        }

    def test_process_identity_rejects_reused_or_wrong_start(self):
        self.assertTrue(processes.matches(self.target["frontend"]))
        changed = {
            **self.target["frontend"],
            "started_at": self.target["frontend"]["started_at"] + 1,
        }
        self.assertFalse(processes.matches(changed))

    def test_close_refusal_writes_durable_receipt_without_process_signal(self):
        receipt = self.directory / "receipt.json"
        with mock.patch.object(
            close.terminal, "close", side_effect=processes.Refusal("tab ambiguous")
        ):
            with self.assertRaisesRegex(processes.Refusal, "tab ambiguous"):
                close.close(self.target, receipt)
        value = json.loads(receipt.read_text(encoding="utf-8"))
        self.assertEqual("refused", value["status"])
        self.assertEqual("tab ambiguous", value["reason"])

    def test_cancellation_file_prevents_armed_mutation(self):
        target = self.directory / "target.json"
        receipt = self.directory / "receipt.json"
        completion = self.directory / "completion.json"
        cancel = self.directory / "cancel.json"
        target.write_text(json.dumps(self.target), encoding="utf-8")
        cancel.write_text("cancel", encoding="utf-8")
        with mock.patch.object(close.terminal, "close") as terminal_close:
            result = close.main(
                [
                    "watch",
                    "--target-json",
                    json.dumps(self.target),
                    "--armed-at",
                    str(__import__("time").time()),
                    "--receipt",
                    str(receipt),
                    "--completion",
                    str(completion),
                    "--completion-token",
                    "token",
                    "--cancel",
                    str(cancel),
                    "--delay",
                    "0",
                    "--timeout",
                    "0.1",
                ]
            )
        self.assertEqual(0, result)
        terminal_close.assert_not_called()
        self.assertEqual(
            "cancelled", json.loads(receipt.read_text(encoding="utf-8"))["status"]
        )

    def test_matching_completion_authorizes_close(self):
        target = self.directory / "target.json"
        receipt = self.directory / "receipt.json"
        completion = self.directory / "completion.json"
        cancel = self.directory / "cancel.json"
        target.write_text(json.dumps(self.target), encoding="utf-8")
        with (
            mock.patch.object(close, "_completion_matches", return_value=True),
            mock.patch.object(close, "close") as close_exact,
        ):
            result = close.main(
                [
                    "watch",
                    "--target-json",
                    json.dumps(self.target),
                    "--armed-at",
                    str(__import__("time").time()),
                    "--receipt",
                    str(receipt),
                    "--completion",
                    str(completion),
                    "--completion-token",
                    "token",
                    "--cancel",
                    str(cancel),
                    "--delay",
                    "0",
                    "--timeout",
                    "0.1",
                ]
            )
        self.assertEqual(0, result)
        close_exact.assert_called_once_with(self.target, receipt)

    def test_arm_freezes_target_and_arming_time_before_watcher_starts(self):
        target_path = self.directory / "target.json"
        receipt = self.directory / "receipt.json"
        completion = self.directory / "completion.json"
        cancel = self.directory / "cancel.json"
        target_path.write_text(json.dumps(self.target), encoding="utf-8")
        with mock.patch.object(close.subprocess, "Popen") as launch:
            self.assertEqual(
                0,
                close.main(
                    [
                        "arm",
                        "--target",
                        str(target_path),
                        "--receipt",
                        str(receipt),
                        "--completion",
                        str(completion),
                        "--completion-token",
                        "token",
                        "--cancel",
                        str(cancel),
                        "--delay",
                        "0",
                        "--timeout",
                        "1",
                    ]
                ),
            )
        command = launch.call_args.args[0]
        self.assertNotIn("--target", command)
        frozen = json.loads(command[command.index("--target-json") + 1])
        self.assertEqual(self.target, frozen)
        armed_at = float(command[command.index("--armed-at") + 1])
        self.assertEqual(
            armed_at, json.loads(receipt.read_text(encoding="utf-8"))["armed_at"]
        )
        replacement = {
            **self.target,
            "frontend": {**self.target["frontend"], "pid": 99999},
        }
        target_path.write_text(json.dumps(replacement), encoding="utf-8")
        completion.write_text(json.dumps({"token": "token"}), encoding="utf-8")
        watcher = close.parser().parse_args(command[5:])
        with mock.patch.object(close, "close") as close_exact:
            self.assertEqual(0, close.watch(watcher))
        close_exact.assert_called_once_with(self.target, receipt)


class VerifiedProcessStopTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if os.name != "nt" and not (
            sys.platform.startswith("linux") and hasattr(os, "pidfd_open")
        ):
            raise unittest.SkipTest("requires a supported held process handle")

    def setUp(self):
        self.processes = []
        self.addCleanup(self._clean_processes)

    def _clean_processes(self):
        for process in self.processes:
            if process.poll() is None:
                process.terminate()
                process.wait(timeout=5)

    def _frontend(self):
        process = subprocess.Popen(
            [sys.executable, "-c", "import time; time.sleep(30)"]
        )
        self.processes.append(process)
        info = processes.process_info(process.pid)
        self.assertIsNotNone(info)
        return process, {"pid": info.pid, "started_at": info.started_at}

    def _assert_preserved(self, frontend, sibling):
        self.assertIsNotNone(frontend.poll())
        self.assertIsNone(sibling.poll(), "sibling process was signalled")
        self.assertTrue(
            processes.matches(
                {
                    "pid": os.getpid(),
                    "started_at": processes.process_info(os.getpid()).started_at,
                }
            ),
            "parent process changed",
        )
        self.assertTrue(ROOT.is_dir(), "checkout was removed")

    def test_stops_only_the_disposable_frontend(self):
        frontend, identity = self._frontend()
        sibling, _sibling_identity = self._frontend()
        processes.stop(identity, before_signal=lambda: None)
        self._assert_preserved(frontend, sibling)

    def test_wrong_birth_refuses_without_signalling_any_process(self):
        frontend, identity = self._frontend()
        sibling, _sibling_identity = self._frontend()
        wrong_birth = {**identity, "started_at": identity["started_at"] + 1}
        before_signal = mock.Mock()
        with self.assertRaisesRegex(processes.Refusal, "identity changed"):
            processes.stop(wrong_birth, before_signal=before_signal)
        before_signal.assert_not_called()
        self.assertIsNone(frontend.poll(), "wrong birth signalled the frontend")
        self.assertIsNone(sibling.poll(), "wrong birth signalled the sibling")

    def test_oversized_pid_refuses_before_guard_or_signal(self):
        frontend, identity = self._frontend()
        sibling, _sibling_identity = self._frontend()
        oversized = {**identity, "pid": identity["pid"] + 2**32}
        before_signal = mock.Mock()
        with self.assertRaisesRegex(processes.Refusal, "pid"):
            processes.stop(oversized, before_signal=before_signal)
        before_signal.assert_not_called()
        self.assertIsNone(frontend.poll(), "oversized pid signalled the frontend")
        self.assertIsNone(sibling.poll(), "oversized pid signalled the sibling")

    def test_refusing_new_input_guard_preserves_the_frontend(self):
        frontend, identity = self._frontend()
        sibling, _sibling_identity = self._frontend()

        def new_input():
            raise processes.Refusal("frontend input changed")

        with self.assertRaisesRegex(processes.Refusal, "input changed"):
            processes.stop(identity, before_signal=new_input)
        self.assertIsNone(frontend.poll(), "refusing guard signalled the frontend")
        self.assertIsNone(sibling.poll(), "refusing guard signalled the sibling")

    def test_raising_guard_preserves_the_frontend(self):
        frontend, identity = self._frontend()
        sibling, _sibling_identity = self._frontend()
        with self.assertRaisesRegex(RuntimeError, "guard failed"):
            processes.stop(
                identity,
                before_signal=lambda: (_ for _ in ()).throw(
                    RuntimeError("guard failed")
                ),
            )
        self.assertIsNone(frontend.poll(), "raising guard signalled the frontend")
        self.assertIsNone(sibling.poll(), "raising guard signalled the sibling")


class TerminalBindingTests(unittest.TestCase):
    def test_packaged_probe_runs_without_site_packages(self):
        probe = ROOT / "plugins" / "cli" / "runtime" / "tj_cli" / "close" / "probe.py"
        result = subprocess.run([sys.executable, "-I", "-B", str(probe), "--help"], cwd=ROOT,
                                capture_output=True, text=True, encoding="utf-8", check=False)
        self.assertEqual(0, result.returncode, result.stderr)
        self.assertIn("usage:", result.stdout)

    def test_windows_runtime_ids_disambiguate_tabs_with_empty_automation_ids(self):
        values = [
            {
                "runtimeId": "42:100:2",
                "automationId": "",
                "processId": 77,
                "title": "same title",
            },
            {
                "runtimeId": "42:100:3",
                "automationId": "",
                "processId": 77,
                "title": "same title",
            },
        ]
        response = subprocess.CompletedProcess([], 0, json.dumps(values), "")
        with (
            mock.patch.object(close.terminal, "os", SimpleNamespace(name="nt")),
            mock.patch.object(close.terminal, "_run", return_value=response),
        ):
            inventory = close.terminal._windows({"kind": "windows-terminal"})
        self.assertEqual(
            ["42:100:2", "42:100:3"], [item["runtime_id"] for item in inventory]
        )
        target = {"kind": "windows-terminal", "tab_binding": {"runtime_id": "42:100:3"}}
        self.assertFalse(close.terminal._matches(inventory[0], target))
        self.assertTrue(close.terminal._matches(inventory[1], target))

    def setUp(self):
        info = processes.ProcessInfo
        self.processes = {
            10: info(10, 1, 100.0),
            20: info(20, 10, 200.0),
            30: info(30, 20, 300.0),
            77: info(77, 1, 400.0),
        }
        self.target = {
            "schema_version": 1,
            "frontend": {"pid": 30, "started_at": 300.0},
            "wrapper": {"pid": 20, "started_at": 200.0},
            "terminal": {"kind": "tmux", "pane_id": "%1"},
        }

    def test_wrapper_beneath_native_pane_root_owns_frontend(self):
        with (
            mock.patch.object(
                processes, "process_info", side_effect=self.processes.get
            ),
            mock.patch.object(
                close.terminal, "_one", return_value={"kind": "tmux", "pid": 10}
            ),
        ):
            close.terminal.verify_binding(self.target)

    def test_windows_same_window_sibling_refuses_without_console_tab_association(self):
        target = {
            **self.target,
            "terminal": {
                "kind": "windows-terminal",
                "tab_binding": {
                    "runtime_id": "tab-b",
                    "observed_title": "launch title",
                    "evidence": "windows-uia-unique-title-at-launch",
                },
            },
        }
        with (
            mock.patch.object(
                processes, "process_info", side_effect=self.processes.get
            ),
            mock.patch.object(
                close.terminal,
                "_one",
                return_value={
                    "kind": "windows-terminal",
                    "runtime_id": "tab-b",
                    "pid": 77,
                },
            ),
            mock.patch.object(
                close.terminal, "_windows_console_association", return_value=False
            ),
            mock.patch.object(close.terminal, "_run") as mutate,
        ):
            with self.assertRaisesRegex(
                processes.Refusal, "cannot prove frontend belongs"
            ):
                close.terminal.close(target)
        mutate.assert_not_called()

    def test_windows_console_association_runs_isolated_probe_and_refuses_negative_result(
        self,
    ):
        identity = {
            "kind": "windows-terminal",
            "tab_binding": {
                "runtime_id": "tab-b",
                "observed_title": "launch title",
                "evidence": "windows-uia-unique-title-at-launch",
            },
        }
        frontend = {"pid": 30, "started_at": 300.0}
        success = subprocess.CompletedProcess([], 0, '{"associated": true}', "")
        with (
            mock.patch.object(close.terminal, "os", SimpleNamespace(name="nt")),
            mock.patch.multiple(
                close.terminal.subprocess,
                DETACHED_PROCESS=8,
                CREATE_NEW_PROCESS_GROUP=512,
                create=True,
            ),
            mock.patch.object(
                close.terminal.subprocess, "run", return_value=success
            ) as run_probe,
        ):
            self.assertTrue(
                close.terminal._windows_console_association(frontend, identity)
            )
        command = run_probe.call_args.args[0]
        self.assertEqual(sys.executable, command[0])
        self.assertEqual("probe.py", Path(command[3]).name)
        self.assertTrue(Path(command[3]).is_absolute())
        self.assertEqual(
            ["--frontend-pid", "30", "--tab-id", "tab-b", "--inventory-adapter"],
            command[4:9],
        )
        self.assertTrue(run_probe.call_args.kwargs["creationflags"])
        negative = subprocess.CompletedProcess([], 2, '{"associated": false}', "")
        with (
            mock.patch.object(close.terminal, "os", SimpleNamespace(name="nt")),
            mock.patch.object(close.terminal.subprocess, "run", return_value=negative),
        ):
            self.assertFalse(
                close.terminal._windows_console_association(frontend, identity)
            )

    def test_last_tmux_pane_accepts_only_verified_server_disappearance(self):
        result = subprocess.CompletedProcess(["tmux"], 0, "", "")
        with (
            mock.patch.object(close.terminal, "verify_binding"),
            mock.patch.object(close.terminal, "_run", return_value=result),
            mock.patch.object(close.terminal, "_one", return_value={"kind": "tmux", "pane_id": "%1", "pid": 10}),
            mock.patch.object(
                close.terminal,
                "_inventory",
                side_effect=processes.Refusal("tmux inventory failed"),
            ),
            mock.patch.object(
                close.terminal, "_tmux_server_disappeared", return_value=True
            ),
        ):
            close.terminal.close(self.target)

    def test_close_identity_runs_guard_after_exact_enumeration(self):
        result = subprocess.CompletedProcess(["tmux"], 0, "", "")
        events = []
        with (
            mock.patch.object(
                close.terminal,
                "_one",
                side_effect=lambda _identity: (
                    events.append("one") or {"kind": "tmux", "pane_id": "%1", "pid": 10}
                ),
            ),
            mock.patch.object(
                close.terminal,
                "_run",
                side_effect=lambda _command: events.append("close") or result,
            ),
            mock.patch.object(close.terminal, "_inventory", return_value=[]),
        ):
            close.terminal.close_identity(
                {"kind": "tmux", "pane_id": "%1"},
                before_close=lambda: events.append("guard"),
            )
        self.assertEqual(["one", "guard", "close"], events)

    def test_focus_tmux_selects_window_then_pane_then_client(self):
        result = subprocess.CompletedProcess(["tmux"], 0, "", "")
        with (
            mock.patch.object(
                close.terminal,
                "_one",
                return_value={"kind": "tmux", "pane_id": "%1", "pid": 10},
            ),
            mock.patch.object(close.terminal, "_run", return_value=result) as command,
        ):
            close.terminal.focus({"kind": "tmux", "pane_id": "%1"})
        self.assertEqual(
            ["select-window", "select-pane", "switch-client"],
            [call.args[0][-3] for call in command.call_args_list],
        )


class CloseoutPreflightTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        root = Path(self.temporary.name)
        self.remote = root / "remote.git"
        self.primary = root / "primary"
        self.linked = root / "linked"
        subprocess.run(["git", "init", "--bare", "-q", str(self.remote)], check=True)
        subprocess.run(
            ["git", "init", "-q", "-b", "main", str(self.primary)], check=True
        )
        git(self.primary, "config", "user.name", "test")
        git(self.primary, "config", "user.email", "test@example.invalid")
        (self.primary / "base.txt").write_text("base", encoding="utf-8")
        git(self.primary, "add", "base.txt")
        git(self.primary, "commit", "-q", "-m", "base")
        git(self.primary, "remote", "add", "origin", str(self.remote))
        git(self.primary, "push", "-q", "-u", "origin", "main")
        git(self.primary, "worktree", "add", "-q", "-b", "feature", str(self.linked))
        (self.linked / "feature.txt").write_text("feature", encoding="utf-8")
        git(self.linked, "add", "feature.txt")
        git(self.linked, "commit", "-q", "-m", "feature")
        git(self.linked, "push", "-q", "-u", "origin", "feature")
        git(self.primary, "merge", "--no-ff", "-q", "feature", "-m", "merge feature")
        git(self.primary, "push", "-q", "origin", "main")
        git(self.linked, "fetch", "-q", "origin")
        self.head = git(self.linked, "rev-parse", "HEAD")
        self.eligibility = root / "eligibility.json"
        info = processes.process_info(__import__("os").getpid())
        self.target = {
            "schema_version": 1,
            "frontend": {"pid": info.pid, "started_at": info.started_at},
            "wrapper": {"pid": info.pid, "started_at": info.started_at},
            "terminal": {"kind": "tmux", "pane_id": "%1"},
        }
        self.write_eligibility()

    def write_eligibility(self, target=None, remote_base="origin/main"):
        target = self.target if target is None else target
        self.eligibility.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "worktree": str(self.linked.resolve()),
                    "head": self.head,
                    "branch": "feature",
                    "remote_base": remote_base,
                    "remote_base_oid": git(self.linked, "rev-parse", "origin/main"),
                    "target": target,
                    "merged_to_remote_base": True,
                    "no_live_other_owner": True,
                    "observed_at": __import__("time").time(),
                    "provenance": {"source": "engineering", "evidence_id": "proof-1"},
                }
            ),
            encoding="utf-8",
        )

    def test_preflight_requires_positive_external_ownership_evidence(self):
        self.eligibility.write_text("{}", encoding="utf-8")
        with self.assertRaisesRegex(
            closeout.Refusal, "eligibility lacks matching positive"
        ):
            closeout.preflight(
                self.linked,
                self.head,
                "feature",
                "origin/main",
                self.eligibility,
                self.target,
            )

    def test_preflight_refuses_local_base_and_mismatched_target_proof(self):
        with self.assertRaisesRegex(closeout.Refusal, "remote base"):
            closeout.preflight(
                self.linked, self.head, "feature", "main", self.eligibility, self.target
            )
        wrong_target = {
            **self.target,
            "frontend": {
                **self.target["frontend"],
                "pid": self.target["frontend"]["pid"] + 1,
            },
        }
        self.write_eligibility(wrong_target)
        with self.assertRaisesRegex(closeout.Refusal, "target"):
            closeout.preflight(
                self.linked,
                self.head,
                "feature",
                "origin/main",
                self.eligibility,
                self.target,
            )

    def test_ignored_env_refuses_before_close_and_retry_preserves_local_data(self):
        ignored = self.linked / ".env"
        (self.linked / ".gitignore").write_text(".env\n", encoding="utf-8")
        git(self.linked, "add", ".gitignore")
        git(self.linked, "commit", "-q", "-m", "ignore local env")
        self.head = git(self.linked, "rev-parse", "HEAD")
        git(self.linked, "push", "-q")
        git(
            self.primary, "merge", "--no-ff", "-q", "feature", "-m", "merge ignored env"
        )
        git(self.primary, "push", "-q", "origin", "main")
        git(self.linked, "fetch", "-q", "origin")
        self.write_eligibility()
        ignored.write_text("KEEP_ME", encoding="utf-8")
        target = self.primary.parent / "target.json"
        receipt = self.primary.parent / "receipt.json"
        target.write_text(json.dumps(self.target), encoding="utf-8")
        with mock.patch.object(closeout.close, "close") as close_exact:
            result = closeout.main(
                [
                    "run",
                    "--target",
                    str(target),
                    "--worktree",
                    str(self.linked),
                    "--expected-head",
                    self.head,
                    "--expected-branch",
                    "feature",
                    "--remote-base",
                    "origin/main",
                    "--eligibility",
                    str(self.eligibility),
                    "--receipt",
                    str(receipt),
                ]
            )
        self.assertEqual(2, result)
        close_exact.assert_not_called()
        self.assertEqual("KEEP_ME", ignored.read_text(encoding="utf-8"))
        receipt.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "operation": "cli-closeout",
                    "status": "closed-removal-failed",
                    "target": self.target,
                    "worktree": str(self.linked.resolve()),
                }
            ),
            encoding="utf-8",
        )
        result = closeout.main(
            [
                "retry-remove",
                "--worktree",
                str(self.linked),
                "--expected-head",
                self.head,
                "--expected-branch",
                "feature",
                "--remote-base",
                "origin/main",
                "--eligibility",
                str(self.eligibility),
                "--receipt",
                str(receipt),
            ]
        )
        self.assertEqual(2, result)
        self.assertEqual("KEEP_ME", ignored.read_text(encoding="utf-8"))

    def test_assume_unchanged_edit_refuses_initial_and_retry_without_removal(self):
        hidden = self.linked / "feature.txt"
        git(self.linked, "update-index", "--assume-unchanged", "feature.txt")
        hidden.write_text("KEEP_HIDDEN_EDIT", encoding="utf-8")
        target = self.primary.parent / "target.json"
        receipt = self.primary.parent / "receipt.json"
        target.write_text(json.dumps(self.target), encoding="utf-8")
        with mock.patch.object(closeout.close, "close") as close_exact:
            result = closeout.main(
                [
                    "run",
                    "--target",
                    str(target),
                    "--worktree",
                    str(self.linked),
                    "--expected-head",
                    self.head,
                    "--expected-branch",
                    "feature",
                    "--remote-base",
                    "origin/main",
                    "--eligibility",
                    str(self.eligibility),
                    "--receipt",
                    str(receipt),
                ]
            )
        self.assertEqual(2, result)
        close_exact.assert_not_called()
        self.assertTrue(self.linked.exists())
        self.assertEqual("KEEP_HIDDEN_EDIT", hidden.read_text(encoding="utf-8"))
        receipt.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "operation": "cli-closeout",
                    "status": "closed-removal-failed",
                    "target": self.target,
                    "worktree": str(self.linked.resolve()),
                }
            ),
            encoding="utf-8",
        )
        result = closeout.main(
            [
                "retry-remove",
                "--worktree",
                str(self.linked),
                "--expected-head",
                self.head,
                "--expected-branch",
                "feature",
                "--remote-base",
                "origin/main",
                "--eligibility",
                str(self.eligibility),
                "--receipt",
                str(receipt),
            ]
        )
        self.assertEqual(2, result)
        self.assertTrue(self.linked.exists())
        self.assertEqual("KEEP_HIDDEN_EDIT", hidden.read_text(encoding="utf-8"))

    def test_installed_closeout_module_reaches_argument_parser(self):
        result = subprocess.run(
            [sys.executable, "-I", "-B", "-m", "tj_cli.closeout", "--help"],
            cwd=ROOT,
            capture_output=True,
            text=True,
            encoding="utf-8",
            check=False,
        )
        self.assertEqual(0, result.returncode, result.stderr)
        self.assertIn("usage:", result.stdout)

    def test_closeout_removes_only_the_verified_linked_worktree(self):
        target = self.primary.parent / "target.json"
        receipt = self.primary.parent / "receipt.json"
        target.write_text(json.dumps(self.target), encoding="utf-8")
        with mock.patch.object(closeout.close, "close") as close_exact:
            result = closeout.main(
                [
                    "run",
                    "--target",
                    str(target),
                    "--worktree",
                    str(self.linked),
                    "--expected-head",
                    self.head,
                    "--expected-branch",
                    "feature",
                    "--remote-base",
                    "origin/main",
                    "--eligibility",
                    str(self.eligibility),
                    "--receipt",
                    str(receipt),
                ]
            )
        self.assertEqual(0, result)
        close_exact.assert_called_once()
        self.assertFalse(self.linked.exists())
        self.assertTrue(self.primary.exists())
        self.assertEqual(
            "closed-and-removed",
            json.loads(receipt.read_text(encoding="utf-8"))["status"],
        )

    def test_retry_removal_rechecks_receipt_and_preflight_without_closing_again(self):
        receipt = self.primary.parent / "receipt.json"
        receipt.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "operation": "cli-closeout",
                    "status": "closed-removal-failed",
                    "target": self.target,
                    "worktree": str(self.linked.resolve()),
                }
            ),
            encoding="utf-8",
        )
        with mock.patch.object(closeout.close, "close") as close_exact:
            result = closeout.main(
                [
                    "retry-remove",
                    "--worktree",
                    str(self.linked),
                    "--expected-head",
                    self.head,
                    "--expected-branch",
                    "feature",
                    "--remote-base",
                    "origin/main",
                    "--eligibility",
                    str(self.eligibility),
                    "--receipt",
                    str(receipt),
                ]
            )
        self.assertEqual(0, result)
        close_exact.assert_not_called()
        self.assertFalse(self.linked.exists())
        self.assertTrue(json.loads(receipt.read_text(encoding="utf-8"))["retry"])

    def test_post_close_change_prevents_removal_and_failed_retry_stays_safe(self):
        target_path = self.primary.parent / "target.json"
        receipt = self.primary.parent / "receipt.json"
        target_path.write_text(json.dumps(self.target), encoding="utf-8")

        def close_then_change(*_args, **_kwargs):
            (self.linked / "late.txt").write_text("late", encoding="utf-8")

        with mock.patch.object(closeout.close, "close", side_effect=close_then_change):
            result = closeout.main(
                [
                    "run",
                    "--target",
                    str(target_path),
                    "--worktree",
                    str(self.linked),
                    "--expected-head",
                    self.head,
                    "--expected-branch",
                    "feature",
                    "--remote-base",
                    "origin/main",
                    "--eligibility",
                    str(self.eligibility),
                    "--receipt",
                    str(receipt),
                ]
            )
        self.assertEqual(2, result)
        self.assertTrue(self.linked.exists())
        self.assertEqual(
            "closed-removal-failed",
            json.loads(receipt.read_text(encoding="utf-8"))["status"],
        )
        result = closeout.main(
            [
                "retry-remove",
                "--worktree",
                str(self.linked),
                "--expected-head",
                self.head,
                "--expected-branch",
                "feature",
                "--remote-base",
                "origin/main",
                "--eligibility",
                str(self.eligibility),
                "--receipt",
                str(receipt),
            ]
        )
        self.assertEqual(2, result)
        self.assertTrue(self.linked.exists())


if __name__ == "__main__":
    unittest.main()

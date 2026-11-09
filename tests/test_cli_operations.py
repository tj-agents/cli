import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[1]

from tj_cli import processes


class ProcessIdentityTests(unittest.TestCase):
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


if __name__ == "__main__":
    unittest.main()

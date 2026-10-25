import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[1]
CLOSE_SCRIPTS = ROOT / ".agents" / "cli" / "utility" / "close" / "scripts"


def load(name, path):
    specification = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(specification)
    sys.path.insert(0, str(path.parent))
    specification.loader.exec_module(module)
    return module


close = load("test_close", CLOSE_SCRIPTS / "close.py")
closeout = load("test_closeout", ROOT / ".agents" / "cli" / "utility" / "closeout" / "scripts" / "closeout.py")


def git(cwd, *arguments):
    result = subprocess.run(["git", "-C", str(cwd), *arguments], capture_output=True, text=True, check=True)
    return result.stdout.strip()


class CloseOperationTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        info = close.process_identity.process_info(__import__("os").getpid())
        self.assertIsNotNone(info)
        self.target = {"schema_version": 1, "frontend": {"pid": info.pid, "started_at": info.started_at},
                       "terminal": {"kind": "tmux", "pane_id": "%1",
                                    "binding": {"pid": info.pid, "started_at": info.started_at}}}

    def test_process_identity_rejects_reused_or_wrong_start(self):
        self.assertTrue(close.process_identity.matches(self.target["frontend"]))
        changed = {**self.target["frontend"], "started_at": self.target["frontend"]["started_at"] + 1}
        self.assertFalse(close.process_identity.matches(changed))

    def test_close_refusal_writes_durable_receipt_without_process_signal(self):
        receipt = self.directory / "receipt.json"
        with mock.patch.object(close.terminal, "close", side_effect=close.process_identity.Refusal("tab ambiguous")):
            with self.assertRaisesRegex(close.process_identity.Refusal, "tab ambiguous"):
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
            result = close.main(["watch", "--target", str(target), "--receipt", str(receipt), "--completion",
                                 str(completion), "--completion-token", "token", "--cancel", str(cancel),
                                 "--delay", "0", "--timeout", "0.1"])
        self.assertEqual(0, result)
        terminal_close.assert_not_called()
        self.assertEqual("cancelled", json.loads(receipt.read_text(encoding="utf-8"))["status"])

    def test_matching_completion_authorizes_close(self):
        target = self.directory / "target.json"
        receipt = self.directory / "receipt.json"
        completion = self.directory / "completion.json"
        cancel = self.directory / "cancel.json"
        target.write_text(json.dumps(self.target), encoding="utf-8")
        with mock.patch.object(close, "_completion_matches", return_value=True), \
                mock.patch.object(close, "close") as close_exact:
            result = close.main(["watch", "--target", str(target), "--receipt", str(receipt), "--completion",
                                 str(completion), "--completion-token", "token", "--cancel", str(cancel),
                                 "--delay", "0", "--timeout", "0.1"])
        self.assertEqual(0, result)
        close_exact.assert_called_once_with(self.target, receipt)


class CloseoutPreflightTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        root = Path(self.temporary.name)
        self.remote = root / "remote.git"
        self.primary = root / "primary"
        self.linked = root / "linked"
        subprocess.run(["git", "init", "--bare", "-q", str(self.remote)], check=True)
        subprocess.run(["git", "init", "-q", "-b", "main", str(self.primary)], check=True)
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
        info = close.process_identity.process_info(__import__("os").getpid())
        self.target = {"schema_version": 1, "frontend": {"pid": info.pid, "started_at": info.started_at},
                       "terminal": {"kind": "tmux", "pane_id": "%1",
                                    "binding": {"pid": info.pid, "started_at": info.started_at}}}
        self.write_eligibility()

    def write_eligibility(self, target=None, remote_base="origin/main"):
        target = self.target if target is None else target
        self.eligibility.write_text(json.dumps({"schema_version": 1, "worktree": str(self.linked.resolve()),
            "head": self.head, "branch": "feature", "remote_base": remote_base,
            "remote_base_oid": git(self.linked, "rev-parse", "origin/main"), "target": target,
            "merged_to_remote_base": True, "no_live_other_owner": True, "observed_at": __import__("time").time(),
            "provenance": {"source": "engineering", "evidence_id": "proof-1"}}), encoding="utf-8")

    def test_preflight_requires_positive_external_ownership_evidence(self):
        self.eligibility.write_text("{}", encoding="utf-8")
        with self.assertRaisesRegex(closeout.Refusal, "eligibility lacks matching positive"):
            closeout.preflight(self.linked, self.head, "feature", "origin/main", self.eligibility, self.target)

    def test_preflight_refuses_local_base_and_mismatched_target_proof(self):
        with self.assertRaisesRegex(closeout.Refusal, "remote base"):
            closeout.preflight(self.linked, self.head, "feature", "main", self.eligibility, self.target)
        wrong_target = {**self.target, "frontend": {**self.target["frontend"], "pid": self.target["frontend"]["pid"] + 1}}
        self.write_eligibility(wrong_target)
        with self.assertRaisesRegex(closeout.Refusal, "target"):
            closeout.preflight(self.linked, self.head, "feature", "origin/main", self.eligibility, self.target)

    def test_closeout_removes_only_the_verified_linked_worktree(self):
        target = self.primary.parent / "target.json"
        receipt = self.primary.parent / "receipt.json"
        target.write_text(json.dumps(self.target), encoding="utf-8")
        with mock.patch.object(closeout.close, "close") as close_exact:
            result = closeout.main(["run", "--target", str(target), "--worktree", str(self.linked),
                "--expected-head", self.head, "--expected-branch", "feature", "--remote-base", "origin/main",
                "--eligibility", str(self.eligibility), "--receipt", str(receipt)])
        self.assertEqual(0, result)
        close_exact.assert_called_once()
        self.assertFalse(self.linked.exists())
        self.assertTrue(self.primary.exists())
        self.assertEqual("closed-and-removed", json.loads(receipt.read_text(encoding="utf-8"))["status"])

    def test_retry_removal_rechecks_receipt_and_preflight_without_closing_again(self):
        receipt = self.primary.parent / "receipt.json"
        receipt.write_text(json.dumps({"schema_version": 1, "operation": "cli-closeout",
            "status": "closed-removal-failed", "target": self.target, "worktree": str(self.linked.resolve())}),
            encoding="utf-8")
        with mock.patch.object(closeout.close, "close") as close_exact:
            result = closeout.main(["retry-remove", "--worktree", str(self.linked), "--expected-head", self.head,
                "--expected-branch", "feature", "--remote-base", "origin/main", "--eligibility",
                str(self.eligibility), "--receipt", str(receipt)])
        self.assertEqual(0, result)
        close_exact.assert_not_called()
        self.assertFalse(self.linked.exists())
        self.assertTrue(json.loads(receipt.read_text(encoding="utf-8"))["retry"])

    def test_post_close_change_prevents_removal_and_failed_retry_stays_safe(self):
        target_path = self.primary.parent / "target.json"
        receipt = self.primary.parent / "receipt.json"
        target_path.write_text(json.dumps(self.target), encoding="utf-8")

        def close_then_change(*_args):
            (self.linked / "late.txt").write_text("late", encoding="utf-8")

        with mock.patch.object(closeout.close, "close", side_effect=close_then_change):
            result = closeout.main(["run", "--target", str(target_path), "--worktree", str(self.linked),
                "--expected-head", self.head, "--expected-branch", "feature", "--remote-base", "origin/main",
                "--eligibility", str(self.eligibility), "--receipt", str(receipt)])
        self.assertEqual(2, result)
        self.assertTrue(self.linked.exists())
        self.assertEqual("closed-removal-failed", json.loads(receipt.read_text(encoding="utf-8"))["status"])
        result = closeout.main(["retry-remove", "--worktree", str(self.linked), "--expected-head", self.head,
            "--expected-branch", "feature", "--remote-base", "origin/main", "--eligibility",
            str(self.eligibility), "--receipt", str(receipt)])
        self.assertEqual(2, result)
        self.assertTrue(self.linked.exists())


if __name__ == "__main__":
    unittest.main()

"""Close one exact CLI target and remove one separately verified linked worktree."""

from __future__ import annotations

import argparse
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import time


ELIGIBILITY_MAX_AGE_SECONDS = 300


def _close_module():
    here = Path(__file__).resolve()
    candidates = []
    for root in (here.parent, *here.parents):
        candidates.extend((
            root / "runtime" / "close" / "close.py",
            root / ".agents" / "cli" / "utility" / "close" / "scripts" / "close.py",
        ))
    path = next((candidate for candidate in candidates if candidate.is_file()), None)
    if path is None:
        raise RuntimeError("cli close runtime is unavailable from this package")
    sys.path.insert(0, str(path.parent))
    specification = importlib.util.spec_from_file_location("cli_close", path)
    if specification is None or specification.loader is None:
        raise RuntimeError("cli close runtime is unavailable")
    module = importlib.util.module_from_spec(specification)
    specification.loader.exec_module(module)
    return module


close = _close_module()
Refusal = close.process_identity.Refusal


def _git(cwd: Path, *arguments: str) -> str:
    result = subprocess.run(["git", "-C", str(cwd), *arguments], stdin=subprocess.DEVNULL,
                            capture_output=True, text=True, encoding="utf-8", timeout=10, check=False)
    if result.returncode:
        raise Refusal((result.stderr or result.stdout or "git command failed").strip())
    return result.stdout.strip()


def _json(path: Path) -> dict:
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise Refusal("eligibility must be a JSON object")
    return value


def _worktree_records(cwd: Path) -> list[dict]:
    output = _git(cwd, "worktree", "list", "--porcelain")
    records, current = [], None
    for line in output.splitlines():
        key, _separator, value = line.partition(" ")
        if key == "worktree":
            current = {"path": Path(value).resolve(), "locked": False, "primary": False}
            records.append(current)
        elif current is not None and key == "locked":
            current["locked"] = True
        elif current is not None and key == "bare":
            current["primary"] = True
    if records:
        records[0]["primary"] = True
    return records


def _primary_checkout(worktree: Path) -> Path:
    common = Path(_git(worktree, "rev-parse", "--path-format=absolute", "--git-common-dir")).resolve()
    if common.name != ".git":
        raise Refusal("repository has no ordinary primary checkout")
    return common.parent


def _remote_base(worktree: Path, remote_base: str) -> tuple[str, str]:
    reference = _git(worktree, "rev-parse", "--symbolic-full-name", "--verify", remote_base)
    prefix = "refs/remotes/"
    if not reference.startswith(prefix) or "/" not in reference[len(prefix):]:
        raise Refusal("remote base must resolve to one configured remote-tracking ref")
    remote = reference[len(prefix):].split("/", 1)[0]
    _git(worktree, "remote", "get-url", remote)
    oid = _git(worktree, "rev-parse", "--verify", reference + "^{commit}")
    return reference, oid


def _reject_hidden_index_flags(worktree: Path) -> None:
    output = _git(worktree, "ls-files", "-v", "-z")
    for record in output.split("\0"):
        flag = record[:1]
        if flag.islower() or flag in {"S", "s"}:
            raise Refusal("target worktree has assume-unchanged or skip-worktree tracked files")


def _require_eligibility(
    path: Path,
    worktree: Path,
    head: str,
    branch: str,
    remote_base: str,
    remote_base_oid: str,
    target: dict,
) -> None:
    value = _json(path)
    expected = {"schema_version": 1, "worktree": str(worktree), "head": head, "branch": branch,
                "remote_base": remote_base, "remote_base_oid": remote_base_oid, "target": target,
                "merged_to_remote_base": True, "no_live_other_owner": True}
    for key, wanted in expected.items():
        if value.get(key) != wanted:
            raise Refusal("eligibility lacks matching positive " + key)
    observed = value.get("observed_at")
    if not isinstance(observed, (int, float)) or observed <= 0 or not 0 <= time.time() - observed <= ELIGIBILITY_MAX_AGE_SECONDS:
        raise Refusal("eligibility observation is absent, future, or stale")
    provenance = value.get("provenance")
    if not isinstance(provenance, dict) or not all(
        isinstance(provenance.get(key), str) and provenance[key].strip() for key in ("source", "evidence_id")
    ):
        raise Refusal("eligibility lacks observation provenance")


def preflight(
    worktree: Path,
    expected_head: str,
    expected_branch: str,
    remote_base: str,
    eligibility: Path,
    target: dict,
) -> None:
    if not worktree.is_absolute():
        raise Refusal("worktree must be an absolute path")
    worktree = worktree.resolve()
    if Path.cwd().resolve() == worktree or Path.cwd().resolve().is_relative_to(worktree):
        raise Refusal("closeout helper must run outside the target worktree")
    top = Path(_git(worktree, "rev-parse", "--show-toplevel")).resolve()
    if top != worktree:
        raise Refusal("target is not the exact worktree root")
    record = next((item for item in _worktree_records(worktree) if item["path"] == worktree), None)
    if record is None or record["primary"] or record["locked"]:
        raise Refusal("target is absent, primary, or locked")
    if _git(worktree, "status", "--porcelain", "--ignored=matching", "--untracked-files=all"):
        raise Refusal("target worktree is dirty or has untracked or ignored local files")
    _reject_hidden_index_flags(worktree)
    head = _git(worktree, "rev-parse", "HEAD")
    branch = _git(worktree, "symbolic-ref", "--quiet", "--short", "HEAD")
    if head != expected_head or branch != expected_branch:
        raise Refusal("target HEAD or branch no longer matches")
    _reference, remote_base_oid = _remote_base(worktree, remote_base)
    merged = subprocess.run(["git", "-C", str(worktree), "merge-base", "--is-ancestor", head, remote_base_oid],
                            stdin=subprocess.DEVNULL, capture_output=True, timeout=10, check=False)
    if merged.returncode:
        raise Refusal("target HEAD is not merged into the explicit remote base")
    upstream = _git(worktree, "rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{upstream}")
    if _git(worktree, "rev-list", upstream + "..HEAD"):
        raise Refusal("target has unpushed commits")
    _require_eligibility(eligibility, worktree, head, branch, remote_base, remote_base_oid, target)


def receipt(path: Path, target: dict, status: str, **extra: object) -> None:
    close._write_json(path, {"schema_version": 1, "operation": "cli-closeout", "status": status,
                             "at": time.time(), "target": target, **extra})


def run(args: argparse.Namespace) -> int:
    target = close.target(args.target)
    worktree = args.worktree.resolve()
    try:
        preflight(worktree, args.expected_head, args.expected_branch, args.remote_base, args.eligibility, target)
    except (OSError, ValueError, subprocess.SubprocessError, Refusal) as error:
        receipt(args.receipt, target, "refused", phase="preflight", reason=str(error))
        raise Refusal(str(error)) from error
    try:
        close.close(target, args.receipt)
    except Refusal as error:
        receipt(args.receipt, target, "refused", phase="closure", reason=str(error), worktree=str(worktree))
        raise
    receipt(args.receipt, target, "closed", worktree=str(worktree), removal="pending")
    try:
        preflight(worktree, args.expected_head, args.expected_branch, args.remote_base, args.eligibility, target)
        _git(_primary_checkout(worktree), "worktree", "remove", str(worktree))
    except (OSError, subprocess.SubprocessError, Refusal) as error:
        receipt(args.receipt, target, "closed-removal-failed", worktree=str(worktree), reason=str(error))
        return 2
    receipt(args.receipt, target, "closed-and-removed", worktree=str(worktree))
    return 0


def retry_remove(args: argparse.Namespace) -> int:
    value = _json(args.receipt)
    worktree = args.worktree.resolve()
    target = value.get("target")
    if value.get("operation") != "cli-closeout" or value.get("status") != "closed-removal-failed":
        raise Refusal("receipt does not describe a failed closeout removal")
    if value.get("worktree") != str(worktree) or not isinstance(target, dict):
        raise Refusal("receipt does not bind this exact worktree")
    try:
        close.validate_target(target)
    except Refusal as error:
        raise Refusal("receipt has no valid original target") from error
    try:
        preflight(worktree, args.expected_head, args.expected_branch, args.remote_base, args.eligibility, target)
        _git(_primary_checkout(worktree), "worktree", "remove", str(worktree))
    except (OSError, ValueError, subprocess.SubprocessError, Refusal) as error:
        receipt(args.receipt, target, "closed-removal-failed", worktree=str(worktree), reason=str(error))
        return 2
    receipt(args.receipt, target, "closed-and-removed", worktree=str(worktree), retry=True)
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    run_command = commands.add_parser("run")
    retry_command = commands.add_parser("retry-remove")
    for command in (run_command, retry_command):
        command.add_argument("--worktree", type=Path, required=True)
        command.add_argument("--expected-head", required=True)
        command.add_argument("--expected-branch", required=True)
        command.add_argument("--remote-base", required=True)
        command.add_argument("--eligibility", type=Path, required=True)
        command.add_argument("--receipt", type=Path, required=True)
    run_command.add_argument("--target", type=Path, required=True)
    args = parser.parse_args(argv)
    try:
        return run(args) if args.command == "run" else retry_remove(args)
    except (OSError, ValueError, subprocess.SubprocessError, Refusal) as error:
        print("cli:closeout: " + str(error), file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())

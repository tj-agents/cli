from copy import deepcopy

import pytest

from tj_cli.native_release import qualified_revision

_SHA = "a" * 40
_REPOSITORY = "tj-agents/cli"


def _run(path: str, identifier: int = 1) -> dict[str, object]:
    return {
        "id": identifier,
        "path": path,
        "event": "push",
        "head_sha": _SHA,
        "head_branch": "default",
        "head_repository": {"full_name": _REPOSITORY},
        "status": "completed",
        "conclusion": "success",
    }


def test_successful_latest_push_runs_qualify() -> None:
    runs = [_run(".github/workflows/ci.yml"), _run(".github/workflows/rust-ci.yml")]
    assert (
        qualified_revision(
            {"workflow_run": runs[0]}, _REPOSITORY, "default", _SHA, runs
        )
        == _SHA
    )


@pytest.mark.parametrize("conclusion", ["failure", "cancelled", None])
def test_newer_unsuccessful_run_blocks_older_success(conclusion: str | None) -> None:
    runs = [_run(".github/workflows/ci.yml"), _run(".github/workflows/rust-ci.yml")]
    newer = _run(".github/workflows/ci.yml", 2)
    newer["conclusion"] = conclusion
    if conclusion is None:
        newer["status"] = "in_progress"
    runs.append(newer)
    assert (
        qualified_revision(
            {"workflow_run": runs[0]}, _REPOSITORY, "default", _SHA, runs
        )
        is None
    )


@pytest.mark.parametrize(
    ("field", "value"),
    [
        ("event", "pull_request"),
        ("head_sha", "b" * 40),
        ("head_repository", {"full_name": "foreign/cli"}),
    ],
)
def test_untrusted_or_superseded_event_cannot_publish(
    field: str, value: object
) -> None:
    runs = [_run(".github/workflows/ci.yml"), _run(".github/workflows/rust-ci.yml")]
    event_run = deepcopy(runs[0])
    event_run[field] = value
    assert (
        qualified_revision(
            {"workflow_run": event_run}, _REPOSITORY, "default", _SHA, runs
        )
        is None
    )


def test_latest_rerun_attempt_must_succeed() -> None:
    runs = [_run(".github/workflows/ci.yml"), _run(".github/workflows/rust-ci.yml")]
    attempt = deepcopy(runs[0])
    attempt.update(run_attempt=2, conclusion="failure")
    runs.append(attempt)
    assert (
        qualified_revision(
            {"workflow_run": runs[0]}, _REPOSITORY, "default", _SHA, runs
        )
        is None
    )

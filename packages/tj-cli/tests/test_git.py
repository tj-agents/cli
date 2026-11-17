import subprocess
from pathlib import Path

import pytest

from tj_cli.git import repository_file


def _git(directory: Path, *arguments: str) -> None:
    subprocess.run(
        ["git", *arguments],
        check=True,
        cwd=directory,
        capture_output=True,
        text=True,
    )


def _repository_with_worktree(tmp_path: Path) -> tuple[Path, Path]:
    primary = tmp_path / "primary space ü"
    primary.mkdir()
    _git(primary, "init")
    _git(primary, "config", "user.email", "test@example.com")
    _git(primary, "config", "user.name", "Test User")
    (primary / "tracked.txt").write_text("tracked\n", encoding="utf-8")
    _git(primary, "add", "tracked.txt")
    _git(primary, "commit", "-m", "initial")

    worktree = tmp_path / "linked space ü"
    _git(primary, "worktree", "add", "-b", "linked", str(worktree))
    return primary, worktree


def test_repository_file_inherits_ignored_untracked_primary_file(
    tmp_path: Path,
) -> None:
    primary, worktree = _repository_with_worktree(tmp_path)
    relative = Path(".agent") / "selection"
    selection = primary / relative
    (primary / ".gitignore").write_text(".agent/\n", encoding="utf-8")
    selection.parent.mkdir()
    selection.write_text("primary", encoding="utf-8")

    assert repository_file(worktree, relative) == selection


def test_repository_file_supports_relative_worktree_metadata(tmp_path: Path) -> None:
    primary, _ = _repository_with_worktree(tmp_path)
    relative = Path(".agent") / "selection"
    selection = primary / relative
    selection.parent.mkdir()
    selection.write_text("primary", encoding="utf-8")
    checkout = tmp_path / "relative checkout ü"
    metadata = checkout / "metadata"
    checkout.mkdir()
    metadata.mkdir()
    (checkout / ".git").write_text("gitdir: metadata\n", encoding="utf-8")
    (metadata / "gitdir").write_text("../.git\n", encoding="utf-8")
    (metadata / "commondir").write_text(
        f"../../{primary.name}/.git\n", encoding="utf-8"
    )

    assert repository_file(checkout, relative) == selection


def test_repository_file_prefers_invalid_local_override(tmp_path: Path) -> None:
    primary, worktree = _repository_with_worktree(tmp_path)
    relative = Path(".agent") / "selection"
    inherited = primary / relative
    inherited.parent.mkdir()
    inherited.write_text("primary", encoding="utf-8")
    override = worktree / relative
    override.parent.mkdir()
    override.write_bytes(b"\xff")

    assert repository_file(worktree, relative) == override


def test_repository_file_keeps_broken_local_link(tmp_path: Path) -> None:
    repository = tmp_path / "repository"
    repository.mkdir()
    broken = repository / "selection"
    try:
        broken.symlink_to("missing")
    except OSError:
        pytest.skip("creating symlinks is unavailable")

    assert repository_file(repository, "selection") == broken


@pytest.mark.parametrize("relative", [Path("/absolute"), Path("..") / "outside"])
def test_repository_file_rejects_paths_outside_repository(
    tmp_path: Path, relative: Path
) -> None:
    with pytest.raises(ValueError, match="relative"):
        repository_file(tmp_path, relative)


def test_repository_file_does_not_borrow_parent_repository(tmp_path: Path) -> None:
    parent, _ = _repository_with_worktree(tmp_path)
    child = parent / "nested repository"
    child.mkdir()
    _git(child, "init")
    inherited = parent / "selection"
    inherited.write_text("parent", encoding="utf-8")

    assert repository_file(child, "selection") == child / "selection"


def test_repository_file_ignores_corrupt_worktree_metadata(tmp_path: Path) -> None:
    checkout = tmp_path / "checkout"
    metadata = checkout / "metadata"
    primary = tmp_path / "primary"
    checkout.mkdir()
    metadata.mkdir()
    (checkout / ".git").write_text("gitdir: metadata\n", encoding="utf-8")
    (metadata / "gitdir").write_text("wrong-checkout/.git\n", encoding="utf-8")
    (metadata / "commondir").write_text("../primary/.git\n", encoding="utf-8")
    (primary / ".git" / "objects").mkdir(parents=True)
    (primary / ".git" / "HEAD").write_text("ref: refs/heads/main\n", encoding="utf-8")
    inherited = primary / "selection"
    inherited.write_text("primary", encoding="utf-8")

    assert repository_file(checkout, "selection") == checkout / "selection"


def test_repository_file_uses_main_checkout_and_bare_repositories_locally(
    tmp_path: Path,
) -> None:
    primary, _ = _repository_with_worktree(tmp_path)
    selection = primary / "selection"
    selection.write_text("primary", encoding="utf-8")
    bare = tmp_path / "bare"
    _git(tmp_path, "init", "--bare", str(bare))

    assert repository_file(primary, "selection") == selection
    assert repository_file(bare, "selection") == bare / "selection"


def test_repository_file_keeps_standalone_and_separate_git_directory_local(
    tmp_path: Path,
) -> None:
    standalone = tmp_path / "standalone"
    standalone.mkdir()
    checkout = tmp_path / "separate checkout"
    gitdir = tmp_path / "git storage"
    _git(tmp_path, "init", "--separate-git-dir", str(gitdir), str(checkout))

    assert repository_file(standalone, "selection") == standalone / "selection"
    assert repository_file(checkout, "selection") == checkout / "selection"

import os
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

from tj_cli.git_environment import native_plugin_environment


def test_appends_longpaths_and_preserves_existing_auth_configuration() -> None:
    environment = {
        "GIT_CONFIG_COUNT": "2",
        "GIT_CONFIG_KEY_0": "http.https://github.com/.extraheader",
        "GIT_CONFIG_VALUE_0": "Authorization: basic sensitive-value",
        "GIT_CONFIG_KEY_1": "core.longpaths",
        "GIT_CONFIG_VALUE_1": "false",
    }

    updated = native_plugin_environment(environment, windows=True)

    assert updated == {
        **environment,
        "GIT_CONFIG_COUNT": "3",
        "GIT_CONFIG_KEY_2": "core.longpaths",
        "GIT_CONFIG_VALUE_2": "true",
    }
    assert environment["GIT_CONFIG_COUNT"] == "2"


@pytest.mark.parametrize("value", ["true", "YES", "On", "1"])
def test_honors_last_case_insensitive_longpaths_setting(value: str) -> None:
    environment = {
        "GIT_CONFIG_COUNT": "2",
        "GIT_CONFIG_KEY_0": "core.longpaths",
        "GIT_CONFIG_VALUE_0": "true",
        "GIT_CONFIG_KEY_1": "Core.LongPaths",
        "GIT_CONFIG_VALUE_1": value,
    }

    assert native_plugin_environment(environment, windows=True) == environment


def test_appends_after_last_false_longpaths_override() -> None:
    environment = {
        "GIT_CONFIG_COUNT": "2",
        "GIT_CONFIG_KEY_0": "core.longpaths",
        "GIT_CONFIG_VALUE_0": "true",
        "GIT_CONFIG_KEY_1": "CORE.LONGPATHS",
        "GIT_CONFIG_VALUE_1": "false",
    }

    updated = native_plugin_environment(environment, windows=True)

    assert updated["GIT_CONFIG_KEY_0"] == "core.longpaths"
    assert updated["GIT_CONFIG_VALUE_0"] == "true"
    assert updated["GIT_CONFIG_KEY_1"] == "CORE.LONGPATHS"
    assert updated["GIT_CONFIG_VALUE_1"] == "false"
    assert updated["GIT_CONFIG_KEY_2"] == "core.longpaths"
    assert updated["GIT_CONFIG_VALUE_2"] == "true"


def test_is_idempotent() -> None:
    first = native_plugin_environment({"GIT_CONFIG_COUNT": "0"}, windows=True)

    assert native_plugin_environment(first, windows=True) == first


def test_missing_count_defaults_to_zero() -> None:
    assert native_plugin_environment({}, windows=True) == {
        "GIT_CONFIG_COUNT": "1",
        "GIT_CONFIG_KEY_0": "core.longpaths",
        "GIT_CONFIG_VALUE_0": "true",
    }


@pytest.mark.parametrize(
    "orphan",
    [
        {"GIT_CONFIG_KEY_1": "unlisted.key"},
        {"GIT_CONFIG_VALUE_1": "unlisted value"},
    ],
)
def test_refuses_to_overwrite_undeclared_append_slot(orphan: dict[str, str]) -> None:
    environment = {
        "GIT_CONFIG_COUNT": "1",
        "GIT_CONFIG_KEY_0": "already.declared",
        "GIT_CONFIG_VALUE_0": "preserve-me",
        **orphan,
    }
    before = environment.copy()

    with pytest.raises(
        ValueError, match="invalid Git config entry 1: append slot already occupied"
    ):
        native_plugin_environment(environment, windows=True)

    assert environment == before


def test_linux_returns_copy_without_validating_git_configuration() -> None:
    environment = {"GIT_CONFIG_COUNT": "invalid", "KEEP": "value"}

    updated = native_plugin_environment(environment, windows=False)

    assert updated == environment
    assert updated is not environment


@pytest.mark.parametrize("count", ["-1", "+1", " 1", "\uff11", "1x"])
def test_rejects_invalid_count_without_mutating_input(count: str) -> None:
    environment = {"GIT_CONFIG_COUNT": count, "SECRET": "do-not-report"}
    before = environment.copy()

    with pytest.raises(ValueError) as caught:
        native_plugin_environment(environment, windows=True)

    assert str(caught.value) == "invalid GIT_CONFIG_COUNT"
    assert "do-not-report" not in str(caught.value)
    assert environment == before


@pytest.mark.parametrize(
    ("entry", "message"),
    [
        ({"GIT_CONFIG_VALUE_0": "secret"}, "invalid Git config entry 0: missing KEY"),
        (
            {"GIT_CONFIG_KEY_0": "", "GIT_CONFIG_VALUE_0": "secret"},
            "invalid Git config entry 0: missing KEY",
        ),
        (
            {"GIT_CONFIG_KEY_0": "http.extraheader"},
            "invalid Git config entry 0: missing VALUE",
        ),
    ],
)
def test_rejects_malformed_entries_without_mutating_or_disclosing_values(
    entry: dict[str, str], message: str
) -> None:
    environment = {"GIT_CONFIG_COUNT": "1", **entry}
    before = environment.copy()

    with pytest.raises(ValueError) as caught:
        native_plugin_environment(environment, windows=True)

    assert str(caught.value) == message
    assert "secret" not in str(caught.value)
    assert environment == before


def _git(arguments: list[str], *, cwd: Path, environment: dict[str, str]) -> str:
    result = subprocess.run(
        ["git", *arguments],
        cwd=cwd,
        env=environment,
        check=True,
        capture_output=True,
        text=True,
        timeout=30,
    )
    return result.stdout.strip()


@pytest.mark.skipif(sys.platform != "win32", reason="Windows Git long-path behavior")
def test_git_reads_process_config_and_clones_long_paths(
    tmp_path: Path,
) -> None:
    clean_git_environment = {
        name: value
        for name, value in os.environ.items()
        if name != "GIT_CONFIG_COUNT"
        and not name.startswith("GIT_CONFIG_KEY_")
        and not name.startswith("GIT_CONFIG_VALUE_")
    }
    (tmp_path / "empty-global-config").write_text("", encoding="utf-8")
    source = tmp_path / "source.git"
    _git(
        ["init", "--bare", "--initial-branch=main", str(source)],
        cwd=tmp_path,
        environment=clean_git_environment,
    )

    long_path = "/".join(["directory-" + "x" * 38] * 5 + ["file-" + "y" * 32 + ".txt"])
    assert len(long_path) > 260
    source_environment = {
        **clean_git_environment,
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_CONFIG_GLOBAL": str(tmp_path / "empty-global-config"),
        "GIT_CONFIG_COUNT": "1",
        "GIT_CONFIG_KEY_0": "core.longpaths",
        "GIT_CONFIG_VALUE_0": "false",
        "GIT_AUTHOR_NAME": "Test",
        "GIT_AUTHOR_EMAIL": "test@example.invalid",
        "GIT_COMMITTER_NAME": "Test",
        "GIT_COMMITTER_EMAIL": "test@example.invalid",
    }
    blob = (
        subprocess.run(
            ["git", "--git-dir", str(source), "hash-object", "-w", "--stdin"],
            input=b"long path payload",
            capture_output=True,
            check=True,
            timeout=30,
        )
        .stdout.decode("ascii")
        .strip()
    )
    _git(
        [
            "--git-dir",
            str(source),
            "update-index",
            "--add",
            "--cacheinfo",
            f"100644,{blob},{long_path}",
        ],
        cwd=tmp_path,
        environment=source_environment,
    )
    tree = _git(
        ["--git-dir", str(source), "write-tree"],
        cwd=tmp_path,
        environment=source_environment,
    )
    commit = _git(
        ["--git-dir", str(source), "commit-tree", tree, "-m", "long path fixture"],
        cwd=tmp_path,
        environment=source_environment,
    )
    _git(
        ["--git-dir", str(source), "update-ref", "refs/heads/main", commit],
        cwd=tmp_path,
        environment=source_environment,
    )

    without_helper = tmp_path / "without-helper"
    failed = subprocess.run(
        ["git", "clone", str(source), str(without_helper)],
        env=source_environment,
        capture_output=True,
        text=True,
        timeout=30,
        check=False,
    )
    assert failed.returncode != 0
    shutil.rmtree(without_helper, ignore_errors=True)

    with_helper = tmp_path / "with-helper"
    updated_environment = native_plugin_environment(source_environment, windows=True)
    assert (
        _git(
            ["--git-dir", str(source), "config", "--get", "core.longpaths"],
            cwd=tmp_path,
            environment=updated_environment,
        )
        == "true"
    )
    cloned = subprocess.run(
        ["git", "clone", str(source), str(with_helper)],
        env=updated_environment,
        capture_output=True,
        text=True,
        timeout=30,
        check=False,
    )
    assert cloned.returncode == 0, cloned.stderr
    assert (with_helper / Path(*long_path.split("/"))).exists()

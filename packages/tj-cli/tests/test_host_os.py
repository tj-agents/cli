import platform
import sysconfig

import pytest

from tj_cli import host_os

# The interpreter's build tag ("win-amd64", "linux-x86_64", "macosx-14.0-arm64") names
# the same system and processor without asking WMI.
BUILD = sysconfig.get_platform().lower()
SYSTEMS = {"win": "Windows", "linux": "Linux", "macosx": "Darwin"}


@pytest.fixture(autouse=True)
def platform_probes_fail(monkeypatch: pytest.MonkeyPatch) -> None:
    def hung() -> object:
        raise AssertionError("platform probe would query WMI")

    for probe in ("uname", "system", "machine", "win32_ver", "_wmi_query"):
        if hasattr(platform, probe):
            monkeypatch.setattr(platform, probe, hung)


def test_host_os_system_names_this_operating_system() -> None:
    assert host_os.system() == SYSTEMS[BUILD.partition("-")[0]]


def test_host_os_machine_names_this_processor() -> None:
    assert host_os.machine().lower() == BUILD.rpartition("-")[2]

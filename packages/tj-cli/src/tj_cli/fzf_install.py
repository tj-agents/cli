from __future__ import annotations

import argparse
import hashlib
import io
import platform
import sys
import tarfile
import urllib.request
import zipfile
from pathlib import Path

from tj_cli import tool_receipts
from tj_cli.native_install import default_bin_dir

VERSION = "0.75.0"
RECEIPT = ".tj-fzf.json"
ARCHIVES = {
    ("Windows", "amd64"): (
        "fzf-0.75.0-windows_amd64.zip",
        "e3fb809d48094bea7f9f4df03b2fea33ae6e28a6cd32fdad9970cd79e1103901",
    ),
    ("Windows", "arm64"): (
        "fzf-0.75.0-windows_arm64.zip",
        "56c9465827208bdfb7b8dfee56fdd6a7e5932ff0341a659350a908da885ebdca",
    ),
    ("Linux", "amd64"): (
        "fzf-0.75.0-linux_amd64.tar.gz",
        "0d6640f83582d991cf429754baad64b07e3a4e439a6019670f4b91da1314141c",
    ),
    ("Linux", "arm64"): (
        "fzf-0.75.0-linux_arm64.tar.gz",
        "bd0717badb6a5620387e014bb94209aac4d9eab6ab77b5348eabb0833aeb4d59",
    ),
}


class FzfInstallError(RuntimeError):
    pass


def executable(bin_dir: Path) -> Path:
    return bin_dir / ("fzf.exe" if platform.system() == "Windows" else "fzf")


def pending(bin_dir: Path) -> str | None:
    return tool_receipts.pending(
        bin_dir / RECEIPT, VERSION, names=(executable(bin_dir).name,)
    )


def binary(archive_name: str, contents: bytes) -> bytes:
    if archive_name.endswith(".zip"):
        with zipfile.ZipFile(io.BytesIO(contents)) as archive:
            return archive.read("fzf.exe")
    with tarfile.open(fileobj=io.BytesIO(contents), mode="r:gz") as archive:
        extracted = archive.extractfile("fzf")
        if extracted is None:
            raise FzfInstallError("the fzf archive has no executable")
        with extracted:
            return extracted.read()


def download(archive_name: str, digest: str) -> bytes:
    url = f"https://github.com/junegunn/fzf/releases/download/v{VERSION}/{archive_name}"
    with urllib.request.urlopen(url, timeout=600) as response:
        contents: bytes = response.read()
    if hashlib.sha256(contents).hexdigest() != digest:
        raise FzfInstallError(f"{archive_name} failed SHA-256 verification")
    return contents


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=f"Install fzf {VERSION}")
    parser.add_argument("--bin-dir", type=Path, default=default_bin_dir())
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--check", action="store_true")
    mode.add_argument("--uninstall", action="store_true")
    arguments = parser.parse_args(argv)
    target = executable(arguments.bin_dir)
    try:
        if arguments.uninstall:
            (arguments.bin_dir / RECEIPT).unlink(missing_ok=True)
            if target.exists():
                target.unlink()
                print(f"removed    {target}")
            else:
                print("ok         fzf (not installed)")
            return 0
        if pending(arguments.bin_dir) is None:
            print(f"ok         fzf {VERSION}")
            return 0
        if arguments.check:
            print(f"missing    fzf {VERSION}")
            return 1
        system, machine = platform.system(), platform.machine().lower()
        architecture = {"x86_64": "amd64", "aarch64": "arm64"}.get(machine, machine)
        if (system, architecture) not in ARCHIVES:
            raise FzfInstallError(f"no pinned fzf build for {system} {machine}")
        archive_name, digest = ARCHIVES[system, architecture]
        contents = binary(archive_name, download(archive_name, digest))
        arguments.bin_dir.mkdir(parents=True, exist_ok=True)
        staged = target.with_name(target.name + ".new")
        staged.write_bytes(contents)
        staged.chmod(0o755)
        staged.replace(target)
        tool_receipts.write(
            arguments.bin_dir / RECEIPT,
            tool_receipts.Receipt(VERSION, {target.name: tool_receipts.sha256(target)}),
        )
    except (
        FzfInstallError,
        OSError,
        KeyError,
        tarfile.TarError,
        zipfile.BadZipFile,
    ) as error:
        print(f"error      fzf: {error}", file=sys.stderr)
        return 1
    print(f"installed  fzf {VERSION} ({target})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

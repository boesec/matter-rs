"""Thin helpers around the Docker command line.

The harness never talks to the Docker API; it builds ``docker run`` command lines so that a log
shows exactly what ran, and so that a scenario can be replayed by hand.
"""

from __future__ import annotations

import shutil
import subprocess
from pathlib import Path


def docker_available() -> bool:
    """True when the ``docker`` client exists and a daemon answers."""
    if shutil.which("docker") is None:
        return False
    try:
        result = subprocess.run(
            ["docker", "version", "--format", "{{.Server.Version}}"],
            capture_output=True,
            text=True,
            timeout=30,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return False
    return result.returncode == 0 and bool(result.stdout.strip())


def image_present(image: str) -> bool:
    result = subprocess.run(
        ["docker", "image", "inspect", image],
        capture_output=True,
        text=True,
        timeout=60,
        check=False,
    )
    return result.returncode == 0


def run_command(
    image: str,
    args: tuple[str, ...] | list[str],
    *,
    name: str,
    host_network: bool = True,
    interactive: bool = False,
    mounts: tuple[tuple[str, str, bool], ...] = (),
    env: dict[str, str] | None = None,
    workdir: str | None = None,
    entrypoint: str | None = None,
    security_opts: tuple[str, ...] = (),
) -> tuple[str, ...]:
    """Build a ``docker run --rm`` command line.

    ``mounts`` are ``(source, target, read_only)`` triples; a source that is not an absolute path
    is a named volume. ``security_opts`` are passed as ``--security-opt`` values.
    """
    command: list[str] = ["docker", "run", "--rm", "--name", name]
    if host_network:
        command += ["--network", "host"]
    if interactive:
        command.append("-i")
    for option in security_opts:
        command += ["--security-opt", option]
    for source, target, read_only in mounts:
        spec = f"{source}:{target}"
        if read_only:
            spec += ":ro"
        command += ["-v", spec]
    for key, value in (env or {}).items():
        command += ["-e", f"{key}={value}"]
    if workdir:
        command += ["-w", workdir]
    if entrypoint:
        command += ["--entrypoint", entrypoint]
    command.append(image)
    command.extend(args)
    return tuple(command)


# Containers that talk to the host's Avahi daemon over the mounted D-Bus socket. Docker's default
# AppArmor profile has no D-Bus rules, and an AppArmor-aware dbus-daemon refuses a confined
# client's connection outright, which chip-tool reports as "Open file failed" from its mDNS
# initialisation. Lifting the profile for these containers lets the connection through; they
# still run without extra capabilities.
DBUS_CLIENT_SECURITY_OPTS: tuple[str, ...] = ("apparmor=unconfined",)


def remove_container(name: str) -> None:
    subprocess.run(
        ["docker", "rm", "-f", name],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        check=False,
        timeout=60,
    )


def remove_volume(name: str) -> None:
    subprocess.run(
        ["docker", "volume", "rm", "-f", name],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        check=False,
        timeout=60,
    )


def host_path(path: Path) -> str:
    """A path as Docker wants it in ``-v``: absolute, forward slashes."""
    return str(path.resolve()).replace("\\", "/")

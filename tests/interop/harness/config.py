"""Configuration of a harness run: pinned images, commissioning parameters, the device under test.

Everything comes from ``containers/images.toml`` and environment variables with defaults, so a
local run and the workflow use the same code path. A device under test is a command line plus a
ready pattern (:class:`DutSpec`). Two are built in: the matter.js on/off light from
``containers/matterjs/`` (the default, ``INTEROP_DUT_COMMAND`` swaps it for any other program)
and the engine example ``onoff_node`` of this repository, a native binary named by
``INTEROP_ENGINE_DUT`` that runs on its own port and discriminator next to the first.
"""

from __future__ import annotations

import os
import shlex
import tomllib
import uuid
from dataclasses import dataclass
from pathlib import Path

INTEROP_ROOT = Path(__file__).resolve().parent.parent
IMAGES_FILE = INTEROP_ROOT / "containers" / "images.toml"

DEFAULT_PASSCODE = 20202021
DEFAULT_DISCRIMINATOR = 3840
DEFAULT_VENDOR_ID = 0xFFF1
DEFAULT_PRODUCT_ID = 0x8000
DEFAULT_PORT = 5540
DEFAULT_DBUS_SOCKET = "/var/run/dbus/system_bus_socket"
DEFAULT_MATTERJS_IMAGE = "matter-rs-interop-matterjs:local"
DEFAULT_READY_TIMEOUT = 90.0
# The engine DUT runs next to the matter.js DUT: another port and another discriminator, so
# that both can be commissionable at the same time and chip-tool tells them apart.
DEFAULT_ENGINE_PORT = 5541
DEFAULT_ENGINE_DISCRIMINATOR = 3841
ENGINE_READY_PATTERN = r"onoff_node: ready"
ENGINE_ENDPOINT = 2


def _int(value: str | None, default: int | None) -> int | None:
    """An integer from the environment (decimal or 0x-prefixed hex), or the default when unset."""
    if value is None or value == "":
        return default
    return int(value, 0)


@dataclass(frozen=True)
class ImageRef:
    """One pinned image from ``images.toml``."""

    name: str
    tag: str
    digest: str
    platform: str
    commit: str | None = None

    @property
    def pinned(self) -> str:
        """The reference the harness pulls and runs: name plus digest."""
        return f"{self.name}@{self.digest}"

    @property
    def tagged(self) -> str:
        return f"{self.name}:{self.tag}"


def load_images(path: Path = IMAGES_FILE) -> dict[str, ImageRef]:
    with path.open("rb") as handle:
        data = tomllib.load(handle)
    images: dict[str, ImageRef] = {}
    for key, entry in data.items():
        images[key] = ImageRef(
            name=entry["name"],
            tag=entry["tag"],
            digest=entry["digest"],
            platform=entry["platform"],
            commit=entry.get("commit"),
        )
    return images


@dataclass(frozen=True)
class DutSpec:
    """The device under test: how to start it and how to tell it is ready."""

    name: str
    command: tuple[str, ...]
    ready_pattern: str
    ready_timeout: float
    endpoint: int
    container_name: str | None = None
    # True when the device takes commands on its standard input (the engine DUT does).
    stdin: bool = False


@dataclass(frozen=True)
class HarnessConfig:
    run_id: str
    logs_dir: Path
    passcode: int
    discriminator: int
    vendor_id: int
    product_id: int
    dut_port: int
    chip_image: str
    matterjs_image: str
    dbus_socket: str
    require_docker: bool
    ready_timeout: float
    dut_command: tuple[str, ...] | None
    dut_ready_pattern: str | None
    dut_endpoint: int | None
    engine_dut_binary: Path | None
    engine_interface: str | None
    engine_port: int
    engine_discriminator: int

    @classmethod
    def from_env(cls, env: dict[str, str] | None = None) -> HarnessConfig:
        env = dict(os.environ if env is None else env)
        images = load_images()
        run_id = env.get("INTEROP_RUN_ID") or uuid.uuid4().hex[:8]
        dut_command = env.get("INTEROP_DUT_COMMAND")
        return cls(
            run_id=run_id,
            logs_dir=Path(env.get("INTEROP_LOGS_DIR") or INTEROP_ROOT / "logs").resolve(),
            passcode=_int(env.get("INTEROP_PASSCODE"), DEFAULT_PASSCODE),
            discriminator=_int(env.get("INTEROP_DISCRIMINATOR"), DEFAULT_DISCRIMINATOR),
            vendor_id=_int(env.get("INTEROP_VENDOR_ID"), DEFAULT_VENDOR_ID),
            product_id=_int(env.get("INTEROP_PRODUCT_ID"), DEFAULT_PRODUCT_ID),
            dut_port=_int(env.get("INTEROP_DUT_PORT"), DEFAULT_PORT),
            chip_image=env.get("INTEROP_CHIP_IMAGE") or images["chip"].pinned,
            matterjs_image=env.get("INTEROP_MATTERJS_IMAGE") or DEFAULT_MATTERJS_IMAGE,
            dbus_socket=env.get("INTEROP_DBUS_SOCKET") or DEFAULT_DBUS_SOCKET,
            require_docker=env.get("INTEROP_REQUIRE_DOCKER", "") not in ("", "0", "false"),
            ready_timeout=float(env.get("INTEROP_READY_TIMEOUT") or DEFAULT_READY_TIMEOUT),
            dut_command=tuple(shlex.split(dut_command)) if dut_command else None,
            dut_ready_pattern=env.get("INTEROP_DUT_READY_PATTERN") or None,
            dut_endpoint=_int(env.get("INTEROP_DUT_ENDPOINT"), None),
            engine_dut_binary=Path(env["INTEROP_ENGINE_DUT"]).resolve() if env.get("INTEROP_ENGINE_DUT") else None,
            engine_interface=env.get("INTEROP_ENGINE_INTERFACE") or None,
            engine_port=_int(env.get("INTEROP_ENGINE_DUT_PORT"), DEFAULT_ENGINE_PORT),
            engine_discriminator=_int(env.get("INTEROP_ENGINE_DISCRIMINATOR"), DEFAULT_ENGINE_DISCRIMINATOR),
        )

    def container_name(self, component: str) -> str:
        return f"interop-{self.run_id}-{component}"

    def volume_name(self, component: str) -> str:
        return f"interop-{self.run_id}-{component}"

    def matterjs_dut(self) -> DutSpec:
        """The default device under test: the matter.js on/off light in its container."""
        name = self.container_name("dut")
        command = (
            "docker",
            "run",
            "--rm",
            "--network",
            "host",
            "--name",
            name,
            "-v",
            f"{self.volume_name('dut')}:/state",
            self.matterjs_image,
            "node",
            "device.mjs",
            f"--passcode={self.passcode}",
            f"--discriminator={self.discriminator}",
            f"--port={self.dut_port}",
            f"--vendorid={self.vendor_id}",
            f"--productid={self.product_id}",
            "--storage-path=/state",
            "--log-format=plain",
        )
        return DutSpec(
            name="matterjs-onoff-light",
            command=command,
            ready_pattern=r"interop-dut: node is online",
            ready_timeout=self.ready_timeout,
            endpoint=1,
            container_name=name,
        )

    def engine_dut(self, state_dir: Path) -> DutSpec:
        """The engine example ``onoff_node`` as a native process on ``state_dir``.

        It keeps its initial light at endpoint 2 and takes the line protocol on standard input.
        ``INTEROP_ENGINE_DUT`` names the built binary; without it there is no engine DUT.
        """
        if self.engine_dut_binary is None:
            raise ValueError("INTEROP_ENGINE_DUT names the built onoff_node binary; it is not set")
        command: list[str] = [
            str(self.engine_dut_binary),
            "--state-dir",
            str(state_dir),
            "--port",
            str(self.engine_port),
            "--discriminator",
            str(self.engine_discriminator),
            "--passcode",
            str(self.passcode),
        ]
        if self.engine_interface:
            command += ["--interface", self.engine_interface]
        return DutSpec(
            name="engine-onoff-node",
            command=tuple(command),
            ready_pattern=ENGINE_READY_PATTERN,
            ready_timeout=self.ready_timeout,
            endpoint=ENGINE_ENDPOINT,
            stdin=True,
        )

    def dut(self) -> DutSpec:
        """The configured device under test: the environment override or the matter.js device."""
        if self.dut_command is None:
            return self.matterjs_dut()
        if self.dut_ready_pattern is None:
            raise ValueError("INTEROP_DUT_COMMAND needs INTEROP_DUT_READY_PATTERN")
        return DutSpec(
            name="custom",
            command=self.dut_command,
            ready_pattern=self.dut_ready_pattern,
            ready_timeout=self.ready_timeout,
            endpoint=self.dut_endpoint if self.dut_endpoint is not None else 1,
        )

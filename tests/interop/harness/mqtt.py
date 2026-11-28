"""An Eclipse Mosquitto broker in a container, and the service configured with an MQTT adapter.

The broker runs with host networking on its own port (1884, so it never clashes with a broker of
the machine), anonymous, without persistence, in a container that is *not* removed on stop: the
restart scenario stops and starts it again under the same name. Publishing and subscribing go
through the image's own ``mosquitto_pub`` and ``mosquitto_sub`` (``docker exec`` into the running
broker container, or a throw-away container for a long-lived subscriber), so no Python MQTT
client is needed.

:class:`MqttServiceDut` is the service DUT with an ``mqtt`` adapter and two devices appended to
its configuration: a dimmable light ``lamp`` (endpoint 2) whose level is a percentage, and a
temperature sensor ``thermometer`` (endpoint 3) whose source publishes degrees.
"""

from __future__ import annotations

import json
import logging
import os
import subprocess
import tempfile
import time
from pathlib import Path

from harness.config import HarnessConfig, load_images
from harness.docker import image_present, remove_container
from harness.process import HarnessError, ManagedProcess
from harness.redact import Redactor
from harness.servicedut import ServiceDut

DEFAULT_MQTT_PORT = 1884
# The MQTT service runs next to the plain service DUT (5542/3842).
DEFAULT_MQTT_SERVICE_PORT = 5543
DEFAULT_MQTT_SERVICE_DISCRIMINATOR = 3843
# The source adapter the scenarios play, and the bridge's instance name on the broker.
SOURCE = "source"
INSTANCE = "matter"
LAMP_ENDPOINT = 2
THERMOMETER_ENDPOINT = 3
PULL_TIMEOUT = 600.0
READY_TIMEOUT = 60.0
# The adapter's ready line in the service log.
ADAPTER_CONNECTED = r"connected to mqtt://127\.0\.0\.1:{port}"
ADAPTER_LOST = r"connection to mqtt://127\.0\.0\.1:{port} failed"


def mqtt_port(env: dict[str, str] | None = None) -> int:
    env = dict(os.environ if env is None else env)
    return int(env.get("INTEROP_MQTT_PORT") or str(DEFAULT_MQTT_PORT), 0)


def mqtt_service_port(env: dict[str, str] | None = None) -> int:
    env = dict(os.environ if env is None else env)
    return int(env.get("INTEROP_MQTT_SERVICE_DUT_PORT") or str(DEFAULT_MQTT_SERVICE_PORT), 0)


def mqtt_service_discriminator(env: dict[str, str] | None = None) -> int:
    env = dict(os.environ if env is None else env)
    return int(env.get("INTEROP_MQTT_DISCRIMINATOR") or str(DEFAULT_MQTT_SERVICE_DISCRIMINATOR), 0)


def mosquitto_image() -> str:
    """The pinned Mosquitto image reference (name plus digest)."""
    return load_images()["mosquitto"].pinned


def ensure_image(image: str, logger: logging.Logger) -> None:
    """Pulls ``image`` when it is not present; raises :class:`HarnessError` when that fails."""
    if image_present(image):
        return
    logger.info("pulling %s", image)
    result = subprocess.run(
        ["docker", "pull", "--quiet", image],
        capture_output=True,
        text=True,
        timeout=PULL_TIMEOUT,
        check=False,
    )
    if result.returncode != 0:
        raise HarnessError(f"docker pull {image} failed:\n{result.stdout}\n{result.stderr}")


def mqtt_sections(port: int) -> str:
    """The adapter and device tables appended to the service configuration."""
    return "\n".join(
        [
            "[adapters.home]",
            'kind = "mqtt"',
            f'url = "mqtt://127.0.0.1:{port}"',
            f'name = "{INSTANCE}"',
            "reconnect = { initial = 0.5, max = 2 }",
            "",
            "[devices.lamp]",
            'kind = "dimmable-light"',
            'adapter = "home"',
            'name = "Interop lamp"',
            f'mapping = {{ source = "{SOURCE}/lamp", items = {{ level = {{ scale = [0, 100] }} }} }}',
            "",
            "[devices.thermometer]",
            'kind = "temperature-sensor"',
            'adapter = "home"',
            f'mapping = {{ source = "{SOURCE}/thermometer", items = {{ temperature = {{ factor = 100 }} }} }}',
            "",
        ]
    )


def percent_to_level(percent: int) -> int:
    """The Matter level (1 to 254) the adapter derives from a percentage under ``scale = [0, 100]``."""
    return round(1 + percent * 253 / 100)


class MosquittoBroker:
    """Mosquitto in a container with host networking, anonymous, on ``port``."""

    def __init__(
        self,
        config: HarnessConfig,
        logs_dir: Path,
        redactor: Redactor,
        logger: logging.Logger,
        *,
        port: int = DEFAULT_MQTT_PORT,
    ) -> None:
        self.port = port
        self.image = mosquitto_image()
        self.container = config.container_name("mosquitto")
        self._config = config
        self._logs_dir = logs_dir
        self._redactor = redactor
        self._logger = logger
        self._counter = 0
        self._work_dir = Path(tempfile.mkdtemp(prefix="interop-mosquitto-"))
        self._conf = self._work_dir / "mosquitto.conf"
        self._conf.write_text(f"listener {port}\nallow_anonymous true\npersistence false\n", encoding="utf-8")
        self._log: ManagedProcess | None = None

    # -- lifecycle -------------------------------------------------------------------------

    def start(self) -> MosquittoBroker:
        """Pulls the image if needed, creates and starts the container and waits for the port."""
        ensure_image(self.image, self._logger)
        remove_container(self.container)
        command = [
            "docker",
            "run",
            "-d",
            "--name",
            self.container,
            "--network",
            "host",
            "-v",
            f"{self._conf.resolve().as_posix()}:/mosquitto/config/mosquitto.conf:ro",
            self.image,
        ]
        self._logger.info("mosquitto: starting %s", " ".join(command))
        result = subprocess.run(command, capture_output=True, text=True, timeout=120, check=False)
        if result.returncode != 0:
            raise HarnessError(f"docker run of Mosquitto failed:\n{result.stdout}\n{result.stderr}")
        self._follow_log()
        self.wait_ready()
        return self

    def _follow_log(self) -> None:
        """Collects the broker's log under ``logs/`` like every other process."""
        self._counter += 1
        self._log = ManagedProcess(
            "mosquitto log",
            ("docker", "logs", "-f", self.container),
            self._logs_dir / f"mosquitto-{self._counter}.log",
            self._redactor,
            self._logger,
        ).start()

    def wait_ready(self, timeout: float = READY_TIMEOUT) -> None:
        """Waits until a publish through the broker succeeds."""
        deadline = time.monotonic() + timeout
        while True:
            result = self._exec(["mosquitto_pub", "-p", str(self.port), "-t", "interop/probe", "-m", "1"], timeout=15)
            if result.returncode == 0:
                return
            if time.monotonic() > deadline:
                raise HarnessError(f"Mosquitto does not accept connections on port {self.port}:\n{result.stderr}")
            time.sleep(0.5)

    def stop_container(self) -> None:
        """``docker stop``: every client loses its connection; the container stays."""
        self._logger.info("mosquitto: stopping the container")
        subprocess.run(["docker", "stop", self.container], capture_output=True, text=True, timeout=60, check=True)
        if self._log is not None:
            self._log.stop()
            self._log = None

    def start_container(self) -> None:
        """``docker start`` after :meth:`stop_container`; waits for the port again."""
        self._logger.info("mosquitto: starting the container again")
        subprocess.run(["docker", "start", self.container], capture_output=True, text=True, timeout=60, check=True)
        self._follow_log()
        self.wait_ready()

    def remove(self) -> None:
        if self._log is not None:
            self._log.stop()
            self._log = None
        remove_container(self.container)

    # -- messages ---------------------------------------------------------------------------

    def _exec(self, args: list[str], *, timeout: float) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["docker", "exec", self.container, *args],
            capture_output=True,
            text=True,
            timeout=timeout,
            check=False,
        )

    def publish(self, topic: str, payload: str, *, retain: bool = False) -> None:
        """Publishes one message through the broker's own ``mosquitto_pub``."""
        args = ["mosquitto_pub", "-p", str(self.port), "-t", topic, "-m", payload]
        if retain:
            args.append("-r")
        self._logger.info("mosquitto: publish %s %s%s", topic, payload, " (retained)" if retain else "")
        result = self._exec(args, timeout=30)
        if result.returncode != 0:
            raise HarnessError(f"mosquitto_pub {topic} failed: {result.stderr}")

    def read_retained(self, topic: str, timeout: float = 5.0) -> str | None:
        """The retained payload of ``topic``, or ``None`` when the broker holds none."""
        result = self._exec(
            ["mosquitto_sub", "-p", str(self.port), "-t", topic, "-C", "1", "-W", str(int(timeout)), "-F", "%p"],
            timeout=timeout + 15,
        )
        if result.returncode == 27:  # mosquitto_sub's exit code for the -W timeout
            return None
        if result.returncode != 0:
            raise HarnessError(f"mosquitto_sub {topic} failed: {result.stderr}")
        return result.stdout.rstrip("\n")

    def read_retained_val(self, topic: str, timeout: float = 5.0) -> object:
        """The ``val`` of a retained status object, or the plain value when it is not JSON."""
        payload = self.read_retained(topic, timeout)
        if payload is None:
            return None
        try:
            parsed = json.loads(payload)
        except json.JSONDecodeError:
            return payload
        return parsed.get("val", parsed) if isinstance(parsed, dict) else parsed

    def collect(self, topic_filter: str, seconds: float) -> list[tuple[str, str]]:
        """Every message on ``topic_filter`` within ``seconds`` (retained ones first), as pairs."""
        result = self._exec(
            ["mosquitto_sub", "-p", str(self.port), "-t", topic_filter, "-W", str(int(seconds)), "-F", "%t\t%p"],
            timeout=seconds + 15,
        )
        if result.returncode not in (0, 27):
            raise HarnessError(f"mosquitto_sub {topic_filter} failed: {result.stderr}")
        pairs: list[tuple[str, str]] = []
        for line in result.stdout.splitlines():
            topic, _, payload = line.partition("\t")
            pairs.append((topic, payload))
        return pairs

    def subscribe(self, topic_filter: str, label: str) -> ManagedProcess:
        """A long-lived subscriber in its own container; its lines are ``<topic> <payload>``."""
        self._counter += 1
        name = self._config.container_name(f"mosquitto-sub-{self._counter}")
        command = (
            "docker",
            "run",
            "--rm",
            "--network",
            "host",
            "--name",
            name,
            self.image,
            "mosquitto_sub",
            "-h",
            "127.0.0.1",
            "-p",
            str(self.port),
            "-t",
            topic_filter,
            "-v",
        )
        return ManagedProcess(
            f"mosquitto_sub {label}",
            command,
            self._logs_dir / f"mosquitto-sub-{self._counter:02d}-{label}.log",
            self._redactor,
            self._logger,
            container_name=name,
        ).start()


class MqttServiceDut(ServiceDut):
    """The service DUT with an MQTT adapter and two devices on the harness broker."""

    def __init__(
        self,
        binary: Path,
        config: HarnessConfig,
        work_dir: Path,
        logs_dir: Path,
        redactor: Redactor,
        logger: logging.Logger,
        *,
        broker_port: int,
        port: int = DEFAULT_MQTT_SERVICE_PORT,
        discriminator: int = DEFAULT_MQTT_SERVICE_DISCRIMINATOR,
    ) -> None:
        super().__init__(binary, config, work_dir, logs_dir, redactor, logger, port=port, discriminator=discriminator)
        self.broker_port = broker_port
        text = self.config_path.read_text(encoding="utf-8") + "\n" + mqtt_sections(broker_port)
        self.config_path.write_text(text, encoding="utf-8")

    def start(self) -> MqttServiceDut:
        """Starts the service and waits for the node and for the adapter's connection."""
        super().start()
        self.running().wait_for(ADAPTER_CONNECTED.format(port=self.broker_port), READY_TIMEOUT)
        return self

"""Fixtures of the interop harness.

Session fixtures start the device under test once and let chip-tool commission it; the scenarios
then act as further administrators on the same device. Everything the harness writes goes
through one redactor, so no passcode or pairing code reaches ``logs/``.
"""

from __future__ import annotations

import logging
import re
import shutil
import sys
from dataclasses import dataclass
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent))

from harness.chiptool import ChipTool
from harness.config import DutSpec, HarnessConfig
from harness.docker import docker_available, image_present, remove_volume
from harness.pairing import manual_pairing_code
from harness.process import HarnessError, ManagedProcess
from harness.redact import Redactor

CHIPTOOL_NODE_ID = 1
LAST_TEST = "test_logs_contain_no_passcode"


def pytest_collection_modifyitems(session: pytest.Session, config: pytest.Config, items: list[pytest.Item]) -> None:
    """The log check runs after every other scenario, whatever the collection order was."""
    last = [item for item in items if item.name.startswith(LAST_TEST)]
    for item in last:
        items.remove(item)
    items.extend(last)


class RedactingFilter(logging.Filter):
    """Rewrites every record of the harness logger before any handler sees it."""

    def __init__(self, redactor: Redactor) -> None:
        super().__init__()
        self._redactor = redactor

    def filter(self, record: logging.LogRecord) -> bool:
        record.msg = self._redactor.redact(record.getMessage())
        record.args = ()
        return True


@dataclass
class DutHandle:
    spec: DutSpec
    process: ManagedProcess


@dataclass
class CommissionedDut:
    dut: DutHandle
    node_id: int

    @property
    def endpoint(self) -> int:
        return self.dut.spec.endpoint


def start_dut(spec: DutSpec, logs_dir: Path, redactor: Redactor, logger: logging.Logger) -> ManagedProcess:
    """Start the device under test and wait for its ready pattern; stop it again on failure."""
    safe_name = re.sub(r"[^A-Za-z0-9_.-]", "_", spec.name)
    process = ManagedProcess(
        f"dut {spec.name}",
        spec.command,
        logs_dir / f"{safe_name}.log",
        redactor,
        logger,
        container_name=spec.container_name,
    ).start()
    try:
        process.wait_for(spec.ready_pattern, spec.ready_timeout)
    except HarnessError:
        process.stop()
        raise
    return process


@pytest.fixture(scope="session")
def config() -> HarnessConfig:
    cfg = HarnessConfig.from_env()
    if cfg.logs_dir.exists():
        shutil.rmtree(cfg.logs_dir, ignore_errors=True)
    cfg.logs_dir.mkdir(parents=True, exist_ok=True)
    return cfg


@pytest.fixture(scope="session")
def redactor(config: HarnessConfig) -> Redactor:
    return Redactor([str(config.passcode), manual_pairing_code(config.passcode, config.discriminator)])


@pytest.fixture(scope="session")
def harness_logger(config: HarnessConfig, redactor: Redactor):
    logger = logging.getLogger("interop")
    logger.setLevel(logging.INFO)
    redacting = RedactingFilter(redactor)
    logger.addFilter(redacting)
    handler = logging.FileHandler(config.logs_dir / "harness.log", encoding="utf-8")
    handler.setFormatter(logging.Formatter("%(asctime)s %(levelname)s %(message)s"))
    logger.addHandler(handler)
    logger.info("harness run %s, logs in %s", config.run_id, config.logs_dir)
    yield logger
    logger.removeHandler(handler)
    logger.removeFilter(redacting)
    handler.close()


@pytest.fixture(scope="session")
def docker(config: HarnessConfig) -> bool:
    """Skips (or fails, when the run requires Docker) the scenarios that need containers."""
    if docker_available():
        missing = [image for image in (config.chip_image, config.matterjs_image) if not image_present(image)]
        if missing:
            message = "images not present locally, pull or build them first (see README): " + ", ".join(missing)
            if config.require_docker:
                pytest.fail(message)
            pytest.skip(message)
        return True
    if config.require_docker:
        pytest.fail("Docker is required for this run (INTEROP_REQUIRE_DOCKER is set) but no daemon answers")
    pytest.skip("Docker is not available; set INTEROP_REQUIRE_DOCKER=1 to make this a failure")


@pytest.fixture(scope="session")
def dut(config: HarnessConfig, redactor: Redactor, harness_logger: logging.Logger, docker: bool):
    spec = config.dut()
    process = start_dut(spec, config.logs_dir / "dut", redactor, harness_logger)
    yield DutHandle(spec, process)
    process.stop()
    remove_volume(config.volume_name("dut"))


@pytest.fixture(scope="session")
def chiptool(config: HarnessConfig, redactor: Redactor, harness_logger: logging.Logger, docker: bool):
    tool = ChipTool(config, redactor, harness_logger, config.logs_dir / "chip-tool")
    yield tool
    remove_volume(tool.volume)


@pytest.fixture(scope="session")
def commissioned_dut(dut: DutHandle, chiptool: ChipTool) -> CommissionedDut:
    """The device under test on chip-tool's fabric; every scenario builds on this first admin."""
    chiptool.pair(CHIPTOOL_NODE_ID)
    return CommissionedDut(dut, CHIPTOOL_NODE_ID)

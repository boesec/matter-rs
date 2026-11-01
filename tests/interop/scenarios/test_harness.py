"""Scenarios about the harness itself: it must fail readably and must not leak secrets."""

from __future__ import annotations

import logging
import subprocess
import time

import pytest

from conftest import start_dut
from harness.config import DutSpec, HarnessConfig
from harness.docker import run_command
from harness.pairing import manual_pairing_code
from harness.process import HarnessTimeout
from harness.redact import MASK, Redactor

BROKEN_DEVICE = (
    "console.log('broken device: starting up');"
    "console.log('broken device: listening on nothing');"
    "setInterval(() => {}, 1000);"
)
READY_TIMEOUT = 10.0


@pytest.mark.docker
@pytest.mark.timeout(120)
def test_harness_detects_failure(
    config: HarnessConfig,
    redactor: Redactor,
    harness_logger: logging.Logger,
    docker: bool,
) -> None:
    """A device that never becomes ready fails within its timeout, with its last lines."""
    name = config.container_name("broken-dut")
    spec = DutSpec(
        name="broken-device",
        command=run_command(config.matterjs_image, ("node", "-e", BROKEN_DEVICE), name=name),
        ready_pattern=r"interop-dut: node is online",
        ready_timeout=READY_TIMEOUT,
        endpoint=1,
        container_name=name,
    )

    started = time.monotonic()
    with pytest.raises(HarnessTimeout) as failure:
        start_dut(spec, config.logs_dir / "broken-dut", redactor, harness_logger)
    elapsed = time.monotonic() - started

    message = str(failure.value)
    assert "broken device: listening on nothing" in message, message
    assert f"did not print within {READY_TIMEOUT:.0f} s" in message, message
    assert elapsed < READY_TIMEOUT + 30, f"failure took {elapsed:.1f} s"

    still_there = subprocess.run(
        ["docker", "ps", "-a", "--filter", f"name=^{name}$", "--format", "{{.Names}}"],
        capture_output=True,
        text=True,
        check=False,
    )
    assert still_there.stdout.strip() == "", "the broken device's container was not removed"


def test_logs_contain_no_passcode(config: HarnessConfig, redactor: Redactor) -> None:
    """Nothing under logs/ contains the passcode, a pairing code or an onboarding payload."""
    secrets = set(redactor.secrets)
    secrets.add(str(config.passcode))
    secrets.add(manual_pairing_code(config.passcode, config.discriminator))

    files = [path for path in config.logs_dir.rglob("*") if path.is_file() and not path.is_symlink()]
    assert files, f"no logs were collected under {config.logs_dir}"

    offenders: list[str] = []
    for path in files:
        try:
            text = path.read_text(encoding="utf-8")
        except UnicodeDecodeError:
            continue
        relative = path.relative_to(config.logs_dir)
        for index, secret in enumerate(sorted(secrets)):
            if secret in text:
                offenders.append(f"{relative}: contains secret #{index} ({len(secret)} digits)")
        if "MT:" in text and MASK not in text:
            offenders.append(f"{relative}: contains an onboarding payload")
    assert not offenders, "secrets found in collected logs:\n" + "\n".join(offenders)

    # The matter.js device prints its onboarding codes at start, so when it ran, its log must
    # show that the redactor actually did something.
    dut_log = config.logs_dir / "dut" / "matterjs-onoff-light.log"
    if dut_log.is_file():
        assert MASK in dut_log.read_text(encoding="utf-8"), "the device log shows no redaction at all"

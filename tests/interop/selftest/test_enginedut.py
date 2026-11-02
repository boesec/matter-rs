"""The engine DUT driver against a scripted stand-in for the example: answers, events, restart."""

from __future__ import annotations

import logging
import sys
from pathlib import Path

import pytest

from harness.config import DutSpec
from harness.enginedut import EngineDut
from harness.process import HarnessError, HarnessTimeout
from harness.redact import Redactor

LOGGER = logging.getLogger("interop.selftest")

# A stand-in that speaks the example's protocol: ready line, ok/err answers, events, quit.
FAKE_DUT = """
import sys
print('onoff_node: ready on=false label="Light" commissioned=false window=Some(Basic)', flush=True)
for line in sys.stdin:
    words = line.split()
    if not words:
        continue
    if words[0] == 'add' and words[2] in ('on-off-light', 'contact-sensor'):
        print('ok', flush=True)
        print('event CommissioningChanged commissioned=false window=None', flush=True)
    elif words[0] == 'quit':
        print('ok', flush=True)
        print('event Stopped Requested', flush=True)
        break
    elif words[0] == 'set':
        print('ok', flush=True)
    else:
        print('err unknown command ' + repr(words[0]), flush=True)
"""


def spec(tmp_path: Path) -> DutSpec:
    return DutSpec(
        name="fake-engine-dut",
        command=(sys.executable, "-u", "-c", FAKE_DUT),
        ready_pattern=r"onoff_node: ready",
        ready_timeout=30.0,
        endpoint=2,
        stdin=True,
    )


def test_commands_are_answered_and_events_can_be_awaited(tmp_path: Path) -> None:
    dut = EngineDut(spec(tmp_path), tmp_path, Redactor(), LOGGER).start()
    try:
        since = dut.mark()
        dut.add(5, "contact-sensor")
        event = dut.wait_event(r"^event CommissioningChanged (.*)$", 30.0, since)
        assert event.group(1) == "commissioned=false window=None"
        dut.set(5, 0x0045, 0x0000, "true")
        assert dut.command("set 5 0x0045 0x0000 true") == "ok"
        assert dut.err("fly 2") == "unknown command 'fly'"
        with pytest.raises(HarnessError, match="refused"):
            dut.ok("fly 2")
        with pytest.raises(HarnessError, match="accepted"):
            dut.err("set 5 0x0045 0x0000 false")
    finally:
        dut.stop()
    assert dut.process is None


def test_restart_starts_a_new_process_on_the_same_log(tmp_path: Path) -> None:
    dut = EngineDut(spec(tmp_path), tmp_path, Redactor(), LOGGER).start()
    first = dut.process
    assert first is not None
    dut.restart()
    second = dut.process
    assert second is not None and second is not first
    assert first.returncode == 0, "quit ends the first process cleanly"
    assert second.returncode is None
    dut.stop()
    log = (tmp_path / "fake-engine-dut.log").read_text(encoding="utf-8")
    assert log.count("onoff_node: ready") == 2, "both runs write into the same log"
    assert log.count("event Stopped Requested") == 2


def test_command_without_answer_times_out_with_the_last_lines(tmp_path: Path) -> None:
    silent = DutSpec(
        name="silent-dut",
        command=(sys.executable, "-u", "-c", "import time; print('onoff_node: ready', flush=True); time.sleep(60)"),
        ready_pattern=r"onoff_node: ready",
        ready_timeout=30.0,
        endpoint=2,
        stdin=True,
    )
    dut = EngineDut(silent, tmp_path, Redactor(), LOGGER).start()
    try:
        with pytest.raises(HarnessTimeout) as failure:
            dut.command("add 5 contact-sensor", timeout=1.0)
        assert "| onoff_node: ready" in str(failure.value)
    finally:
        dut.stop(timeout=1.0)


def test_spec_must_take_stdin(tmp_path: Path) -> None:
    without = DutSpec(name="x", command=("x",), ready_pattern="y", ready_timeout=1.0, endpoint=1)
    with pytest.raises(ValueError, match="standard input"):
        EngineDut(without, tmp_path, Redactor(), LOGGER)

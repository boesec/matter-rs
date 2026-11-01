"""ManagedProcess: redacted logs, pattern waits, timeouts that carry the last lines."""

from __future__ import annotations

import logging
import sys
import time
from pathlib import Path

import pytest

from harness.process import HarnessError, HarnessTimeout, ManagedProcess
from harness.redact import MASK, Redactor

LOGGER = logging.getLogger("interop.selftest")


def python(script: str) -> tuple[str, ...]:
    return (sys.executable, "-u", "-c", script)


def test_output_is_collected_and_redacted(tmp_path: Path) -> None:
    redactor = Redactor(["20202021"])
    process = ManagedProcess(
        "printer", python("print('passcode 20202021'); print('done')"), tmp_path / "printer.log", redactor, LOGGER
    )
    code = process.run(timeout=30)

    assert code == 0
    assert process.lines == [f"passcode {MASK}", "done"]
    assert (tmp_path / "printer.log").read_text(encoding="utf-8") == f"passcode {MASK}\ndone\n"


def test_wait_for_times_out_with_the_last_lines(tmp_path: Path) -> None:
    process = ManagedProcess(
        "sleeper",
        python("import time; print('starting up'); print('listening on nothing'); time.sleep(30)"),
        tmp_path / "sleeper.log",
        Redactor(),
        LOGGER,
    ).start()
    try:
        started = time.monotonic()
        with pytest.raises(HarnessTimeout) as failure:
            process.wait_for("node is online", timeout=1.0)
        assert time.monotonic() - started < 10
    finally:
        process.stop()

    message = str(failure.value)
    assert "did not print within 1 s" in message
    assert "| listening on nothing" in message
    assert process.returncode is not None


def test_wait_for_fails_fast_when_the_process_exits(tmp_path: Path) -> None:
    process = ManagedProcess("quitter", python("print('bye')"), tmp_path / "quitter.log", Redactor(), LOGGER).start()
    assert process.wait_for("bye", timeout=30).group(0) == "bye"
    with pytest.raises(HarnessTimeout) as failure:
        process.wait_for("never", timeout=30)
    process.stop()
    assert "exited with code 0 before" in str(failure.value)


def test_mark_limits_the_search_to_later_output(tmp_path: Path) -> None:
    process = ManagedProcess(
        "echo",
        python("import sys\nfor line in sys.stdin:\n    print('got ' + line.strip())"),
        tmp_path / "echo.log",
        Redactor(),
        LOGGER,
        stdin=True,
    ).start()
    try:
        process.send("one")
        process.wait_for("got one", timeout=30)
        since = process.mark()
        process.send("two")
        match = process.wait_for(r"got (\w+)", timeout=30, since=since)
        assert match.group(1) == "two"
    finally:
        process.stop()


def test_run_reports_a_non_zero_exit(tmp_path: Path) -> None:
    process = ManagedProcess(
        "failer", python("print('boom'); raise SystemExit(3)"), tmp_path / "failer.log", Redactor(), LOGGER
    )
    with pytest.raises(HarnessError) as failure:
        process.run(timeout=30)
    assert "exited with code 3" in str(failure.value)
    assert "| boom" in str(failure.value)

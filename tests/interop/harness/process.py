"""A child process whose output is collected, redacted and searchable.

:class:`ManagedProcess` starts a command, copies its output line by line through a
:class:`~harness.redact.Redactor` into a log file and an in-memory list, and lets a scenario wait
for a pattern with a timeout. Every timeout error carries the last lines of the log, so a failing
scenario reads the reason in the assertion message instead of in an artefact.
"""

from __future__ import annotations

import contextlib
import logging
import re
import subprocess
import threading
import time
from pathlib import Path

from harness.redact import Redactor

DEFAULT_TAIL = 30
# chip-tool colours its output and clears the line before every log record; the escape
# sequences would otherwise end up in values the scenarios compare (`"Kitchen"\x1b[0m`).
ANSI_ESCAPE = re.compile(r"\x1b\[[0-9;?]*[A-Za-z]")


def strip_ansi(line: str) -> str:
    """Remove terminal escape sequences (colours, cursor and line control) from ``line``."""
    return ANSI_ESCAPE.sub("", line)


class HarnessError(Exception):
    """A failure of the harness itself or of a counterpart it drives."""


class HarnessTimeout(HarnessError):
    """A pattern or an exit did not arrive in time; the message carries the last log lines."""


def tail(lines: list[str], count: int = DEFAULT_TAIL) -> str:
    chosen = lines[-count:]
    if not chosen:
        return "    (no output)"
    return "\n".join(f"    | {line}" for line in chosen)


class ManagedProcess:
    """One started command with a redacted, searchable log."""

    def __init__(
        self,
        name: str,
        command: tuple[str, ...] | list[str],
        log_path: Path,
        redactor: Redactor,
        logger: logging.Logger,
        *,
        stdin: bool = False,
        container_name: str | None = None,
    ) -> None:
        self.name = name
        self.command = tuple(command)
        self.log_path = log_path
        self.container_name = container_name
        self._redactor = redactor
        self._logger = logger
        self._stdin = stdin
        self._lines: list[str] = []
        self._condition = threading.Condition()
        self._eof = False
        self._process: subprocess.Popen[bytes] | None = None
        self._reader: threading.Thread | None = None
        self.started_at: float | None = None

    # -- lifecycle -------------------------------------------------------------------------

    def start(self) -> ManagedProcess:
        self.log_path.parent.mkdir(parents=True, exist_ok=True)
        self._logger.info("%s: starting %s", self.name, " ".join(self.command))
        self.started_at = time.monotonic()
        self._process = subprocess.Popen(
            self.command,
            stdin=subprocess.PIPE if self._stdin else subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            bufsize=0,
        )
        self._reader = threading.Thread(target=self._pump, name=f"reader-{self.name}", daemon=True)
        self._reader.start()
        return self

    def _pump(self) -> None:
        assert self._process is not None and self._process.stdout is not None
        with self.log_path.open("a", encoding="utf-8") as sink:
            for raw in iter(self._process.stdout.readline, b""):
                line = self._redactor.redact(strip_ansi(raw.decode("utf-8", errors="replace").rstrip("\r\n")))
                sink.write(line + "\n")
                sink.flush()
                with self._condition:
                    self._lines.append(line)
                    self._condition.notify_all()
        with self._condition:
            self._eof = True
            self._condition.notify_all()

    @property
    def returncode(self) -> int | None:
        return None if self._process is None else self._process.poll()

    @property
    def lines(self) -> list[str]:
        with self._condition:
            return list(self._lines)

    def mark(self) -> int:
        """The current line count; pass it as ``since`` to search only later output."""
        with self._condition:
            return len(self._lines)

    def tail(self, count: int = DEFAULT_TAIL) -> str:
        return tail(self.lines, count)

    def send(self, line: str) -> None:
        assert self._process is not None and self._process.stdin is not None
        self._logger.info("%s: sending %s", self.name, line)
        self._process.stdin.write((line + "\n").encode("utf-8"))
        self._process.stdin.flush()

    # -- waiting ---------------------------------------------------------------------------

    def wait_for(self, pattern: str | re.Pattern[str], timeout: float, since: int = 0) -> re.Match[str]:
        """Block until a line at index >= ``since`` matches ``pattern`` or the timeout elapses.

        Raises :class:`HarnessTimeout` with the last log lines if the pattern does not show up,
        also when the process exits first.
        """
        compiled = re.compile(pattern) if isinstance(pattern, str) else pattern
        deadline = time.monotonic() + timeout
        index = since
        with self._condition:
            while True:
                while index < len(self._lines):
                    match = compiled.search(self._lines[index])
                    index += 1
                    if match:
                        return match
                if self._eof:
                    break
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    break
                self._condition.wait(remaining)
        if self._eof:
            # The output is closed, so the process is gone or about to be; collect its code.
            assert self._process is not None
            with contextlib.suppress(subprocess.TimeoutExpired):
                self._process.wait(5)
            reason = f"exited with code {self.returncode} before"
        else:
            reason = f"did not print within {timeout:.0f} s"
        raise HarnessTimeout(
            f"{self.name}: {reason} the expected pattern {compiled.pattern!r} appeared.\n"
            f"Last lines of {self.log_path.name}:\n{self.tail()}"
        )

    def wait(self, timeout: float) -> int:
        """Wait for the process to exit; raises :class:`HarnessTimeout` otherwise."""
        assert self._process is not None
        try:
            code = self._process.wait(timeout)
        except subprocess.TimeoutExpired as error:
            raise HarnessTimeout(
                f"{self.name}: still running after {timeout:.0f} s.\nLast lines of {self.log_path.name}:\n{self.tail()}"
            ) from error
        if self._reader is not None:
            self._reader.join(10)
        return code

    def run(self, timeout: float, *, expect_success: bool = True) -> int:
        """Start, wait for the exit and optionally require exit code 0."""
        self.start()
        try:
            code = self.wait(timeout)
        finally:
            self.stop()
        if expect_success and code != 0:
            raise HarnessError(
                f"{self.name}: exited with code {code}.\nLast lines of {self.log_path.name}:\n{self.tail()}"
            )
        return code

    def expect_pattern(self, pattern: str) -> re.Match[str]:
        """Find ``pattern`` in the collected output or raise with the last lines."""
        compiled = re.compile(pattern)
        for line in self.lines:
            match = compiled.search(line)
            if match:
                return match
        raise HarnessError(
            f"{self.name}: pattern {pattern!r} not found in its output.\n"
            f"Last lines of {self.log_path.name}:\n{self.tail()}"
        )

    # -- stopping --------------------------------------------------------------------------

    def stop(self, grace: float = 10.0) -> None:
        """Stop the process (and its container, if it has one) and close the log."""
        process = self._process
        if process is None:
            return
        if process.poll() is None:
            if self.container_name:
                subprocess.run(
                    ["docker", "stop", "-t", str(int(grace)), self.container_name],
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                    check=False,
                    timeout=grace + 30,
                )
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(grace)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(grace)
        if self.container_name:
            subprocess.run(
                ["docker", "rm", "-f", self.container_name],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                check=False,
                timeout=60,
            )
        if self._reader is not None:
            self._reader.join(10)
        if process.stdin is not None:
            with contextlib.suppress(OSError):
                process.stdin.close()
        self._logger.info("%s: stopped with code %s", self.name, process.returncode)

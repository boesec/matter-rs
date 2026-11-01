"""The matter.js shell as a second controller, driven through its standard input.

The shell prints ``Done.`` after every command it executed, so a command is "sent, then wait for
``Done.`` after the point where it was sent, then look for the expected line in between".
"""

from __future__ import annotations

import logging
import re
from pathlib import Path

from harness.config import HarnessConfig
from harness.docker import run_command
from harness.process import HarnessError, ManagedProcess
from harness.redact import Redactor

STARTED = r"Started Node #"
DONE = r"Done\."


class MatterShell:
    def __init__(
        self,
        config: HarnessConfig,
        redactor: Redactor,
        logger: logging.Logger,
        logs_dir: Path,
        *,
        node_num: int = 1,
    ) -> None:
        self._config = config
        self._redactor = redactor
        self._logger = logger
        self._logs_dir = logs_dir
        self._node_num = node_num
        self.volume = config.volume_name("shell")
        self.process: ManagedProcess | None = None

    def start(self, timeout: float = 120.0) -> None:
        name = self._config.container_name("shell")
        command = run_command(
            self._config.matterjs_image,
            ("node_modules/.bin/matter-shell", str(self._node_num)),
            name=name,
            interactive=True,
            mounts=((self.volume, "/state", False),),
            env={"MATTER_STORAGE_PATH": "/state", "MATTER_LOG_FORMAT": "plain"},
        )
        self.process = ManagedProcess(
            "matter.js shell",
            command,
            self._logs_dir / "matterjs-shell.log",
            self._redactor,
            self._logger,
            stdin=True,
            container_name=name,
        ).start()
        self.process.wait_for(STARTED, timeout)

    def command(self, line: str, expect: str, timeout: float) -> re.Match[str]:
        """Send a shell command, wait for its ``Done.`` and return the match of ``expect``."""
        assert self.process is not None, "shell not started"
        since = self.process.mark()
        self.process.send(line)
        self.process.wait_for(DONE, timeout, since)
        compiled = re.compile(expect)
        for text in self.process.lines[since:]:
            match = compiled.search(text)
            if match:
                return match
        raise HarnessError(
            f"matter.js shell: command {line!r} finished without the expected line {expect!r}.\n"
            f"Last lines of {self.process.log_path.name}:\n{self.process.tail()}"
        )

    def pair(self, node_id: int, manual_code: str, timeout: float = 300.0) -> None:
        self.command(
            f"commission pair {node_id} --pairing-code {manual_code}", rf"Commissioned Node: {node_id}\b", timeout
        )

    def toggle(self, node_id: int, endpoint: int, timeout: float = 60.0) -> None:
        self.command(
            f"commands onoff toggle {node_id} {endpoint}",
            rf"Command Toggle {node_id}/{endpoint}/6/2 invoked",
            timeout,
        )

    def read_on_off(self, node_id: int, endpoint: int, timeout: float = 60.0) -> bool:
        match = self.command(
            f"attributes onoff read onoff {node_id} {endpoint} --remote",
            r"(?:onOff \(0\)|/6/0):\s*(true|false)",
            timeout,
        )
        return match.group(1) == "true"

    def close(self) -> None:
        if self.process is None:
            return
        try:
            if self.process.returncode is None:
                self.process.send("exit")
                self.process.wait(30)
        except (HarnessError, OSError):
            pass
        finally:
            self.process.stop()

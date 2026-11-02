"""One connectedhomeip Python test case, run through its own framework in the certification image.

The framework (``matter.testing``) commissions the device itself, runs the test class with Mobly
and writes ``test_summary.yaml`` into ``--logs-path``. That file, not the exit code, is what the
harness parses: a record per test with ``PASS``/``FAIL``/``SKIP``/``ERROR`` and a summary with
the counts.
"""

from __future__ import annotations

import logging
import os
from dataclasses import dataclass, field
from pathlib import Path

import yaml

from harness.config import HarnessConfig
from harness.docker import DBUS_CLIENT_SECURITY_OPTS, host_path, run_command
from harness.process import HarnessError, ManagedProcess
from harness.redact import Redactor

SCRIPTS_DIR = "/root/python_testing/scripts/sdk"

# The Matter test PAA roots the framework trusts (see paa/README.md), mounted as /paa.
PAA_DIR = Path(__file__).resolve().parent.parent / "paa"
SUMMARY_FILE = "test_summary.yaml"


@dataclass(frozen=True)
class TcRecord:
    name: str
    result: str
    details: str | None = None


@dataclass
class TcResult:
    summary_path: Path
    records: list[TcRecord] = field(default_factory=list)
    counts: dict[str, int] = field(default_factory=dict)

    @property
    def passed(self) -> int:
        return self.counts.get("Passed", 0)

    @property
    def failed(self) -> int:
        return self.counts.get("Failed", 0)

    @property
    def errors(self) -> int:
        return self.counts.get("Error", 0)

    @property
    def skipped(self) -> int:
        return self.counts.get("Skipped", 0)

    def record(self, name: str) -> TcRecord:
        for record in self.records:
            if record.name == name:
                return record
        raise HarnessError(f"no record for {name} in {self.summary_path}; records: {[r.name for r in self.records]}")

    def describe(self) -> str:
        lines = [f"summary {self.counts} from {self.summary_path}"]
        for record in self.records:
            lines.append(f"  {record.result:5s} {record.name}")
            if record.details:
                lines.append(f"        {record.details}")
        return "\n".join(lines)


def parse_summary(path: Path) -> TcResult:
    """Parse a Mobly ``test_summary.yaml`` (several YAML documents in one file)."""
    result = TcResult(summary_path=path)
    with path.open(encoding="utf-8") as handle:
        for document in yaml.safe_load_all(handle):
            if not isinstance(document, dict):
                continue
            kind = document.get("Type")
            if kind == "Record":
                result.records.append(
                    TcRecord(
                        name=str(document.get("Test Name")),
                        result=str(document.get("Result")),
                        details=None if document.get("Details") is None else str(document.get("Details")),
                    )
                )
            elif kind == "Summary":
                for key in ("Requested", "Executed", "Passed", "Failed", "Error", "Skipped"):
                    if key in document:
                        result.counts[key] = int(document[key])
    return result


def find_summary(root: Path) -> Path | None:
    """The newest ``test_summary.yaml`` below ``root``; symlinked copies are ignored."""
    candidates = [
        p for p in root.rglob(SUMMARY_FILE) if p.is_file() and not any(part.is_symlink() for part in p.parents)
    ]
    if not candidates:
        return None
    return max(candidates, key=lambda p: p.stat().st_mtime)


class PythonTestRunner:
    def __init__(
        self,
        config: HarnessConfig,
        redactor: Redactor,
        logger: logging.Logger,
        logs_dir: Path,
        *,
        component: str = "python-tc",
    ) -> None:
        """A runner whose framework storage (its fabric) lives in the volume named ``component``."""
        self._config = config
        self._redactor = redactor
        self._logger = logger
        self._logs_dir = logs_dir
        self._component = component
        self._counter = 0
        self.volume = config.volume_name(component)

    def run(
        self,
        script: str,
        manual_code: str | None,
        pics_file: Path,
        *,
        timeout: float = 420.0,
        extra_args: tuple[str, ...] = (),
    ) -> tuple[ManagedProcess, TcResult]:
        """Run ``script`` (a ``TC_*.py`` file of the framework) and parse its summary.

        With a ``manual_code`` the framework commissions the device first (a fresh fabric in this
        runner's storage); with ``None`` it runs on the fabric an earlier run of this runner
        left in its storage, which is how several scripts share one commissioning.
        """
        host_logs = self._logs_dir / "framework" / Path(script).stem
        host_logs.mkdir(parents=True, exist_ok=True)
        # The container runs as root and writes the framework logs into a bind mount; hand them
        # back to the calling user so the harness can redact them in place afterwards.
        owner = f"{os.getuid()}:{os.getgid()}" if hasattr(os, "getuid") else "0:0"
        inner = f'python3 "$0" "$@"; status=$?; chown -R {owner} /logs; exit $status'
        commissioning = (
            () if manual_code is None else ("--commissioning-method", "on-network", "--manual-code", manual_code)
        )
        args = (
            "bash",
            "-c",
            inner,
            f"{SCRIPTS_DIR}/{script}",
            *commissioning,
            "--PICS",
            f"/pics/{pics_file.name}",
            "--storage-path",
            "/state/admin_storage.json",
            "--logs-path",
            "/logs",
            # Without an explicit trust store the framework looks next to its own scripts, finds
            # no root and fails device attestation; the test roots live in the harness.
            "--paa-trust-store-path",
            "/paa",
            *extra_args,
        )
        self._counter += 1
        name = self._config.container_name(f"{self._component}-{self._counter}")
        command = run_command(
            self._config.chip_image,
            args,
            name=name,
            mounts=(
                (self._config.dbus_socket, self._config.dbus_socket, False),
                (self.volume, "/state", False),
                (host_path(host_logs), "/logs", False),
                (host_path(pics_file.parent), "/pics", True),
                (host_path(PAA_DIR), "/paa", True),
            ),
            workdir=SCRIPTS_DIR,
            security_opts=DBUS_CLIENT_SECURITY_OPTS,
        )
        process = ManagedProcess(
            f"python-tc {script}",
            command,
            self._logs_dir / f"{self._counter:02d}-{Path(script).stem}.log",
            self._redactor,
            self._logger,
            container_name=name,
        )
        process.run(timeout, expect_success=False)
        self._redactor.redact_tree(host_logs)
        summary = find_summary(host_logs)
        if summary is None:
            raise HarnessError(
                f"python-tc {script}: the framework wrote no {SUMMARY_FILE} below {host_logs}.\n"
                f"Last lines of {process.log_path.name}:\n{process.tail()}"
            )
        return process, parse_summary(summary)

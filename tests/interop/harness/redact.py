"""Redaction of secrets in everything the harness writes.

Every line the harness stores, logs or puts into an assertion message passes through a
:class:`Redactor`. It masks the configured secrets (passcode, manual pairing codes), any onboarding
payload (``MT:...``) and any 11- or 21-digit number, which is the shape of a manual pairing code.
Tools may print new codes at run time (an enhanced commissioning window creates a fresh passcode);
a :class:`SecretCapture` picks such a value out of the raw line, registers it as a secret and
hands it to the caller, so the value never reaches a log file.
"""

from __future__ import annotations

import re
import threading
from dataclasses import dataclass, field
from pathlib import Path

MASK = "<redacted>"

_ONBOARDING_PAYLOAD = re.compile(r"MT:[A-Z0-9.\-]{8,}")
_MANUAL_CODE_SHAPE = re.compile(r"(?<![0-9])(?:[0-9]{11}|[0-9]{21})(?![0-9])")


@dataclass
class SecretCapture:
    """A pattern whose first group is a secret printed by a tool; the value lands in ``value``."""

    pattern: re.Pattern[str]
    value: str | None = None
    event: threading.Event = field(default_factory=threading.Event)

    def wait(self, timeout: float) -> str | None:
        self.event.wait(timeout)
        return self.value


class Redactor:
    """Masks secrets in text. Thread-safe: reader threads of several processes share one."""

    def __init__(self, secrets: tuple[str, ...] | list[str] = ()) -> None:
        self._lock = threading.Lock()
        self._secrets: list[str] = []
        self._captures: list[SecretCapture] = []
        for secret in secrets:
            self.add_secret(secret)

    def add_secret(self, secret: str | int) -> None:
        text = str(secret)
        if not text:
            return
        with self._lock:
            if text not in self._secrets:
                self._secrets.append(text)
                # Longest first, so a code that contains the passcode is masked as a whole.
                self._secrets.sort(key=len, reverse=True)

    @property
    def secrets(self) -> tuple[str, ...]:
        with self._lock:
            return tuple(self._secrets)

    def capture(self, pattern: str) -> SecretCapture:
        """Register a capture; the first group of ``pattern`` is a secret when it appears."""
        compiled = re.compile(pattern)
        if compiled.groups < 1:
            raise ValueError("a capture pattern needs one group for the secret")
        capture = SecretCapture(compiled)
        with self._lock:
            self._captures.append(capture)
        return capture

    def redact(self, line: str) -> str:
        """Return ``line`` with every known secret, payload and code-shaped number masked."""
        with self._lock:
            captures = list(self._captures)
        for capture in captures:
            if capture.value is None:
                match = capture.pattern.search(line)
                if match:
                    capture.value = match.group(1)
                    self.add_secret(capture.value)
                    capture.event.set()
        for secret in self.secrets:
            line = line.replace(secret, MASK)
        line = _ONBOARDING_PAYLOAD.sub(MASK, line)
        line = _MANUAL_CODE_SHAPE.sub(MASK, line)
        return line

    def redact_file(self, path: Path) -> bool:
        """Redact a text file in place; returns False for files that are not UTF-8 text."""
        try:
            text = path.read_text(encoding="utf-8")
        except (UnicodeDecodeError, OSError):
            return False
        redacted = "\n".join(self.redact(line) for line in text.split("\n"))
        if redacted != text:
            path.write_text(redacted, encoding="utf-8")
        return True

    def redact_tree(self, root: Path) -> int:
        """Redact every regular file below ``root`` in place; returns the number of files seen."""
        count = 0
        if not root.exists():
            return count
        for path in sorted(root.rglob("*")):
            if path.is_file() and not path.is_symlink():
                self.redact_file(path)
                count += 1
        return count

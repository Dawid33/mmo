"""One command runner for the deploy orchestrator, and a scripted fake for tests.

Secrets are never passed in argv: ECR passwords go through stdin, tokens and
kubeconfigs through the environment. `Runner.run` prints `+ cmd` lines so a run
is auditable without printing any of that.
"""

from __future__ import annotations

import os
import shlex
import signal
import subprocess
from dataclasses import dataclass, field
from pathlib import Path
from typing import Mapping, Sequence


@dataclass(frozen=True)
class Completed:
    rc: int
    out: str = ""
    err: str = ""


class DeployError(RuntimeError):
    """A step failed. `code` is the process exit code the CLI should use."""

    def __init__(self, message: str, code: int = 1) -> None:
        super().__init__(message)
        self.code = code


class Runner:
    """Runs commands from the repository root, printing each one."""

    def __init__(self, cwd: Path | str | None = None) -> None:
        self.cwd = Path(cwd) if cwd is not None else Path(__file__).resolve().parents[2]

    def run(
        self,
        argv: Sequence[str | Path],
        *,
        check: bool = True,
        capture: bool = False,
        stdin: bytes | None = None,
        env: Mapping[str, str] | None = None,
        cwd: Path | str | None = None,
    ) -> Completed:
        words = [str(word) for word in argv]
        print("+ " + shlex.join(words), flush=True)
        child_env = None
        if env:
            child_env = os.environ.copy()
            child_env.update({str(key): str(value) for key, value in env.items()})
        proc = subprocess.Popen(
            words,
            cwd=str(cwd or self.cwd),
            env=child_env,
            stdin=subprocess.PIPE if stdin is not None else None,
            stdout=subprocess.PIPE if capture else None,
            stderr=subprocess.PIPE if capture else None,
        )
        try:
            out, err = proc.communicate(stdin)
        except KeyboardInterrupt:
            # Forward the signal to the child (world_update.sh prints its own
            # recovery message on the way out) and never swallow it.
            try:
                proc.send_signal(signal.SIGINT)
                proc.wait(timeout=10)
            except (ProcessLookupError, subprocess.TimeoutExpired):
                proc.kill()
                proc.wait()
            raise
        result = Completed(
            proc.returncode,
            out.decode("utf-8", "replace") if out else "",
            err.decode("utf-8", "replace") if err else "",
        )
        if check and result.rc != 0:
            detail = (result.err or result.out).strip()
            raise DeployError(
                f"command failed (exit {result.rc}): {shlex.join(words)}"
                + (f"\n{detail}" if detail else "")
            )
        return result


class FakeRunner:
    """Scripted Runner for tests: `responses` maps an argv prefix to a result.

    The longest matching prefix wins; `()` is the default. Every call is
    recorded in `calls` (and, for assertions on pipes/env, in `stdins`/`envs`).
    """

    def __init__(
        self,
        responses: Mapping[tuple[str, ...], Completed] | None = None,
        default: Completed | None = None,
    ) -> None:
        self.responses = {tuple(k): v for k, v in (responses or {}).items()}
        self.default = default or Completed(0, "", "")
        self.calls: list[list[str]] = []
        self.stdins: list[bytes | None] = []
        self.envs: list[dict[str, str] | None] = []

    def run(
        self,
        argv: Sequence[str | Path],
        *,
        check: bool = True,
        capture: bool = False,
        stdin: bytes | None = None,
        env: Mapping[str, str] | None = None,
        cwd: Path | str | None = None,
    ) -> Completed:
        words = [str(word) for word in argv]
        self.calls.append(words)
        self.stdins.append(stdin)
        self.envs.append(dict(env) if env else None)
        key = next(
            (
                k
                for k in sorted(self.responses, key=len, reverse=True)
                if tuple(words[: len(k)]) == k
            ),
            (),
        )
        result = self.responses.get(key, self.default)
        if check and result.rc != 0:
            raise DeployError(f"fake command failed (exit {result.rc}): {shlex.join(words)}")
        return result

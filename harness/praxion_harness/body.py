"""Start a server, a forwarder and the ``praxion-body`` process for one seed.

The body connects to the forwarder as a game client and to its tee, so the run
exercises the same bytes path as Demo-1 (docs/decisions.md 2026-11-01, "Demo-0
data path").

```python
from praxion_harness.body import start_body

with start_body(101, "get_wooden_pickaxe",
                setup=lambda fixture, referee, run: fixture.give("Praxion", "oak_log", 3)) as run:
    summary = run.wait_for_outcome(timeout=240)
    assert summary["goal"]["outcome"] == "done"
    referee = run.referee
    assert referee.player_inventory()["minecraft:wooden_pickaxe"] >= 1
```

The body waits ``start_delay_ticks`` (3 s by default) after its first tick, so a
``setup`` callback has time to give items, set the time and summon mobs through
the logged RCON fixture channel before the skill starts.

Only ports 25640-25659 are used for the forwarder's game and tee listeners. The
server keeps its own random port from ``TestServer``.
"""

from __future__ import annotations

import json
import os
import socket
import subprocess
import time
import uuid
from pathlib import Path
from typing import Any, Callable, Dict, Iterable, Optional

from .fixtures import FixtureChannel, fixture_log_path
from .paths import repo_root
from .referee import Referee
from .server import TestServer

#: Ports this helper may use (the task's reserved range).
BODY_PORT_RANGE = range(25640, 25660)
#: Seconds to wait for the body's ``joined`` line.
BODY_JOIN_TIMEOUT = 90.0
#: Seconds to wait for the forwarder's listening line.
FORWARDER_READY_TIMEOUT = 30.0

Setup = Callable[[FixtureChannel, Referee, "BodyRun"], None]


def _binary(name: str) -> Path:
    suffix = ".exe" if os.name == "nt" else ""
    return repo_root() / "body" / "target" / "release" / f"{name}{suffix}"


def _port_free(port: int) -> bool:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        try:
            sock.bind(("127.0.0.1", port))
        except OSError:
            return False
    return True


def _free_ports(count: int, used: Optional[set[int]] = None) -> list[int]:
    used = set() if used is None else set(used)
    ports: list[int] = []
    for port in BODY_PORT_RANGE:
        if len(ports) == count:
            break
        if port in used or not _port_free(port):
            continue
        ports.append(port)
        used.add(port)
        # Bind then release, so the next candidate cannot silently take it.
    if len(ports) < count:
        raise RuntimeError(
            f"no free port for the body in {BODY_PORT_RANGE.start}-{BODY_PORT_RANGE.stop - 1}"
        )
    return ports


class BodyRun:
    """A started body, forwarder and server, and how to read what it did."""

    def __init__(
        self,
        run_dir: Path,
        server: TestServer,
        forwarder: subprocess.Popen,
        forwarder_out: Path,
        process: subprocess.Popen,
        output_path: Path,
        referee: Referee,
        fixture: FixtureChannel,
        *,
        keep: bool,
    ) -> None:
        self.run_dir = Path(run_dir)
        self.body_dir = self.run_dir / "body"
        self.server = server
        self.forwarder = forwarder
        self.forwarder_out = Path(forwarder_out)
        self.process = process
        self.output_path = Path(output_path)
        self.referee = referee
        self.fixture = fixture
        self.keep = keep
        self._out = None
        self._forwarder_out = None
        self._stopped = False

    # ------------------------------------------------------------------ #
    def output(self) -> str:
        try:
            return self.output_path.read_text(encoding="utf-8", errors="replace")
        except OSError:
            return ""

    def forwarder_output(self) -> str:
        try:
            return self.forwarder_out.read_text(encoding="utf-8", errors="replace")
        except OSError:
            return ""

    def summary(self) -> Optional[Dict[str, Any]]:
        path = self.body_dir / "summary.json"
        try:
            return json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            return None

    def events(self) -> list[dict]:
        path = self.body_dir / "events.jsonl"
        events = []
        try:
            with path.open("r", encoding="utf-8", errors="replace") as handle:
                for line in handle:
                    line = line.strip()
                    if line:
                        events.append(json.loads(line))
        except OSError:
            return events
        return events

    def wait_for_outcome(self, timeout: float = 240.0) -> Dict[str, Any]:
        """Wait until the goal settled (or the process ended) and return the summary."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            summary = self.summary()
            if summary is not None and summary.get("goal", {}).get("outcome") not in (None, "running"):
                return summary
            if self.process.poll() is not None:
                summary = self.summary()
                if summary is not None:
                    return summary
                raise RuntimeError(
                    f"the body exited (code {self.process.returncode}) without summary.json "
                    f"({self.output_path}):\n{self.output()[-4000:]}"
                )
            time.sleep(0.1)
        raise TimeoutError(
            f"the goal did not settle within {timeout:.0f}s ({self.output_path}):\n"
            f"{self.output()[-4000:]}"
        )

    # ------------------------------------------------------------------ #
    def stop(self) -> None:
        if self._stopped:
            return
        self._stopped = True
        _terminate(self.process, 10.0)
        try:
            if self._out is not None:
                self._out.close()
        except Exception:
            pass
        _terminate(self.forwarder, 10.0)
        try:
            if self._forwarder_out is not None:
                self._forwarder_out.close()
        except Exception:
            pass
        if self.keep:
            self.server.keep = True
        self.server.stop()

    def __enter__(self) -> "BodyRun":
        return self

    def __exit__(self, exc_type, exc, tb) -> None:
        if exc_type is not None:
            # Keep the evidence of a failed live run.
            self.keep = True
        self.stop()


def _terminate(process: Optional[subprocess.Popen], timeout: float) -> None:
    if process is None or process.poll() is not None:
        return
    try:
        process.terminate()
    except Exception:
        pass
    try:
        process.wait(timeout=timeout)
        return
    except Exception:
        pass
    try:
        process.kill()
    except Exception:
        pass
    try:
        process.wait(timeout=5)
    except Exception:
        pass


def _wait_for(path: Path, needle: str, process: subprocess.Popen, timeout: float) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if process.poll() is not None:
            try:
                text = path.read_text(encoding="utf-8", errors="replace")
            except OSError:
                text = ""
            raise RuntimeError(
                f"process exited before printing {needle!r} (code {process.returncode}, {path}):\n{text}"
            )
        try:
            if needle in path.read_text(encoding="utf-8", errors="replace"):
                return
        except OSError:
            pass
        time.sleep(0.05)
    try:
        text = path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        text = ""
    raise TimeoutError(f"process did not print {needle!r} within {timeout:.0f}s ({path}):\n{text}")


def start_body(
    seed: int,
    goal: str,
    goal_args: Iterable[str] = (),
    *,
    setup: Optional[Setup] = None,
    max_ticks: int = 6000,
    start_delay_ticks: int = 60,
    linger_ticks: int = 60,
    settings: Optional[Path] = None,
    name: str = "Praxion",
    run_dir: Optional[Path] = None,
    keep: Optional[bool] = None,
    extra_args: Iterable[str] = (),
) -> BodyRun:
    """Start server + forwarder + body for ``seed``; return the running run.

    Blocks until the body prints ``joined`` and the optional ``setup`` callback
    returned. The caller owns the run; use it as a context manager or call
    :meth:`BodyRun.stop`.
    """
    for binary in ("praxion-body", "praxion-forwarder"):
        path = _binary(binary)
        if not path.exists():
            raise RuntimeError(
                f"{binary} is not built ({path}); run: cd body && cargo build --release"
            )

    run_dir = Path(run_dir) if run_dir is not None else (
        repo_root() / "runs" / f"test-body-{uuid.uuid4().hex}"
    )
    run_dir.mkdir(parents=True, exist_ok=True)
    server = TestServer(run_dir, seed, keep=keep)
    forwarder: Optional[subprocess.Popen] = None
    body: Optional[subprocess.Popen] = None
    body_out = None
    forwarder_out = None
    try:
        server.start()
        referee = Referee(server.rcon, player=name)
        referee.setup_death_objective()
        fixture = FixtureChannel(server.rcon, fixture_log_path(run_dir))

        game_port, tee_port = _free_ports(2)
        forwarder_out_path = run_dir / "forwarder.out"
        forwarder_out = forwarder_out_path.open("w", encoding="utf-8", errors="replace")
        forwarder = subprocess.Popen(
            [
                str(_binary("praxion-forwarder")),
                "--listen", f"127.0.0.1:{game_port}",
                "--upstream", f"127.0.0.1:{server.port}",
                "--tee-listen", f"127.0.0.1:{tee_port}",
                "--stats", str(run_dir / "forwarder.jsonl"),
            ],
            cwd=str(repo_root()),
            stdout=forwarder_out,
            stderr=subprocess.STDOUT,
        )
        _wait_for(forwarder_out_path, "tee on", forwarder, FORWARDER_READY_TIMEOUT)

        body_dir = run_dir / "body"
        body_out_path = run_dir / "body.out"
        body_out = body_out_path.open("w", encoding="utf-8", errors="replace")
        command = [
            str(_binary("praxion-body")),
            "--forwarder", f"127.0.0.1:{game_port}",
            "--tee", f"127.0.0.1:{tee_port}",
            "--name", name,
            "--goal", goal,
            "--run-dir", str(body_dir),
            "--max-ticks", str(max_ticks),
            "--start-delay-ticks", str(start_delay_ticks),
            "--linger-ticks", str(linger_ticks),
        ]
        if goal_args:
            command.append("--goal-args")
            command.extend(str(argument) for argument in goal_args)
        if settings is not None:
            command.extend(["--settings", str(settings)])
        command.extend(str(argument) for argument in extra_args)
        body = subprocess.Popen(
            command,
            cwd=str(repo_root()),
            stdout=body_out,
            stderr=subprocess.STDOUT,
        )
        _wait_for(body_out_path, "joined", body, BODY_JOIN_TIMEOUT)

        run = BodyRun(
            run_dir,
            server,
            forwarder,
            forwarder_out_path,
            body,
            body_out_path,
            referee,
            fixture,
            keep=server.keep,
        )
        run._out = body_out
        run._forwarder_out = forwarder_out
        if setup is not None:
            setup(fixture, referee, run)
        return run
    except BaseException:
        _terminate(body, 5.0)
        try:
            if body_out is not None:
                body_out.close()
        except Exception:
            pass
        _terminate(forwarder, 5.0)
        try:
            if forwarder_out is not None:
                forwarder_out.close()
        except Exception:
            pass
        server.stop()
        raise


__all__ = [
    "BODY_PORT_RANGE",
    "BodyRun",
    "start_body",
]

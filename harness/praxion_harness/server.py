"""Control a local vanilla Minecraft test server.

``TestServer`` creates ``<run_dir>/server/`` with eula.txt and server.properties,
starts Java with the pinned server jar, waits for the ``Done (`` and
``RCON running on`` lines, exposes ``rcon``, ``port`` and ``log_path``, and stops
gracefully (RCON ``stop``, then process-tree kill after a timeout). Context manager.

Run directories are removed on a clean :meth:`TestServer.stop` unless ``keep=True``
or ``PRAXION_KEEP_RUNS=1`` is set, so a failed test can keep its evidence.

Every started server is tracked in a module-level registry; :func:`stop_active_servers`
stops whatever a test session left behind.
"""

from __future__ import annotations

import logging
import os
import shutil
import socket
import subprocess
import time
from pathlib import Path
from typing import Any, Dict, List, Optional

from .paths import repo_root, resolve_java, resolve_server_jar
from .rcon import RconClient

logger = logging.getLogger(__name__)

FORBIDDEN_PORTS = {25565, 25566, 25570, 25571, 25575}
DEFAULT_RCON_PASSWORD = "praxion-local"
READY_TIMEOUT = 180.0
STOP_TIMEOUT = 30.0
KEEP_RUNS_ENV = "PRAXION_KEEP_RUNS"

# Strong references: a started server must stay tracked until stop(), even after
# the test that created it returns (a WeakSet would drop it and leak Java).
_ACTIVE_SERVERS: "set[TestServer]" = set()


def _free_port(forbidden: set[int]) -> int:
    while True:
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
            s.bind(("127.0.0.1", 0))
            port = s.getsockname()[1]
        if port not in forbidden and port not in FORBIDDEN_PORTS:
            return port


def keep_runs_by_default() -> bool:
    """Whether ``PRAXION_KEEP_RUNS`` asks to keep run directories."""
    return os.environ.get(KEEP_RUNS_ENV, "").strip().lower() in {"1", "true", "yes", "on"}


def active_servers() -> List["TestServer"]:
    """Every server this process started and has not stopped yet."""
    return list(_ACTIVE_SERVERS)


def stop_active_servers() -> List[Path]:
    """Stop every still-running server; return the run directories that were kept."""
    kept: List[Path] = []
    for srv in list(_ACTIVE_SERVERS):
        if srv.keep:
            kept.append(srv.run_dir)
        srv.stop()
    return kept


class TestServer:
    # Pytest must not collect this class as a test case (it starts a real server).
    __test__ = False

    def __init__(
        self,
        run_dir,
        seed,
        *,
        properties: Optional[Dict[str, Any]] = None,
        properties_overrides: Optional[Dict[str, Any]] = None,
        overrides: Optional[Dict[str, Any]] = None,
        keep: Optional[bool] = None,
        **extra: Any,
    ):
        self.run_dir = Path(run_dir)
        if self.run_dir.name == "server":
            self.server_dir = self.run_dir
            self.run_dir = self.run_dir.parent
        else:
            self.server_dir = self.run_dir / "server"
        self.seed = seed
        self.keep = keep_runs_by_default() if keep is None else bool(keep)

        merged: Dict[str, Any] = {}
        for d in (properties, properties_overrides, overrides):
            if d:
                merged.update(d)
        merged.update(extra)
        self._overrides = merged

        self.java = resolve_java()
        self.jar = resolve_server_jar()

        self.port: int = 0
        self.rcon_port: int = 0
        self.rcon: Optional[RconClient] = None
        self.log_path: Path = self.server_dir / "logs" / "latest.log"
        self.stdout_path: Path = self.server_dir / "server.out"
        self.process: Optional[subprocess.Popen] = None
        self._stdout_fh = None
        self._stopped = False

        # Startup measurement (monotonic seconds relative to the Popen call).
        self.started_at: Optional[float] = None
        self.done_at: Optional[float] = None
        self.ready_at: Optional[float] = None

    # ------------------------------------------------------------------ #
    @property
    def startup_seconds(self) -> Optional[float]:
        """Seconds from spawning Java until the ``Done (`` line (None if not seen)."""
        if self.started_at is None or self.done_at is None:
            return None
        return self.done_at - self.started_at

    @property
    def ready_seconds(self) -> Optional[float]:
        """Seconds until both ``Done (`` and RCON were observed."""
        if self.started_at is None or self.ready_at is None:
            return None
        return self.ready_at - self.started_at

    def _prepare(self) -> None:
        self.server_dir.mkdir(parents=True, exist_ok=True)
        # Fresh world: remove any stale world dir.
        world_dir = self.server_dir / "world"
        if world_dir.exists():
            shutil.rmtree(world_dir, ignore_errors=True)

        # Ports.
        forbidden = set(FORBIDDEN_PORTS)
        self.port = _free_port(forbidden)
        forbidden.add(self.port)
        self.rcon_port = _free_port(forbidden)

        # eula.
        (self.server_dir / "eula.txt").write_text("eula=true\n", encoding="utf-8")

        # properties.
        props = {
            "online-mode": "false",
            "server-ip": "127.0.0.1",
            "server-port": str(self.port),
            "enable-rcon": "true",
            "rcon.port": str(self.rcon_port),
            "rcon.password": DEFAULT_RCON_PASSWORD,
            "level-name": "world",
            "level-seed": str(self.seed),
            "difficulty": "normal",
            "gamemode": "survival",
            "spawn-protection": "0",
            "view-distance": "8",
            "max-players": "4",
            "white-list": "false",
        }
        # Allow overrides using either hyphen or underscore keys.
        for k, v in self._overrides.items():
            key = k.replace("_", "-")
            props[key] = str(v)
        lines = [f"{k}={v}" for k, v in props.items()]
        (self.server_dir / "server.properties").write_text(
            "\n".join(lines) + "\n", encoding="utf-8"
        )

    # ------------------------------------------------------------------ #
    def start(self) -> "TestServer":
        self._prepare()
        out_fh = self.stdout_path.open("w", encoding="utf-8", errors="replace")
        self._stdout_fh = out_fh
        self.started_at = time.monotonic()
        self._stopped = False
        try:
            self.process = subprocess.Popen(
                [str(self.java), "-jar", str(self.jar), "nogui"],
                cwd=str(self.server_dir),
                stdout=out_fh,
                stderr=subprocess.STDOUT,
            )
            _ACTIVE_SERVERS.add(self)
            self._wait_ready()
            self._connect_rcon()
        except BaseException:
            # Never leave a half-started Java process behind, but keep the
            # directory so the failure can be inspected.
            self._terminate_process()
            self._close_stdout()
            _ACTIVE_SERVERS.discard(self)
            raise
        return self

    def _connect_rcon(self) -> None:
        self.rcon = RconClient(
            host="127.0.0.1", port=self.rcon_port,
            password=DEFAULT_RCON_PASSWORD, timeout=10.0,
        )
        # Connect with retries; the RCON listener may lag behind the log line.
        last_exc: Optional[BaseException] = None
        for _ in range(10):
            try:
                self.rcon.connect()
                self.rcon.command("list")
                return
            except Exception as exc:
                last_exc = exc
                self.rcon.close()
                time.sleep(1.0)
        assert last_exc is not None
        raise last_exc

    def _wait_ready(self) -> None:
        deadline = time.monotonic() + READY_TIMEOUT
        while time.monotonic() < deadline:
            if self.process is not None and self.process.poll() is not None:
                raise RuntimeError(
                    f"server process exited early (code {self.process.returncode}); "
                    f"see {self.stdout_path}"
                )
            combined = self._read_logs()
            if self.done_at is None and "Done (" in combined:
                self.done_at = time.monotonic()
                logger.debug("server %s done after %.2fs", self.seed, self.startup_seconds)
            if self.done_at is not None and "RCON running on" in combined:
                # Give the RCON listener a moment to finish binding.
                time.sleep(1.0)
                self.ready_at = time.monotonic()
                return
            time.sleep(0.25)
        raise TimeoutError(
            f"server did not become ready within {READY_TIMEOUT}s; "
            f"done={self.done_at is not None}; see {self.stdout_path}"
        )

    def _read_logs(self) -> str:
        texts = []
        for p in (self.log_path, self.stdout_path):
            if p.exists():
                try:
                    texts.append(p.read_text(encoding="utf-8", errors="replace"))
                except OSError:
                    pass
        return "\n".join(texts)

    # ------------------------------------------------------------------ #
    def stop(self) -> None:
        """Stop gracefully and remove the run directory unless ``keep`` is set."""
        if self._stopped:
            return
        self._stopped = True
        # Graceful: RCON stop.
        if self.rcon is not None:
            try:
                self.rcon.command("stop")
            except Exception as exc:
                logger.debug("rcon stop failed: %s", exc)
            try:
                self.rcon.close()
            except Exception:
                pass
            self.rcon = None

        self._terminate_process()
        self._close_stdout()
        _ACTIVE_SERVERS.discard(self)
        self._remove_run_dir()

    def _terminate_process(self) -> None:
        proc = self.process
        self.process = None
        if proc is None:
            return
        try:
            proc.wait(timeout=STOP_TIMEOUT)
            return
        except subprocess.TimeoutExpired:
            pass
        # Kill the process tree on Windows, else fall back to SIGKILL.
        try:
            subprocess.run(
                ["taskkill", "/PID", str(proc.pid), "/T", "/F"],
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            )
        except Exception:
            try:
                proc.kill()
            except Exception:
                pass
        try:
            proc.wait(timeout=10)
        except Exception:
            pass

    def _close_stdout(self) -> None:
        fh = self._stdout_fh
        self._stdout_fh = None
        if fh is not None:
            try:
                fh.close()
            except Exception:
                pass

    def _remove_run_dir(self) -> None:
        if self.keep:
            return
        target = self.run_dir.resolve()
        if (
            target == Path(target.anchor)
            or target == Path(target.anchor) / target.name
            or target in {repo_root().resolve(), (repo_root() / "runs").resolve()}
        ):
            logger.warning("refusing to delete %s", target)
            return
        for attempt in range(3):
            try:
                shutil.rmtree(target)
                return
            except FileNotFoundError:
                return
            except OSError as exc:
                logger.debug("could not remove %s (attempt %d): %s", target, attempt + 1, exc)
                time.sleep(0.2)

    def __enter__(self) -> "TestServer":
        return self.start()

    def __exit__(self, *exc) -> None:
        self.stop()


__all__ = [
    "TestServer",
    "active_servers",
    "stop_active_servers",
    "keep_runs_by_default",
    "FORBIDDEN_PORTS",
]

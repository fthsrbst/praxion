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

Only ports 25640-25659 are used for the forwarder's game and tee listeners and
for the body port in a brain session. The server keeps its own random port from
``TestServer``.

``start_session`` (proposal §2.6) starts the same stack with ``--port`` and no
``--goal``: the body listens for the brain and runs goal after goal in one
process. Its ``BodySession`` adds the port number to the usual handles.

The stack can also be split. ``start_server`` starts only :class:`TestServer`
plus its referee and one-time setup; ``start_session(..., server=that)`` then
starts only the forwarder and the body against the already-running server, in a
fresh attempt directory under the server's run directory. A borrowed server is
never stopped by the body's cleanup, so a bot crash or a cancelled start leaves
the world alive for the next attempt and the console can restart just the bot.
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


class StartAborted(RuntimeError):
    """The caller asked to stop before the next startup stage was launched.

    ``start_session`` raises it from a ``should_stop`` callback so the owner of
    the session can tear down a half-started stack (server, forwarder, body)
    instead of letting it come up while nobody wants it.
    """


def _should_stop(should_stop: Optional[Callable[[], bool]]) -> bool:
    return should_stop is not None and bool(should_stop())


def _check_abort(should_stop: Optional[Callable[[], bool]]) -> None:
    if _should_stop(should_stop):
        raise StartAborted("startup stopped by the caller")


def _binary(name: str) -> Path:
    suffix = ".exe" if os.name == "nt" else ""
    return repo_root() / "body" / "target" / "release" / f"{name}{suffix}"


def active_skills_dir() -> Path:
    """The admitted-skills directory: the brain writes it, the body watches it.

    ``PRAXION_SKILLS_ACTIVE`` is the one override this module and
    ``brain/praxion_brain/config.py`` both read, so a custom directory cannot
    detach the body's watch from where admission writes. A relative path is
    resolved against the repository root.
    """
    value = os.environ.get("PRAXION_SKILLS_ACTIVE")
    if value:
        path = Path(value).expanduser()
        return path if path.is_absolute() else repo_root() / path
    return repo_root() / "local" / "skills" / "active"


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


class ServerSession:
    """An independently owned server with its referee and one-time setup.

    ``start_server`` returns one. Its ``run_dir`` names the managed world (the
    brain uses that name as ``world_id``), its ``port`` is the game port Join
    uses, and one :class:`BodySession` may borrow it at a time. Stopping the
    owner stops Java; a borrowed body never does.
    """

    def __init__(
        self,
        run_dir: Path,
        server: TestServer,
        referee: Referee,
        fixture: FixtureChannel,
        *,
        keep: bool,
        owns_server: bool = True,
    ) -> None:
        self.run_dir = Path(run_dir)
        self.server = server
        self.referee = referee
        self.fixture = fixture
        self.keep = keep
        self.owns_server = owns_server
        self._stopped = False

    @property
    def port(self) -> int:
        return int(self.server.port)

    def stop(self) -> None:
        """Stop the server only when this session owns it; borrowed ones stay up."""
        if self._stopped:
            return
        self._stopped = True
        if not self.owns_server:
            return
        if self.keep:
            self.server.keep = True
        self.server.stop()


class BodyRun:
    """A started body and forwarder, and how to read what it did.

    ``owns_server`` decides whether :meth:`stop` also stops the :class:`TestServer`:
    a run started by ``start_body``/``start_session`` owns its server, while a
    body borrowed onto a ``ServerSession`` only tears down its own two processes.
    """

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
        skills_dir: Optional[Path] = None,
        owns_server: bool = True,
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
        self.owns_server = owns_server
        #: The directory a port session watches for newly admitted skills.
        self.skills_dir = Path(skills_dir) if skills_dir is not None else None
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
        if not self.owns_server:
            return
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


class BodySession(BodyRun):
    """A body in port mode: one process, many goals, the brain in charge.

    ``port`` is the loopback port the brain connects to with
    :class:`praxion_brain.port.PortClient`. ``run_dir``, ``referee`` and
    ``fixture`` are the same as for :class:`BodyRun`; ``wait_for_goal`` waits
    for one command's ``settled`` receipt and ``summary()["goals"]`` lists every
    settled goal (proposal §2.6). ``wait_for_outcome`` stays for a CLI run: on
    a session it could return the previous goal's summary before the new
    command is even accepted.
    """

    def __init__(self, *args, port: int, **kwargs) -> None:
        super().__init__(*args, **kwargs)
        self.port = int(port)

    def wait_for_goal(self, command_id: str, timeout: float = 240.0) -> Dict[str, Any]:
        """Wait for ``command_id``'s ``settled`` receipt and return that event.

        The receipt is the one line that says this command is over, so a
        session never mistakes the previous goal's summary for the new one.
        """
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            for event in self.events():
                if (
                    event.get("kind") == "command_receipt"
                    and event.get("command_id") == command_id
                    and event.get("state") == "settled"
                ):
                    return event
            if self.process.poll() is not None:
                raise RuntimeError(
                    f"the body exited (code {self.process.returncode}) before "
                    f"{command_id!r} settled ({self.output_path}):\n"
                    f"{self.output()[-4000:]}"
                )
            time.sleep(0.1)
        raise TimeoutError(
            f"{command_id!r} did not settle within {timeout:.0f}s "
            f"({self.output_path}):\n{self.output()[-4000:]}"
        )


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


def _wait_for(
    path: Path,
    needle: str,
    process: subprocess.Popen,
    timeout: float,
    *,
    should_stop: Optional[Callable[[], bool]] = None,
) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        _check_abort(should_stop)
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
    return _start(
        seed,
        goal,
        goal_args,
        port_mode=False,
        setup=setup,
        max_ticks=max_ticks,
        start_delay_ticks=start_delay_ticks,
        linger_ticks=linger_ticks,
        settings=settings,
        name=name,
        run_dir=run_dir,
        keep=keep,
        extra_args=extra_args,
    )


def start_server(
    seed: int,
    *,
    setup: Optional[Setup] = None,
    name: str = "Praxion",
    run_dir: Optional[Path] = None,
    keep: Optional[bool] = None,
    should_stop: Optional[Callable[[], bool]] = None,
) -> ServerSession:
    """Start one independently owned server and run ``setup`` once.

    Blocks until Java and RCON are ready and the setup callback returned, so the
    returned game ``port`` is joinable immediately. Nothing else is running yet:
    a caller may expose Join before it starts a body.

    ``setup`` receives the server's referee and fixture, with ``None`` as the
    run argument (the same signature ``start_session`` uses). It runs here and
    only here, once per server: a body borrowed later must not reset the clock,
    the mob spawning or anything else the world already has.

    A failed start stops the half-started server; a caller that stops during
    startup gets :class:`StartAborted` and no server is left behind.
    """
    _check_abort(should_stop)
    run_dir = Path(run_dir) if run_dir is not None else (
        repo_root() / "runs" / f"server-{uuid.uuid4().hex}"
    )
    run_dir.mkdir(parents=True, exist_ok=True)
    server = TestServer(run_dir, seed, keep=keep)
    try:
        server.start()
        _check_abort(should_stop)
        referee = Referee(server.rcon, player=name)
        referee.setup_death_objective()
        fixture = FixtureChannel(server.rcon, fixture_log_path(run_dir))
        _check_abort(should_stop)
        if setup is not None:
            setup(fixture, referee, None)  # type: ignore[arg-type]
        _check_abort(should_stop)
        return ServerSession(
            run_dir,
            server,
            referee,
            fixture,
            keep=server.keep,
            owns_server=True,
        )
    except BaseException:
        server.stop()
        raise


def start_session(
    seed: int,
    *,
    setup: Optional[Setup] = None,
    max_ticks: int = 60000,
    start_delay_ticks: int = 0,
    settings: Optional[Path] = None,
    name: str = "Praxion",
    run_dir: Optional[Path] = None,
    keep: Optional[bool] = None,
    extra_args: Iterable[str] = (),
    should_stop: Optional[Callable[[], bool]] = None,
    server: Optional[ServerSession] = None,
) -> BodySession:
    """Start a body in port mode: it runs goal after goal for the brain.

    The body gets ``--port`` and no ``--goal``; the brain connects to
    :attr:`BodySession.port` and sends commands (proposal §2.6). Live case::

        with start_session(101, setup=give_logs) as session:
            with PortClient(session.port) as client:
                client.connect()
                ...

    The body watches :func:`active_skills_dir` (``PRAXION_SKILLS_ACTIVE``,
    default ``local/skills/active``) as ``--candidates`` and reloads its
    program when a file there changes, so a skill the brain admits during the
    run is playable by the next command. The directory is created when missing.

    ``max_ticks`` is only the process-wide safety cap, not a goal budget.

    ``should_stop`` is polled between startup stages (server, forwarder, body)
    and while waiting for the body's ``joined`` line; when it returns true the
    call raises :class:`StartAborted` after tearing the half-started stack down.
    It lets an owner stop a startup without waiting for a 90 s join timeout.

    With ``server`` set, this is a borrowed run: the server from
    :func:`start_server` is reused as-is, only the forwarder and body start, and
    neither :meth:`BodySession.stop` nor a failed start ever stops the server.
    The body runs in ``run_dir`` when given, else in a unique attempt directory
    under the server's run directory. ``setup`` is refused for a borrowed
    server: its world setup already ran once in :func:`start_server`.
    """
    if server is not None and setup is not None:
        raise ValueError(
            "a borrowed server already ran its setup; pass setup to start_server"
        )
    return _start(
        seed,
        None,
        (),
        port_mode=True,
        setup=setup,
        max_ticks=max_ticks,
        start_delay_ticks=start_delay_ticks,
        linger_ticks=0,
        settings=settings,
        name=name,
        run_dir=run_dir,
        keep=keep,
        extra_args=extra_args,
        should_stop=should_stop,
        server=server,
    )


def _start(
    seed: int,
    goal: Optional[str],
    goal_args: Iterable[str],
    *,
    port_mode: bool,
    setup: Optional[Setup],
    max_ticks: int,
    start_delay_ticks: int,
    linger_ticks: int,
    settings: Optional[Path],
    name: str,
    run_dir: Optional[Path],
    keep: Optional[bool],
    extra_args: Iterable[str],
    should_stop: Optional[Callable[[], bool]] = None,
    server: Optional[ServerSession] = None,
) -> BodySession:
    _check_abort(should_stop)
    borrowed = server is not None
    if borrowed and setup is not None:
        raise ValueError(
            "a borrowed server already ran its setup; pass setup to start_server"
        )
    for binary in ("praxion-body", "praxion-forwarder"):
        path = _binary(binary)
        if not path.exists():
            raise RuntimeError(
                f"{binary} is not built ({path}); run: cd body && cargo build --release"
            )

    # The brain's admitted skills live here; the body watches it and reloads,
    # so a skill written mid-run needs no restart. Both sides read
    # `active_skills_dir()`, so an override cannot point the watch elsewhere.
    skills_dir = active_skills_dir() if port_mode else None
    if skills_dir is not None:
        skills_dir.mkdir(parents=True, exist_ok=True)

    if borrowed:
        # A unique attempt directory per bot, under the one managed world, so
        # a restart never overwrites the previous attempt's logs and the
        # server's own files stay untouched.
        default_run_dir = server.run_dir / "bots" / f"attempt-{uuid.uuid4().hex}"
    else:
        default_run_dir = repo_root() / "runs" / f"test-body-{uuid.uuid4().hex}"
    run_dir = Path(run_dir) if run_dir is not None else default_run_dir
    run_dir.mkdir(parents=True, exist_ok=True)
    server_obj: Optional[TestServer] = None
    forwarder: Optional[subprocess.Popen] = None
    body: Optional[subprocess.Popen] = None
    body_out = None
    forwarder_out = None
    try:
        if borrowed:
            referee = server.referee
            fixture = server.fixture
            server_obj = server.server
        else:
            server_obj = TestServer(run_dir, seed, keep=keep)
            server_obj.start()
            _check_abort(should_stop)
            referee = Referee(server_obj.rcon, player=name)
            referee.setup_death_objective()
            fixture = FixtureChannel(server_obj.rcon, fixture_log_path(run_dir))

        assert server_obj is not None
        ports = _free_ports(3 if port_mode else 2)
        game_port, tee_port = ports[0], ports[1]
        body_port = ports[2] if port_mode else 0
        _check_abort(should_stop)
        forwarder_out_path = run_dir / "forwarder.out"
        forwarder_out = forwarder_out_path.open("w", encoding="utf-8", errors="replace")
        forwarder = subprocess.Popen(
            [
                str(_binary("praxion-forwarder")),
                "--listen", f"127.0.0.1:{game_port}",
                "--upstream", f"127.0.0.1:{server_obj.port}",
                "--tee-listen", f"127.0.0.1:{tee_port}",
                "--stats", str(run_dir / "forwarder.jsonl"),
            ],
            cwd=str(repo_root()),
            stdout=forwarder_out,
            stderr=subprocess.STDOUT,
        )
        _wait_for(
            forwarder_out_path,
            "tee on",
            forwarder,
            FORWARDER_READY_TIMEOUT,
            should_stop=should_stop,
        )
        _check_abort(should_stop)

        body_dir = run_dir / "body"
        body_out_path = run_dir / "body.out"
        body_out = body_out_path.open("w", encoding="utf-8", errors="replace")
        command = [
            str(_binary("praxion-body")),
            "--forwarder", f"127.0.0.1:{game_port}",
            "--tee", f"127.0.0.1:{tee_port}",
            "--name", name,
            "--run-dir", str(body_dir),
            "--max-ticks", str(max_ticks),
            "--start-delay-ticks", str(start_delay_ticks),
            "--linger-ticks", str(linger_ticks),
        ]
        if goal is not None:
            command.extend(["--goal", goal])
        if port_mode:
            command.extend(["--port", f"127.0.0.1:{body_port}"])
            command.extend(["--candidates", str(skills_dir)])
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
        _wait_for(
            body_out_path,
            "joined",
            body,
            BODY_JOIN_TIMEOUT,
            should_stop=should_stop,
        )
        _check_abort(should_stop)

        run = BodySession(
            run_dir,
            server_obj,
            forwarder,
            forwarder_out_path,
            body,
            body_out_path,
            referee,
            fixture,
            keep=server_obj.keep if not borrowed else True,
            port=body_port,
            skills_dir=skills_dir,
            owns_server=not borrowed,
        )
        run._out = body_out
        run._forwarder_out = forwarder_out
        _check_abort(should_stop)
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
        # Only an owned server is cleaned up here. A borrowed server belongs to
        # its owner and must survive a cancelled or failed bot start.
        if not borrowed and server_obj is not None:
            server_obj.stop()
        raise


__all__ = [
    "BODY_PORT_RANGE",
    "BodyRun",
    "BodySession",
    "ServerSession",
    "StartAborted",
    "active_skills_dir",
    "start_body",
    "start_server",
    "start_session",
]

"""The console's owned session: a Minecraft server and a bot on top of it.

``python -m praxion_brain console`` opens the one-page console and waits. The
page sends fixed lifecycle actions; this module owns everything behind them:

* **Start server** starts one harness :func:`~praxion_harness.body.start_server`
  (``TestServer``) with its world setup. The server alone is joinable, so Join
  works before any bot exists.
* **Start bot** starts only a forwarder and body against that server through
  ``start_session(..., server=...)``, then builds a fresh
  :class:`~praxion_brain.loop.Loop` and :class:`~.hub.Console`. A bot restart
  reuses the same server world and its brain memory, in a new attempt directory
  and with a fresh console, so no snapshot or queued request survives.
* **Stop bot** tears the body, loop and console down and keeps the server and
  the world running for the next bot.
* **Stop server** stops the bot first, then the server, and leaves the HTTP
  console serving an idle page.
* **Join** launches the machine-local Minecraft client on the server's game
  port as the configured operator. It never kills the client.

Request threads only record a start or a stop; one worker per resource owns
every process and every loop call. A startup error of either side leaves the
other side as it was: a failed bot start or crash keeps a healthy server
available for retry, and a failed server start leaves no half-started stack.
"""

from __future__ import annotations

import threading
import time
import uuid
from dataclasses import dataclass
from enum import Enum
from pathlib import Path
from typing import Any, Callable, Optional

from .. import config, needs
from ..evidence import Evidence
from ..governor import Governor
from ..ledger import Ledger
from ..loop import Loop
from ..memory import Memory
from ..port import PortClient
from ..skills import SkillCatalog, load_catalog
from .hub import Console

__all__ = [
    "JoinError",
    "Phase",
    "Runtime",
    "SessionManager",
    "StartAborted",
    "build_runtime",
]

#: How long bot readiness may take after ``Loop.start`` before the start fails.
READY_TIMEOUT_SECONDS = 60.0
#: A loop step blocks at most this long, so Stop is noticed promptly.
STEP_TIMEOUT_SECONDS = 0.1
#: How often a live server checks that Java is still alive.
SERVER_POLL_SECONDS = 0.25
#: Bound on waiting for the bot's own cleanup before the server stops.
BOT_CLEANUP_SECONDS = 120.0
#: The only host a managed session or a Join uses.
LOOPBACK = "127.0.0.1"

_UNSET = object()


class Phase(str, Enum):
    """One resource's lifecycle phase; the server and the bot each have one."""

    IDLE = "idle"
    STARTING = "starting"
    RUNNING = "running"
    STOPPING = "stopping"
    FAILED = "failed"


class StartAborted(RuntimeError):
    """Stop arrived before the next startup stage; nothing went wrong."""


class JoinError(RuntimeError):
    """Join cannot run now, or the client launcher failed."""


@dataclass
class Runtime:
    """Everything one loop needs, built the same way by ``run`` and the console."""

    evidence: Evidence
    ledger: Ledger
    memory: Memory
    catalog: SkillCatalog
    gamedata: Any
    governor: Governor
    console: Console
    loop: Loop


def build_runtime(
    *,
    port: int,
    world_id: str,
    host: str = LOOPBACK,
    evidence: Optional[Evidence] = None,
    ledger: Optional[Ledger] = None,
    memory: Optional[Memory] = None,
    memory_root: Optional[Path] = None,
    catalog: Optional[SkillCatalog] = None,
    console: Optional[Console] = None,
    core_dir: Optional[str] = None,
    active_dir: Optional[str] = None,
    gamedata: Any = _UNSET,
) -> Runtime:
    """Build one console + loop over one body port.

    The CLI's ``run`` command and the managed console share this factory, so a
    managed run is the same brain, skills, memory, governor and console the
    command line uses. A caller may pass a fresh ``memory`` or ``console``; by
    default both are new for ``world_id``.
    """
    evidence = evidence if evidence is not None else Evidence(config.local_brain_dir())
    ledger = ledger if ledger is not None else Ledger()
    memory = (
        memory
        if memory is not None
        else Memory(world_id, root=memory_root, evidence=evidence)
    )
    catalog = (
        catalog
        if catalog is not None
        else load_catalog(
            Path(core_dir) if core_dir is not None else None,
            Path(active_dir) if active_dir is not None else None,
        )
    )
    if gamedata is _UNSET:
        gamedata = needs.load()
    governor = Governor(ledger, evidence=evidence)
    console = (
        console
        if console is not None
        else Console(
            evidence=evidence,
            ledger=ledger,
            memory=memory,
            world_id=world_id,
            core_dir=core_dir,
            active_dir=active_dir,
        )
    )
    loop = Loop(
        PortClient(port, host=host),
        world_id=world_id,
        memory=memory,
        catalog=catalog,
        ledger=ledger,
        evidence=evidence,
        governor=governor,
        console=console,
        gamedata=gamedata,
    )
    return Runtime(
        evidence=evidence,
        ledger=ledger,
        memory=memory,
        catalog=catalog,
        gamedata=gamedata,
        governor=governor,
        console=console,
        loop=loop,
    )


class SessionManager:
    """Owns at most one managed server, one bot on it, and the client Join launched."""

    def __init__(
        self,
        *,
        seed: int = 303,
        operator: str = "Fatih",
        peaceful: bool = False,
        start_time: int = 0,
        evidence: Optional[Evidence] = None,
        ledger: Optional[Ledger] = None,
        memory_root: Optional[Path] = None,
        ready_timeout: float = READY_TIMEOUT_SECONDS,
        server_start_fn: Optional[Callable[[Callable[[], bool]], Any]] = None,
        bot_start_fn: Optional[Callable[[Any, Callable[[], bool]], Any]] = None,
        start_session_fn: Optional[Callable[[Any, Callable[[], bool]], Any]] = None,
        runtime_factory: Optional[Callable[[int, str], Runtime]] = None,
        client_launch: Optional[Callable[..., Any]] = None,
        desync_factory: Optional[Callable[[Path], Any]] = None,
        clock: Callable[[], float] = time.monotonic,
        logger: Optional[Callable[[str], None]] = None,
    ) -> None:
        self.seed = int(seed)
        self.operator = str(operator)
        self.peaceful = bool(peaceful)
        self.start_time = int(start_time)
        self.evidence = evidence if evidence is not None else Evidence(config.local_brain_dir())
        self.ledger = ledger if ledger is not None else Ledger()
        self.memory_root = memory_root
        self.ready_timeout = float(ready_timeout)
        self._server_start_fn = server_start_fn
        # ``start_session_fn`` is the old name of the bot seam; keep it working.
        self._bot_start_fn = bot_start_fn or start_session_fn
        self._runtime_factory = runtime_factory
        self._client_launch = client_launch
        self._desync_factory = desync_factory
        self._clock = clock
        self._log = logger if logger is not None else print

        self._lock = threading.RLock()
        # Shutdown is permanent: a start that races the teardown is refused.
        self._closing = False

        # The server has its own worker, stop event and phase; the bot likewise.
        self._server_phase = Phase.IDLE
        self._server_stop = threading.Event()
        self._server_thread: Optional[threading.Thread] = None
        self._server_generation = 0
        self._server_session: Any = None
        self._server_world: Optional[str] = None
        self._server_error: Optional[str] = None

        self._bot_phase = Phase.IDLE
        self._bot_stop = threading.Event()
        self._bot_thread: Optional[threading.Thread] = None
        self._bot_generation = 0
        self._bot_session: Any = None
        self._bot_world: Optional[str] = None
        self._bot_ready = False
        self._bot_error: Optional[str] = None
        self._loop: Any = None
        self._console: Optional[Console] = None

        # One Join at a time, so two requests cannot spawn two clients.
        self._joining = False
        self._server: Any = None
        self._client: Any = None

    # -- what the page reads -------------------------------------------- #
    @property
    def phase(self) -> Phase:
        """The bot phase; kept as the short name existing callers use."""
        with self._lock:
            return self._bot_phase

    @property
    def server_phase(self) -> Phase:
        with self._lock:
            return self._server_phase

    def bot_live(self) -> bool:
        """Whether goal controls may reach a loop right now."""
        with self._lock:
            return self._bot_phase is Phase.RUNNING and self._bot_ready

    def status(self) -> dict[str, Any]:
        """The lifecycle view for ``/api/state``; safe to call from any thread."""
        with self._lock:
            client_running = False
            client_pid = None
            if self._client is not None:
                if self._client.poll() is None:
                    client_running = True
                    client_pid = getattr(self._client, "pid", None)
                else:
                    # Let Join retry after the launched client exits.
                    self._client = None

            server_phase = self._server_phase
            bot_phase = self._bot_phase
            server_session = self._server_session
            server_ready = server_phase is Phase.RUNNING and server_session is not None
            address = f"{LOOPBACK}:{int(server_session.port)}" if server_ready else None
            bot_ready = bot_phase is Phase.RUNNING and self._bot_ready

            server_thread = self._server_thread
            bot_thread = self._bot_thread
            server_busy = server_thread is not None and server_thread.is_alive()
            bot_busy = bot_thread is not None and bot_thread.is_alive()
            server_active = server_phase in (Phase.STARTING, Phase.RUNNING, Phase.STOPPING)
            bot_active = bot_phase in (Phase.STARTING, Phase.RUNNING, Phase.STOPPING)

            can_start_server = (
                not self._closing
                and server_phase in (Phase.IDLE, Phase.FAILED)
                and not server_busy
                and not bot_active
                and not bot_busy
                and server_session is None
            )
            can_stop_server = not self._closing and (
                server_active
                or bot_active
                or server_session is not None
                or self._bot_session is not None
            )
            can_start_bot = (
                not self._closing
                and server_ready
                and bot_phase in (Phase.IDLE, Phase.FAILED)
                and not bot_busy
                and self._bot_session is None
            )
            can_stop_bot = not self._closing and bot_active
            can_join = (
                not self._closing
                and server_ready
                and not client_running
                and not self._joining
            )

            return {
                "managed": True,
                # Legacy short fields keep the bot as the subject.
                "phase": bot_phase.value,
                "ready": bot_ready,
                "address": address,
                "error": self._bot_error or self._server_error,
                "seed": self.seed,
                "operator": self.operator,
                "peaceful": self.peaceful,
                "client": {"running": client_running, "pid": client_pid},
                "server": {
                    "phase": server_phase.value,
                    "ready": server_ready,
                    "address": address,
                    "world_id": self._server_world if server_ready else None,
                    "error": self._server_error,
                    "can": {"start": can_start_server, "stop": can_stop_server},
                },
                "bot": {
                    "phase": bot_phase.value,
                    "ready": bot_ready,
                    "world_id": self._bot_world if bot_ready else None,
                    "error": self._bot_error,
                    "can": {"start": can_start_bot, "stop": can_stop_bot},
                },
                "can": {
                    "start_server": can_start_server,
                    "stop_server": can_stop_server,
                    "start_bot": can_start_bot,
                    "stop_bot": can_stop_bot,
                    "join": can_join,
                },
            }

    # -- wiring --------------------------------------------------------- #
    def attach_server(self, server: Any) -> None:
        """Give the manager the HTTP server whose console it swaps."""
        with self._lock:
            self._server = server
            console = self._console
        server.set_console(console)

    def _install_console(self, console: Optional[Console]) -> None:
        with self._lock:
            self._console = console
            server = self._server
        if server is not None:
            server.set_console(console)

    # -- controls ------------------------------------------------------- #
    def start_server(self) -> bool:
        """Request one server start; ``False`` when the slot is busy or occupied."""
        with self._lock:
            if self._closing:
                return False
            if self._server_phase in (Phase.STARTING, Phase.RUNNING, Phase.STOPPING):
                return False
            if self._bot_phase in (Phase.STARTING, Phase.RUNNING, Phase.STOPPING):
                return False
            if self._server_session is not None or self._bot_session is not None:
                return False
            thread = self._server_thread
            if thread is not None and thread.is_alive():
                return False
            self._server_stop.clear()
            self._server_error = None
            self._server_world = None
            self._server_phase = Phase.STARTING
            self._server_generation += 1
            generation = self._server_generation
            thread = threading.Thread(
                target=self._server_work,
                args=(generation,),
                name="console-server",
                daemon=True,
            )
            self._server_thread = thread
            try:
                self.evidence.write(
                    "session_server_start_requested",
                    seed=self.seed,
                    operator=self.operator,
                    peaceful=self.peaceful,
                )
                thread.start()
            except Exception as error:
                self._server_thread = None
                self._server_generation += 1
                self._server_phase = Phase.FAILED
                self._server_error = f"the server worker did not start: {_describe(error)}"
                failure = self._server_error
            else:
                failure = None
        if failure is None:
            return True
        self._log(f"server: {failure}")
        self._evidence_error("session_server_error", failure, Phase.FAILED)
        return False

    def start_bot(self) -> bool:
        """Request one bot start on the ready server; ``False`` when it cannot."""
        with self._lock:
            if self._closing:
                return False
            if self._server_phase is not Phase.RUNNING or self._server_session is None:
                return False
            if self._bot_phase in (Phase.STARTING, Phase.RUNNING, Phase.STOPPING):
                return False
            if self._bot_session is not None:
                return False
            thread = self._bot_thread
            if thread is not None and thread.is_alive():
                return False
            self._bot_stop.clear()
            self._bot_error = None
            self._bot_ready = False
            self._bot_world = None
            self._bot_phase = Phase.STARTING
            self._bot_generation += 1
            generation = self._bot_generation
            thread = threading.Thread(
                target=self._bot_work,
                args=(generation,),
                name="console-bot",
                daemon=True,
            )
            self._bot_thread = thread
            try:
                self.evidence.write(
                    "session_bot_start_requested",
                    seed=self.seed,
                    operator=self.operator,
                    world_id=self._server_world,
                )
                thread.start()
            except Exception as error:
                self._bot_thread = None
                self._bot_generation += 1
                self._bot_phase = Phase.FAILED
                self._bot_error = f"the bot worker did not start: {_describe(error)}"
                failure = self._bot_error
            else:
                failure = None
        if failure is None:
            return True
        self._log(f"bot: {failure}")
        self._evidence_error("session_bot_error", failure, Phase.FAILED)
        return False

    def request_stop_bot(self) -> bool:
        """Ask the bot worker to tear down the bot; the server stays up."""
        with self._lock:
            if self._bot_phase in (Phase.STARTING, Phase.RUNNING):
                previous = self._bot_phase
                self._bot_stop.set()
                self._bot_phase = Phase.STOPPING
                self._bot_ready = False
                self.evidence.write("session_bot_stop_requested", phase=previous.value)
                return True
            if self._bot_phase is Phase.STOPPING:
                self._bot_stop.set()
                return True
            return False

    def request_stop_server(self) -> bool:
        """Stop the server and any bot on it; returns whether anything existed."""
        with self._lock:
            server_active = self._server_phase in (
                Phase.STARTING,
                Phase.RUNNING,
                Phase.STOPPING,
            )
            bot_active = self._bot_phase in (
                Phase.STARTING,
                Phase.RUNNING,
                Phase.STOPPING,
            )
            if not (
                server_active
                or bot_active
                or self._server_session is not None
                or self._bot_session is not None
            ):
                return False
            previous_bot = self._bot_phase
            previous_server = self._server_phase
            # The bot stops first; the server worker waits for it before Java.
            if self._bot_phase in (Phase.STARTING, Phase.RUNNING):
                self._bot_stop.set()
                self._bot_phase = Phase.STOPPING
                self._bot_ready = False
            elif self._bot_phase is Phase.STOPPING:
                self._bot_stop.set()
            if self._server_phase in (Phase.STARTING, Phase.RUNNING):
                self._server_stop.set()
                self._server_phase = Phase.STOPPING
            elif self._server_phase is Phase.STOPPING:
                self._server_stop.set()
            self.evidence.write(
                "session_server_stop_requested",
                server_phase=previous_server.value,
                bot_phase=previous_bot.value,
            )
            return True

    def join(self) -> dict[str, Any]:
        """Launch the local client once on the ready server's game port."""
        with self._lock:
            if self._joining:
                raise JoinError("a Join is already in progress; wait for it to finish")
            if self._server_phase is not Phase.RUNNING or self._server_session is None:
                raise JoinError("the server is not ready; start the server first")
            if self._client is not None:
                if self._client.poll() is None:
                    pid = getattr(self._client, "pid", "?")
                    raise JoinError(f"a Minecraft client is already running (pid {pid})")
                self._client = None
            try:
                port = int(self._server_session.port)
            except (TypeError, ValueError):
                raise JoinError("the server has no game port yet") from None
            # Reserve the single Join, then do the slow part (the Java version
            # check and the Quick Play command) without the lifecycle lock, so
            # status, Stop and a second Join are never held up by it.
            self._joining = True
            generation = self._server_generation
            session = self._server_session
            address = f"{LOOPBACK}:{port}"
        try:
            process = self._launch_client(port, session, generation)
            # Publish while the reservation is still held: no Join may slip in
            # between the spawn and the recorded client.
            with self._lock:
                self._client = process
                pid = getattr(process, "pid", None)
        finally:
            with self._lock:
                self._joining = False
        self.evidence.write("console_join", address=address, pid=pid, operator=self.operator)
        return {"running": True, "pid": pid, "address": address}

    def shutdown(self) -> None:
        """Stop the server and bot and wait for both workers' cleanup.

        The server worker owns the server stop and waits for the bot worker, so
        joining the bot first and the server second leaves no child behind.
        """
        with self._lock:
            # Permanent: from here no new worker may be prepared or started.
            self._closing = True
        self.request_stop_server()
        with self._lock:
            bot_thread = self._bot_thread
            server_thread = self._server_thread
        for thread in (bot_thread, server_thread):
            if thread is None or thread is threading.current_thread():
                continue
            if thread.ident is None:
                # start() stores and starts under the lifecycle lock, so this
                # can only be a worker that never ran and owns no process.
                continue
            thread.join()

    # -- the server worker ---------------------------------------------- #
    def _server_work(self, generation: int) -> None:
        session: Any = None
        failure: Optional[str] = None
        try:
            if self._server_stop.is_set() or generation != self._server_generation:
                raise StartAborted()
            session = self._start_server_session()
            if self._server_stop.is_set() or generation != self._server_generation:
                raise StartAborted()
            world_id = Path(session.run_dir).name
            with self._lock:
                if (
                    self._server_stop.is_set()
                    or self._closing
                    or generation != self._server_generation
                ):
                    # Stop won the race for the slot: never publish a ready
                    # server the page could then race with teardown.
                    raise StartAborted()
                self._server_session = session
                self._server_world = world_id
                self._server_phase = Phase.RUNNING
                address = f"{LOOPBACK}:{int(session.port)}"
            self.evidence.write(
                "session_server_ready",
                address=address,
                world_id=world_id,
                run_dir=str(session.run_dir),
            )
            self._watch_server(session, generation)
        except StartAborted:
            pass
        except Exception as error:  # every startup/runtime failure reaches the page
            failure = _describe(error)
            self._evidence_error("session_server_error", failure, self.server_phase)
            self._log(f"server: {failure}")
        finally:
            self._finish_server(generation, session, failure)

    def _watch_server(self, session: Any, generation: int) -> None:
        """Hold the server's lifetime until Stop, and notice a dead Java process."""
        while not self._server_stop.is_set():
            if generation != self._server_generation:
                return
            process = getattr(getattr(session, "server", None), "process", None)
            if process is not None:
                code = process.poll()
                if code is not None:
                    raise RuntimeError(
                        f"the Minecraft server exited unexpectedly (code {code})"
                    )
            self._server_stop.wait(SERVER_POLL_SECONDS)

    def _finish_server(
        self, generation: int, session: Any, failure: Optional[str]
    ) -> None:
        with self._lock:
            if generation == self._server_generation:
                self._server_session = None
                self._server_world = None
                self._server_phase = (
                    Phase.FAILED if failure is not None else Phase.STOPPING
                )
                if failure is not None:
                    self._server_error = failure
            if failure is not None:
                # A dead server takes its world with it: the bot cannot stay.
                self._bot_stop.set()
                if self._bot_phase in (Phase.STARTING, Phase.RUNNING):
                    self._bot_phase = Phase.STOPPING
                    self._bot_ready = False
        # The bot owns its processes; the server waits for that cleanup before
        # stopping Java, so no body is left talking to a closed server.
        self._join_bot_cleanup()
        problems: list[str] = []
        try:
            if session is not None:
                session.stop()
        except Exception as error:  # cleanup still has to report what broke
            problems.append(f"server cleanup failed: {_describe(error)}")
        if problems:
            self.evidence.write("session_server_cleanup_error", problems=problems)
            if failure is None:
                failure = "; ".join(problems)
        with self._lock:
            if generation == self._server_generation:
                self._server_phase = Phase.FAILED if failure is not None else Phase.IDLE
                self._server_error = failure
        self.evidence.write(
            "session_server_stopped",
            reason="failed" if failure is not None else "stopped",
            error=failure,
        )

    def _join_bot_cleanup(self) -> None:
        with self._lock:
            thread = self._bot_thread
        if thread is None or thread is threading.current_thread() or thread.ident is None:
            return
        thread.join(BOT_CLEANUP_SECONDS)

    # -- the bot worker ------------------------------------------------- #
    def _bot_work(self, generation: int) -> None:
        loop = None
        session = None
        failure: Optional[str] = None
        try:
            if self._bot_stop.is_set() or generation != self._bot_generation:
                raise StartAborted()
            # A previous bot's console must never be readable while this starts;
            # the worker owns the swap so no server lock is taken under ours.
            self._install_console(None)
            with self._lock:
                server = self._server_session
                world_id = self._server_world
                if (
                    self._server_phase is not Phase.RUNNING
                    or server is None
                    or world_id is None
                ):
                    raise RuntimeError("the managed server is not ready")
            session = self._start_bot_session(server, world_id, generation)
            if self._bot_stop.is_set() or generation != self._bot_generation:
                raise StartAborted()
            code = session.process.poll()
            if code is not None:
                raise RuntimeError(f"the bot exited during startup (code {code})")

            runtime = self._runtime(int(session.port), world_id)
            loop = runtime.loop
            self._install_console(runtime.console)
            if self._bot_stop.is_set():
                raise StartAborted()
            loop.start()
            self._await_ready(loop, session, generation)
            if self._bot_stop.is_set():
                raise StartAborted()

            with self._lock:
                if (
                    self._bot_stop.is_set()
                    or self._closing
                    or generation != self._bot_generation
                ):
                    # Stop or shutdown won the race for the slot: never publish
                    # a ready bot the page could then race with teardown.
                    raise StartAborted()
                self._bot_session = session
                self._loop = loop
                self._bot_world = world_id
                self._bot_phase = Phase.RUNNING
                self._bot_ready = True
            self.evidence.write(
                "session_bot_ready",
                world_id=world_id,
                body_port=int(session.port),
                run_dir=str(session.run_dir),
            )
            self._watch(loop, session, self._make_desync(session))
        except StartAborted:
            pass
        except Exception as error:  # every startup/runtime failure reaches the page
            failure = _describe(error)
            self._evidence_error("session_bot_error", failure, self.phase)
            self._log(f"bot: {failure}")
        finally:
            self._finish_bot(generation, loop, session, failure)

    def _start_server_session(self) -> Any:
        if self._server_start_fn is not None:
            return self._server_start_fn(self._server_stop.is_set)
        from praxion_harness.autonomy import (
            DEFAULT_START_TIME,
            peaceful_setup,
            survival_setup,
        )
        from praxion_harness.body import StartAborted as HarnessStartAborted
        from praxion_harness.body import start_server
        from praxion_harness.paths import repo_root

        setup = (
            peaceful_setup
            if self.peaceful
            else survival_setup(self.start_time or DEFAULT_START_TIME)
        )
        # A fresh world id per managed server: Stop server then Start server is
        # a new world, so an earlier world's brain memory cannot leak in. Bot
        # restarts reuse this same id and therefore its memory.
        run_dir = repo_root() / "runs" / f"console-world-{uuid.uuid4().hex}"
        try:
            return start_server(
                self.seed,
                setup=setup,
                run_dir=run_dir,
                keep=True,
                should_stop=self._server_stop.is_set,
            )
        except HarnessStartAborted as error:
            raise StartAborted(str(error)) from error

    def _start_bot_session(self, server: Any, world_id: str, generation: int) -> Any:
        if self._bot_start_fn is not None:
            return self._bot_start_fn(server, self._bot_stop.is_set)
        from praxion_harness.body import StartAborted as HarnessStartAborted
        from praxion_harness.body import start_session

        # One attempt directory per bot under the one managed world. The server
        # keeps its own files; a restart never overwrites the last attempt.
        attempt_dir = (
            Path(server.run_dir)
            / "bots"
            / f"attempt-{generation}-{uuid.uuid4().hex}"
        )
        try:
            return start_session(
                self.seed,
                setup=None,  # the server's setup ran once, before Join existed
                max_ticks=10**9,
                keep=True,
                server=server,
                run_dir=attempt_dir,
                extra_args=["--operator", self.operator],
                should_stop=self._bot_stop.is_set,
            )
        except HarnessStartAborted as error:
            raise StartAborted(str(error)) from error

    def _runtime(self, port: int, world_id: str) -> Runtime:
        if self._runtime_factory is not None:
            return self._runtime_factory(port, world_id)
        return build_runtime(
            port=port,
            world_id=world_id,
            evidence=self.evidence,
            ledger=self.ledger,
            memory_root=self.memory_root,
        )

    def _make_desync(self, session: Any) -> Any:
        path = Path(session.run_dir) / "body" / "events.jsonl"
        if self._desync_factory is not None:
            return self._desync_factory(path)
        from praxion_harness.autonomy import DesyncWatch

        return DesyncWatch(path)

    def _launch_client(self, port: int, session: Any, generation: int) -> Any:
        """Prepare the client without the lifecycle lock, then spawn it.

        The Java version check and the command build read the machine-local
        client and run ``java -version``; that must not block status, Stop or a
        second Join. The server is checked again between preparing and
        spawning, so a client never starts for a server that already ended.
        The produced command is spawned under the same lock as that check, so
        Stop cannot land in the gap and leave an orphan client behind.

        An injected launcher (``client_launch``) is a test seam and may block,
        so it stays outside the lock; it is validated under the lock first.
        """
        try:
            if self._client_launch is not None:
                prepared = None
            else:
                from praxion_harness.watch_client import client_command, spawn_client

                prepared = client_command(name=self.operator, host=LOOPBACK, port=port)
        except Exception as error:  # the launcher's own message is the useful one
            raise JoinError(f"could not start Minecraft: {error}") from error
        if prepared is None:
            with self._lock:
                live = self._join_is_live(session, generation)
            if not live:
                raise JoinError("the server stopped before the client could start")
            try:
                return self._client_launch(host=LOOPBACK, port=port, name=self.operator)
            except Exception as error:  # the launcher's own message is the useful one
                raise JoinError(f"could not start Minecraft: {error}") from error
        command, paths = prepared
        with self._lock:
            if not self._join_is_live(session, generation):
                raise JoinError("the server stopped before the client could start")
            try:
                return spawn_client(command, paths)
            except Exception as error:  # the launcher's own message is the useful one
                raise JoinError(f"could not start Minecraft: {error}") from error

    def _join_is_live(self, session: Any, generation: int) -> bool:
        """Whether Join may still start a client for ``session``; caller holds the lock."""
        return (
            self._server_phase is Phase.RUNNING
            and self._server_session is session
            and generation == self._server_generation
            and not self._closing
            and not self._server_stop.is_set()
        )

    # -- bot worker stages ---------------------------------------------- #
    def _await_ready(self, loop: Any, session: Any, generation: int) -> None:
        """Step the loop until the body's first snapshot makes the bot usable."""
        deadline = self._clock() + self.ready_timeout
        while True:
            if self._bot_stop.is_set() or generation != self._bot_generation:
                raise StartAborted()
            code = session.process.poll()
            if code is not None:
                raise RuntimeError(f"the bot exited during startup (code {code})")
            loop.run_once(STEP_TIMEOUT_SECONDS)
            if self._bot_stop.is_set():
                raise StartAborted()
            if loop.stopped:
                bye = _bye_reason(loop)
                detail = f": bye({bye})" if bye is not None else ""
                raise RuntimeError(f"the brain loop stopped before its first snapshot{detail}")
            if loop.session.connected and loop.session.snapshot is not None:
                return
            if self._clock() >= deadline:
                raise RuntimeError(
                    f"the brain did not receive a first snapshot within "
                    f"{self.ready_timeout:.0f}s"
                )

    def _watch(self, loop: Any, session: Any, desync: Any) -> None:
        """Step the loop while watching the stop event, the body and the world."""
        while not self._bot_stop.is_set():
            loop.run_once(STEP_TIMEOUT_SECONDS)
            if self._bot_stop.is_set():
                return
            # Report the specific cause first: a desync or a dead body also
            # stops the loop, and the generic message would hide what broke.
            reason = desync.poll()
            if reason is not None:
                raise RuntimeError(f"the world desynced: {reason}")
            code = session.process.poll()
            if code is not None:
                raise RuntimeError(f"the bot exited unexpectedly (code {code})")
            if loop.stopped:
                bye = _bye_reason(loop)
                if bye is not None:
                    raise RuntimeError(f"the body closed the session with bye({bye})")
                raise RuntimeError("the brain loop stopped unexpectedly")

    def _finish_bot(
        self, generation: int, loop: Any, session: Any, failure: Optional[str]
    ) -> None:
        with self._lock:
            if generation == self._bot_generation:
                self._bot_ready = False
                self._bot_session = None
                self._bot_world = None
                self._loop = None
                if self._bot_phase is not Phase.FAILED:
                    self._bot_phase = Phase.STOPPING
        problems: list[str] = []
        try:
            if loop is not None:
                loop.stop()
                loop.close()
        except Exception as error:  # cleanup still has to reach the bot session
            problems.append(f"brain cleanup failed: {_describe(error)}")
        try:
            if session is not None:
                # Borrowed: stops the body and forwarder only, never the server.
                session.stop()
        except Exception as error:
            problems.append(f"bot cleanup failed: {_describe(error)}")
        if problems:
            self.evidence.write("session_bot_cleanup_error", problems=problems)
            if failure is None:
                failure = "; ".join(problems)
        self._install_console(None)
        with self._lock:
            if generation == self._bot_generation:
                self._bot_phase = Phase.FAILED if failure is not None else Phase.IDLE
                self._bot_error = failure
        self.evidence.write(
            "session_bot_stopped",
            reason="failed" if failure is not None else "stopped",
            error=failure,
        )

    def _evidence_error(self, kind: str, failure: str, phase: Phase) -> None:
        try:
            self.evidence.write(
                kind,
                error=failure,
                error_kind="StartError",
                phase=phase.value,
            )
        except Exception:  # the record itself is what failed; the page still shows it
            pass


def _describe(error: BaseException) -> str:
    text = str(error).strip()
    return f"{type(error).__name__}: {text}" if text else type(error).__name__


def _bye_reason(loop: Any) -> Optional[str]:
    """The body's terminal bye reason when the loop ended on one, else None."""
    port = getattr(getattr(loop, "session", None), "port", None)
    reason = getattr(port, "bye_reason", None)
    return str(reason) if reason else None

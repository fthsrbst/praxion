"""The managed console's split lifecycle: server, bot, stop semantics and guards.

No test here starts Java, a body or a client. A fake server session, a fake bot
session and a fake loop exercise the two workers' phase transitions; the HTTP
tests drive the same manager through the real console server. The lifecycle is
deliberately asymmetric: Start server is joinable on its own, Stop bot keeps the
world, Stop server cleans both, and a failure of either side never takes the
healthy other side down.
"""

from __future__ import annotations

import http.client
import json
import threading
import time
from pathlib import Path
from types import SimpleNamespace

import pytest

from praxion_brain.console import run as run_module
from praxion_brain.console.hub import Console
from praxion_brain.console.run import JoinError, Phase, SessionManager, StartAborted
from praxion_brain.console.server import ConsoleServer, SESSION_ACTIONS
from praxion_brain.evidence import Evidence
from praxion_brain.ledger import Ledger
from praxion_brain.memory import Memory


# --------------------------------------------------------------------------- #
# Fakes
# --------------------------------------------------------------------------- #
class FakeProcess:
    def __init__(self, pid: int = 4242) -> None:
        self.pid = pid
        self.returncode = None

    def poll(self):
        return self.returncode

    def exit(self, code: int = 1) -> None:
        self.returncode = code


class FakeServerSession:
    """One independently owned server; its stop() is the ownership assertion."""

    def __init__(self, root: Path, *, game_port: int = 25641, index: int = 1) -> None:
        self.run_dir = root / "runs" / f"console-world-{index}"
        self.run_dir.mkdir(parents=True, exist_ok=True)
        self.server = SimpleNamespace(port=game_port, process=FakeProcess(pid=3000 + index))
        self.port = game_port
        # The real ServerSession carries a referee and fixture; borrowed body
        # starts reuse them, so they must be present here too.
        self.referee = object()
        self.fixture = object()
        self.stopped = False

    def stop(self) -> None:
        self.stopped = True


class FakeSession:
    """One borrowed bot attempt under a server's world directory."""

    def __init__(
        self,
        server: FakeServerSession,
        root: Path,
        *,
        body_port: int = 25640,
        index: int = 1,
        stop_gate: threading.Event | None = None,
    ) -> None:
        self.run_dir = Path(server.run_dir) / "bots" / f"attempt-{index}"
        self.run_dir.mkdir(parents=True, exist_ok=True)
        self.port = body_port
        self.server = server.server
        self.process = FakeProcess(pid=4000 + index)
        self.stopped = False
        self.stop_gate = stop_gate

    def stop(self) -> None:
        if self.stop_gate is not None:
            self.stop_gate.wait(5.0)
        self.stopped = True


class FakeLoopSession:
    def __init__(self) -> None:
        self.connected = True
        self.snapshot = None


class FakeLoop:
    def __init__(self, *, snapshot_after: int = 1) -> None:
        self.session = FakeLoopSession()
        self.stopped = False
        self.started = False
        self.closed = False
        self.steps = 0
        self.snapshot_after = snapshot_after

    def start(self) -> None:
        self.started = True

    def run_once(self, timeout: float = 0.0) -> None:
        self.steps += 1
        if self.steps >= self.snapshot_after:
            self.session.snapshot = {"facts": {"inventory": {}}}

    def stop(self) -> None:
        self.stopped = True

    def close(self) -> None:
        self.closed = True


class FakeDesync:
    def __init__(self) -> None:
        self.reason = None

    def poll(self):
        return self.reason


class Rig:
    def __init__(self, tmp_path: Path, *, snapshot_after: int = 1) -> None:
        self.tmp_path = tmp_path
        self.evidence = Evidence(tmp_path / "evidence")
        self.ledger = Ledger(tmp_path / "ledger.jsonl")
        self.servers: list[FakeServerSession] = []
        self.sessions: list[FakeSession] = []
        self.loops: list[FakeLoop] = []
        self.consoles: list[Console] = []
        self.runtime_calls = 0
        self.snapshot_after = snapshot_after
        self.desync = FakeDesync()
        self.server_calls = 0
        self.bot_calls = 0
        self.bot_servers: list[FakeServerSession] = []
        self.launched: list[dict] = []
        self.clients: list[FakeProcess] = []

    def default_server_starter(self, should_stop):
        self.server_calls += 1
        server = FakeServerSession(self.tmp_path, index=self.server_calls)
        self.servers.append(server)
        return server

    def default_bot_starter(self, server, should_stop):
        self.bot_calls += 1
        self.bot_servers.append(server)
        session = FakeSession(server, self.tmp_path, index=self.bot_calls)
        self.sessions.append(session)
        return session

    def runtime_factory(self, port: int, world_id: str):
        self.runtime_calls += 1
        console = Console(
            evidence=self.evidence,
            ledger=self.ledger,
            memory=Memory(world_id, root=self.tmp_path / "worlds"),
            world_id=world_id,
        )
        loop = FakeLoop(snapshot_after=self.snapshot_after)
        self.consoles.append(console)
        self.loops.append(loop)
        return SimpleNamespace(console=console, loop=loop)

    def launch(self, *, host: str, port: int, name: str):
        self.launched.append({"host": host, "port": port, "name": name})
        process = FakeProcess(pid=5000 + len(self.launched))
        self.clients.append(process)
        return process

    def manager(self, *, server_starter=None, bot_starter=None, **kwargs) -> SessionManager:
        return SessionManager(
            seed=303,
            operator="Fatih",
            evidence=self.evidence,
            ledger=self.ledger,
            memory_root=self.tmp_path / "worlds",
            server_start_fn=server_starter or self.default_server_starter,
            bot_start_fn=bot_starter or self.default_bot_starter,
            runtime_factory=self.runtime_factory,
            client_launch=self.launch,
            desync_factory=lambda path: self.desync,
            ready_timeout=5.0,
            **kwargs,
        )


def eventually(condition, seconds: float = 5.0) -> bool:
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if condition():
            return True
        time.sleep(0.01)
    return bool(condition())


def wait_server(manager: SessionManager, phase: Phase, seconds: float = 5.0) -> None:
    assert eventually(lambda: manager.server_phase is phase, seconds), manager.status()


def wait_bot(manager: SessionManager, phase: Phase, seconds: float = 5.0) -> None:
    assert eventually(lambda: manager.phase is phase, seconds), manager.status()


class GateLock:
    """The manager's lifecycle lock with a gate for a worker thread.

    With ``block`` set, a non-main thread waits at ``__enter__`` before it
    takes the lock. The test opens the gate only while the bot worker is inside
    ``_await_ready`` (which takes no lifecycle lock), so the next acquisition is
    the publish step and the ready/stop race becomes deterministic.
    """

    def __init__(self) -> None:
        self._lock = threading.RLock()
        self.block = False
        self.entered = threading.Event()
        self.release = threading.Event()

    def __enter__(self) -> "GateLock":
        if self.block and threading.current_thread() is not threading.main_thread():
            self.entered.set()
            self.release.wait(5.0)
        self._lock.acquire()
        return self

    def __exit__(self, *_exc: object) -> None:
        self._lock.release()


def call(server, method, path, body=None, headers=None):
    connection = http.client.HTTPConnection("127.0.0.1", server.port, timeout=5)
    payload = json.dumps(body) if body is not None else None
    connection.request(method, path, body=payload, headers=headers or {})
    response = connection.getresponse()
    raw = response.read()
    connection.close()
    return response.status, json.loads(raw) if raw else None


def start_console(manager: SessionManager) -> ConsoleServer:
    server = ConsoleServer(port=0, manager=manager).start()
    manager.attach_server(server)
    return server


# --------------------------------------------------------------------------- #
# Server and bot are independent
# --------------------------------------------------------------------------- #
def test_server_ready_without_a_bot_is_joinable(tmp_path) -> None:
    rig = Rig(tmp_path)
    manager = rig.manager()
    try:
        assert manager.start_server() is True
        wait_server(manager, Phase.RUNNING)
        status = manager.status()

        assert status["server"]["ready"] is True
        assert status["server"]["address"] == "127.0.0.1:25641"
        assert status["server"]["world_id"] == "console-world-1"
        assert status["bot"]["phase"] == "idle" and status["bot"]["ready"] is False
        # Legacy short fields still describe the bot, not the server.
        assert status["phase"] == "idle" and status["ready"] is False
        assert status["can"] == {
            "start_server": False,
            "stop_server": True,
            "start_bot": True,
            "stop_bot": False,
            "join": True,
        }
        assert rig.bot_calls == 0 and rig.consoles == []
        assert rig.servers[0].stopped is False

        joined = manager.join()
        assert joined["address"] == "127.0.0.1:25641"
        assert rig.launched == [{"host": "127.0.0.1", "port": 25641, "name": "Fatih"}]
    finally:
        manager.shutdown()


def test_bot_can_start_only_after_the_server_is_ready(tmp_path) -> None:
    rig = Rig(tmp_path)
    early = rig.manager()
    try:
        assert early.start_bot() is False
    finally:
        early.shutdown()

    manager = rig.manager()
    try:
        assert manager.start_server() is True
        wait_server(manager, Phase.RUNNING)
        assert manager.start_bot() is True
        wait_bot(manager, Phase.RUNNING)

        status = manager.status()
        assert status["server"]["ready"] is True
        assert status["bot"]["ready"] is True
        assert status["bot"]["world_id"] == status["server"]["world_id"]
        assert status["address"] == status["server"]["address"] == "127.0.0.1:25641"
        assert status["can"]["start_bot"] is False
        assert status["can"]["stop_bot"] is True
        assert eventually(lambda: manager._console is rig.consoles[0])
    finally:
        manager.shutdown()


def test_ready_is_published_only_after_the_first_snapshot(tmp_path) -> None:
    rig = Rig(tmp_path, snapshot_after=10**9)
    manager = rig.manager()
    try:
        assert manager.start_server() is True
        wait_server(manager, Phase.RUNNING)
        assert manager.start_bot() is True
        assert eventually(lambda: rig.loops and rig.loops[0].steps > 0)
        assert manager.phase is Phase.STARTING
        status = manager.status()
        assert status["bot"]["ready"] is False
        assert status["server"]["ready"] is True
        assert status["can"]["join"] is True

        rig.loops[0].session.snapshot = {"facts": {}}
        wait_bot(manager, Phase.RUNNING)
        assert manager.status()["bot"]["ready"] is True
    finally:
        manager.shutdown()


def test_stop_and_restart_bot_reuses_the_server_world_and_memory(tmp_path) -> None:
    rig = Rig(tmp_path)
    manager = rig.manager()
    try:
        assert manager.start_server() is True
        wait_server(manager, Phase.RUNNING)
        server = rig.servers[0]

        assert manager.start_bot() is True
        wait_bot(manager, Phase.RUNNING)
        first_console = rig.consoles[0]
        first_attempt = rig.sessions[0].run_dir
        world_id = manager.status()["server"]["world_id"]

        assert manager.request_stop_bot() is True
        wait_bot(manager, Phase.IDLE)
        assert manager.server_phase is Phase.RUNNING
        assert server.stopped is False
        assert rig.sessions[0].stopped is True
        assert rig.loops[0].closed is True
        assert eventually(lambda: manager._console is None)

        status = manager.status()
        assert status["server"]["ready"] is True
        assert status["bot"]["phase"] == "idle"
        assert status["can"]["start_bot"] is True
        assert status["can"]["join"] is True

        assert manager.start_bot() is True
        wait_bot(manager, Phase.RUNNING)
        assert len(rig.servers) == 1, "a bot restart must not start a second server"
        assert server.stopped is False
        second_console = rig.consoles[1]
        assert second_console is not first_console
        assert second_console.control is not first_console.control
        assert second_console.memory.directory == first_console.memory.directory
        assert manager.status()["bot"]["world_id"] == world_id
        assert rig.sessions[1].run_dir != first_attempt
        assert rig.sessions[1].run_dir.parent == first_attempt.parent
        assert rig.bot_servers == [server, server], "both bots borrowed the same server"
        assert rig.runtime_calls == 2
        assert first_console.watchers == 0 and second_console.watchers == 0
    finally:
        manager.shutdown()


def test_stop_server_cleans_the_bot_then_the_server(tmp_path) -> None:
    rig = Rig(tmp_path)
    manager = rig.manager()
    try:
        assert manager.start_server() is True
        wait_server(manager, Phase.RUNNING)
        assert manager.start_bot() is True
        wait_bot(manager, Phase.RUNNING)

        assert manager.request_stop_server() is True
        wait_bot(manager, Phase.IDLE)
        wait_server(manager, Phase.IDLE)
        assert rig.sessions[0].stopped is True
        assert rig.servers[0].stopped is True
        assert eventually(lambda: manager._console is None)
        status = manager.status()
        assert status["server"]["phase"] == "idle" and status["server"]["ready"] is False
        assert status["can"]["start_server"] is True
        assert status["can"]["join"] is False
        assert status["can"]["stop_server"] is False

        # A new server is a new world and can start again.
        assert manager.start_server() is True
        wait_server(manager, Phase.RUNNING)
        assert manager.status()["server"]["world_id"] == "console-world-2"
    finally:
        manager.shutdown()


def test_stop_server_works_while_the_bot_is_starting(tmp_path) -> None:
    rig = Rig(tmp_path)
    entered, release = threading.Event(), threading.Event()

    def bot_starter(server, should_stop):
        entered.set()
        while not should_stop():
            time.sleep(0.01)
        release.set()
        raise StartAborted("stopped during startup")

    manager = rig.manager(bot_starter=bot_starter)
    try:
        assert manager.start_server() is True
        wait_server(manager, Phase.RUNNING)
        assert manager.start_bot() is True
        assert entered.wait(3.0)
        status = manager.status()
        assert status["bot"]["phase"] == "starting"
        assert status["can"]["stop_bot"] is True
        assert status["can"]["start_bot"] is False

        assert manager.request_stop_server() is True
        assert release.wait(3.0)
        wait_bot(manager, Phase.IDLE)
        wait_server(manager, Phase.IDLE)
        assert rig.servers[0].stopped is True
        assert rig.runtime_calls == 0
        assert manager.status()["bot"]["error"] is None
    finally:
        release.set()
        manager.shutdown()


def test_stop_server_is_bounded_while_the_bot_is_dying(tmp_path, monkeypatch) -> None:
    rig = Rig(tmp_path)
    stop_gate = threading.Event()

    def bot_starter(server, should_stop):
        session = FakeSession(server, tmp_path, index=1, stop_gate=stop_gate)
        rig.sessions.append(session)
        return session

    monkeypatch.setattr(run_module, "BOT_CLEANUP_SECONDS", 0.2)
    manager = rig.manager(bot_starter=bot_starter)
    try:
        assert manager.start_server() is True
        wait_server(manager, Phase.RUNNING)
        assert manager.start_bot() is True
        wait_bot(manager, Phase.RUNNING)

        # The body died; its cleanup (terminating the process) is still running.
        rig.sessions[0].process.exit(7)
        wait_bot(manager, Phase.STOPPING)
        assert manager.server_phase is Phase.RUNNING

        # Stop server must not wait forever for the dying bot; its own cleanup
        # is bounded, and the server still goes down.
        assert manager.request_stop_server() is True
        wait_server(manager, Phase.IDLE)
        assert rig.servers[0].stopped is True
        assert manager.phase is Phase.STOPPING
    finally:
        stop_gate.set()
        wait_bot(manager, Phase.FAILED)
        manager.shutdown()


def test_join_depends_on_the_server_and_survives_a_bot_stop(tmp_path) -> None:
    rig = Rig(tmp_path)
    manager = rig.manager()
    try:
        with pytest.raises(JoinError, match="server is not ready"):
            manager.join()

        assert manager.start_server() is True
        wait_server(manager, Phase.RUNNING)
        assert manager.start_bot() is True
        wait_bot(manager, Phase.RUNNING)
        first = manager.join()
        assert first["pid"] == 5001
        with pytest.raises(JoinError, match="already running"):
            manager.join()

        assert manager.request_stop_bot() is True
        wait_bot(manager, Phase.IDLE)
        # The user's client is not the bot: its process is untouched.
        assert manager.status()["client"] == {"running": True, "pid": 5001}
        assert manager.status()["can"]["join"] is False

        rig.clients[0].exit()
        assert eventually(lambda: manager.status()["client"]["running"] is False)
        second = manager.join()
        assert second["pid"] == 5002
        assert len(rig.launched) == 2
    finally:
        manager.shutdown()


# --------------------------------------------------------------------------- #
# Failures keep the healthy side
# --------------------------------------------------------------------------- #
def test_server_failure_is_visible_and_a_retry_starts(tmp_path) -> None:
    rig = Rig(tmp_path)
    calls = {"n": 0}

    def server_starter(should_stop):
        calls["n"] += 1
        if calls["n"] == 1:
            raise RuntimeError("the Minecraft server jar is missing")
        return rig.default_server_starter(should_stop)

    manager = rig.manager(server_starter=server_starter)
    try:
        assert manager.start_server() is True
        wait_server(manager, Phase.FAILED)
        status = manager.status()
        assert "server jar is missing" in status["server"]["error"]
        assert status["server"]["ready"] is False and status["address"] is None
        assert status["can"]["start_server"] is True
        assert status["can"]["start_bot"] is False
        assert manager.start_bot() is False
        assert rig.bot_calls == 0

        assert manager.start_server() is True
        wait_server(manager, Phase.RUNNING)
        assert calls["n"] == 2
        assert manager.status()["server"]["error"] is None
    finally:
        manager.shutdown()


def test_bot_failure_leaves_the_server_ready_for_retry(tmp_path) -> None:
    rig = Rig(tmp_path)
    calls = {"n": 0}

    def bot_starter(server, should_stop):
        calls["n"] += 1
        if calls["n"] == 1:
            raise RuntimeError("the body binary is missing")
        return rig.default_bot_starter(server, should_stop)

    manager = rig.manager(bot_starter=bot_starter)
    try:
        assert manager.start_server() is True
        wait_server(manager, Phase.RUNNING)
        assert manager.start_bot() is True
        wait_bot(manager, Phase.FAILED)

        status = manager.status()
        assert "body binary is missing" in status["bot"]["error"]
        assert status["server"]["ready"] is True
        assert status["server"]["error"] is None
        assert status["can"]["start_bot"] is True
        assert status["can"]["join"] is True
        assert manager._console is None
        assert rig.servers[0].stopped is False

        assert manager.start_bot() is True
        wait_bot(manager, Phase.RUNNING)
        assert calls["n"] == 2
        assert manager.status()["bot"]["error"] is None
        assert manager.status()["server"]["ready"] is True
    finally:
        manager.shutdown()


def test_an_unexpected_bot_exit_fails_only_the_bot(tmp_path) -> None:
    rig = Rig(tmp_path)
    manager = rig.manager()
    try:
        assert manager.start_server() is True
        wait_server(manager, Phase.RUNNING)
        assert manager.start_bot() is True
        wait_bot(manager, Phase.RUNNING)

        rig.sessions[0].process.exit(7)
        wait_bot(manager, Phase.FAILED)
        status = manager.status()
        assert "bot exited unexpectedly" in status["bot"]["error"]
        assert status["server"]["ready"] is True
        assert rig.sessions[0].stopped is True
        assert rig.loops[0].closed is True
        assert rig.servers[0].stopped is False
    finally:
        manager.shutdown()


def test_a_world_desync_fails_the_bot_and_keeps_the_server(tmp_path) -> None:
    rig = Rig(tmp_path)
    manager = rig.manager()
    try:
        assert manager.start_server() is True
        wait_server(manager, Phase.RUNNING)
        assert manager.start_bot() is True
        wait_bot(manager, Phase.RUNNING)

        rig.desync.reason = "explode left 1 of 83 bytes unread"
        wait_bot(manager, Phase.FAILED)
        assert "world desynced" in manager.status()["bot"]["error"]
        assert manager.status()["server"]["ready"] is True
        assert rig.servers[0].stopped is False
    finally:
        manager.shutdown()


def test_stop_that_wins_before_the_bot_publish_never_reports_running(tmp_path) -> None:
    rig = Rig(tmp_path, snapshot_after=10**9)
    manager = rig.manager()
    gate = GateLock()
    manager._lock = gate
    try:
        assert manager.start_server() is True
        wait_server(manager, Phase.RUNNING)
        assert manager.start_bot() is True
        assert eventually(lambda: rig.loops and rig.loops[0].steps > 0)
        gate.block = True
        rig.loops[0].session.snapshot = {"facts": {}}  # readiness is reached
        assert gate.entered.wait(3.0), manager.status()
        assert manager.request_stop_bot() is True
        gate.block = False
        gate.release.set()

        wait_bot(manager, Phase.IDLE)
        status = manager.status()
        assert status["bot"]["ready"] is False
        assert status["bot"]["error"] is None
        assert status["server"]["ready"] is True
        assert rig.servers[0].stopped is False
        assert "session_bot_ready" not in rig.evidence.path.read_text()
    finally:
        gate.block = False
        gate.release.set()
        manager.shutdown()


def test_a_worker_that_cannot_be_recorded_or_started_frees_its_slot(tmp_path) -> None:
    rig = Rig(tmp_path)
    manager = rig.manager()
    real_write = manager.evidence.write
    calls = {"n": 0}

    def write(kind, **fields):
        if kind == "session_server_start_requested" and calls["n"] == 0:
            calls["n"] += 1
            raise OSError("evidence disk is full")
        return real_write(kind, **fields)

    manager.evidence.write = write
    try:
        assert manager.start_server() is False
        status = manager.status()
        assert status["server"]["phase"] == "failed"
        assert "did not start" in status["server"]["error"]
        assert status["can"]["start_server"] is True

        # The slot is free: no unstarted worker is left behind to join.
        assert manager.start_server() is True
        wait_server(manager, Phase.RUNNING)
    finally:
        manager.shutdown()


# --------------------------------------------------------------------------- #
# Manager guards
# --------------------------------------------------------------------------- #
def test_controls_refuse_actions_that_do_not_apply(tmp_path) -> None:
    rig = Rig(tmp_path)
    entered, release = threading.Event(), threading.Event()

    def server_starter(should_stop):
        entered.set()
        release.wait(5.0)
        return rig.default_server_starter(should_stop)

    manager = rig.manager(server_starter=server_starter)
    try:
        assert manager.request_stop_bot() is False
        assert manager.request_stop_server() is False
        assert manager.start_bot() is False

        assert manager.start_server() is True
        assert entered.wait(3.0)
        # A duplicate start is refused while the first is still starting.
        assert manager.start_server() is False
        assert manager.start_bot() is False
        release.set()
        wait_server(manager, Phase.RUNNING)

        assert manager.start_server() is False
        assert manager.start_bot() is True
        wait_bot(manager, Phase.RUNNING)
        assert manager.start_bot() is False
        assert manager.start_server() is False
        assert manager.request_stop_server() is True
    finally:
        release.set()
        manager.shutdown()


# --------------------------------------------------------------------------- #
# HTTP: lifecycle endpoint and authorization
# --------------------------------------------------------------------------- #
def test_state_has_separate_idle_server_and_bot_status(tmp_path) -> None:
    rig = Rig(tmp_path)
    manager = rig.manager()
    server = start_console(manager)
    try:
        status, state = call(server, "GET", "/api/state")
        assert status == 200
        session = state["session"]
        assert session["managed"] is True
        assert session["server"] == {
            "phase": "idle",
            "ready": False,
            "address": None,
            "world_id": None,
            "error": None,
            "can": {"start": True, "stop": False},
        }
        assert session["bot"] == {
            "phase": "idle",
            "ready": False,
            "world_id": None,
            "error": None,
            "can": {"start": False, "stop": False},
        }
        assert session["can"] == {
            "start_server": True,
            "stop_server": False,
            "start_bot": False,
            "stop_bot": False,
            "join": False,
        }
        assert state["world_id"] is None
        assert state["rungs"] and all(not rung["blocked"] for rung in state["rungs"])
        assert call(server, "GET", "/api/goals") == (200, [])
        assert call(server, "GET", "/api/skills")[1] == {
            "core": [],
            "learned": [],
            "candidates": [],
        }
        assert call(server, "GET", "/api/metrics")[0] == 200
    finally:
        manager.shutdown()
        server.shutdown()


def test_http_lifecycle_runs_server_then_bot_and_stops_them_separately(tmp_path) -> None:
    rig = Rig(tmp_path)
    manager = rig.manager()
    server = start_console(manager)
    headers = {"Content-Type": "application/json"}
    try:
        # The bot cannot start before the server is ready.
        status, body = call(server, "POST", "/api/session", {"action": "start_bot"}, headers=headers)
        assert status == 409 and "server is ready" in body["error"]

        status, body = call(server, "POST", "/api/session", {"action": "start_server"}, headers=headers)
        assert status == 202 and body["action"] == "start_server"
        wait_server(manager, Phase.RUNNING)

        status, state = call(server, "GET", "/api/state")
        assert state["session"]["server"]["ready"] is True
        assert state["session"]["server"]["address"] == "127.0.0.1:25641"
        assert state["session"]["bot"]["phase"] == "idle"

        # No loop exists yet: goal controls are refused, Join is not.
        status, _ = call(server, "POST", "/api/control", {"action": "pause"}, headers=headers)
        assert status == 409
        status, body = call(server, "POST", "/api/session", {"action": "join"}, headers=headers)
        assert status == 202 and body["client"]["address"] == "127.0.0.1:25641"

        status, body = call(server, "POST", "/api/session", {"action": "start_bot"}, headers=headers)
        assert status == 202 and body["action"] == "start_bot"
        wait_bot(manager, Phase.RUNNING)
        assert eventually(lambda: server.console is rig.consoles[0])

        status, body = call(server, "POST", "/api/control", {"action": "pause"}, headers=headers)
        assert status == 202 and body["queued"] == "pause"
        assert rig.consoles[0].control.drain() == [{"action": "pause"}]

        status, body = call(server, "POST", "/api/session", {"action": "stop_bot"}, headers=headers)
        assert status == 202 and body["session"]["bot"]["phase"] == "stopping"
        wait_bot(manager, Phase.IDLE)
        state = call(server, "GET", "/api/state")[1]
        assert state["session"]["server"]["ready"] is True
        assert state["session"]["server"]["address"] == "127.0.0.1:25641"
        assert state["session"]["bot"]["phase"] == "idle"
        assert eventually(lambda: server.console is None)

        status, body = call(server, "POST", "/api/session", {"action": "stop_server"}, headers=headers)
        assert status == 202
        wait_server(manager, Phase.IDLE)
        state = call(server, "GET", "/api/state")[1]
        assert state["session"]["server"]["ready"] is False
        assert state["session"]["address"] is None
    finally:
        manager.shutdown()
        server.shutdown()


def test_lifecycle_endpoint_accepts_only_the_fixed_action_enum(tmp_path) -> None:
    rig = Rig(tmp_path)
    manager = rig.manager()
    server = start_console(manager)
    json_headers = {"Content-Type": "application/json"}
    try:
        assert SESSION_ACTIONS == (
            "start_server",
            "start_bot",
            "stop_bot",
            "stop_server",
            "join",
        )
        cases = [
            ({"action": "start_server"}, {}, 415),
            ({"action": "start_server"}, {"Content-Type": "text/plain"}, 415),
            ({"action": "start_server", "port": 25565}, json_headers, 400),
            ({"action": "start_bot", "operator": "someone"}, json_headers, 400),
            # The old combined action is gone, not an alias.
            ({"action": "start"}, json_headers, 400),
            ({"action": "stop"}, json_headers, 400),
            ({"action": "launch"}, json_headers, 400),
            (["start_server"], json_headers, 400),
        ]
        for body, headers, expected in cases:
            status, _ = call(server, "POST", "/api/session", body, headers=headers)
            assert status == expected, (body, headers)
        # Nothing started from any refused request.
        assert manager.server_phase is Phase.IDLE
        assert manager.phase is Phase.IDLE
        assert rig.server_calls == 0 and rig.bot_calls == 0

        status, _ = call(
            server,
            "POST",
            "/api/session",
            {"action": "start_server"},
            headers={"Content-Type": "application/json", "Origin": "http://evil.example"},
        )
        assert status == 403
    finally:
        manager.shutdown()
        server.shutdown()


def test_lifecycle_endpoint_rejects_an_oversized_body(tmp_path) -> None:
    rig = Rig(tmp_path)
    manager = rig.manager()
    server = start_console(manager)
    try:
        status, _ = call(
            server,
            "POST",
            "/api/session",
            {"action": "start_server", "padding": "x" * 5000},
            headers={"Content-Type": "application/json"},
        )
        assert status == 413  # the length check runs before parsing
        connection = http.client.HTTPConnection("127.0.0.1", server.port, timeout=5)
        connection.request(
            "POST",
            "/api/session",
            body=b"x" * 5000,
            headers={"Content-Type": "application/json", "Content-Length": "5000"},
        )
        response = connection.getresponse()
        response.read()
        connection.close()
        assert response.status == 413
        assert manager.server_phase is Phase.IDLE
    finally:
        manager.shutdown()
        server.shutdown()


def test_a_brain_only_console_keeps_its_old_surface(tmp_path) -> None:
    rig = Rig(tmp_path)
    console = Console(
        evidence=rig.evidence,
        ledger=rig.ledger,
        memory=Memory("external", root=tmp_path / "worlds"),
        world_id="external",
    )
    console.publish({"world_id": "external", "state": "playing", "paused": False})
    server = ConsoleServer(console, 0).start()
    try:
        status, state = call(server, "GET", "/api/state")
        assert status == 200 and state["state"] == "playing"
        assert "session" not in state
        assert call(
            server,
            "POST",
            "/api/session",
            {"action": "start_server"},
            headers={"Content-Type": "application/json"},
        )[0] == 404
        assert call(server, "POST", "/api/control", {"action": "pause"})[0] == 202
    finally:
        server.shutdown()


# --------------------------------------------------------------------------- #
# HTTP: SSE follows the console swap and releases the right watcher
# --------------------------------------------------------------------------- #
def test_an_open_stream_follows_a_new_console_and_releases_the_old_watcher(tmp_path) -> None:
    rig = Rig(tmp_path)
    first = Console(
        evidence=rig.evidence,
        ledger=rig.ledger,
        memory=Memory("first", root=tmp_path / "worlds"),
        world_id="first",
    )
    second = Console(
        evidence=rig.evidence,
        ledger=rig.ledger,
        memory=Memory("second", root=tmp_path / "worlds"),
        world_id="second",
    )
    server = ConsoleServer(first, 0).start()
    connection = http.client.HTTPConnection("127.0.0.1", server.port, timeout=5)
    try:
        connection.request("GET", "/api/stream")
        response = connection.getresponse()
        assert response.status == 200
        assert response.readline().decode().startswith("event: state")
        assert eventually(lambda: first.watchers == 1)

        server.set_console(None)
        assert eventually(lambda: first.watchers == 0)
        server.set_console(second)
        assert eventually(lambda: second.watchers == 1)

        response.close()
        connection.close()
        assert eventually(lambda: second.watchers == 0)
        assert first.watchers == 0
    finally:
        connection.close()
        server.shutdown()

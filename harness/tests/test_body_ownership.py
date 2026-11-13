"""Ownership of a split server and body: a borrowed server is never stopped.

These are unit tests with fake processes: no Java, forwarder or body is started.
They pin the rule the managed console relies on: ``start_session(...,
server=...)`` owns only its forwarder and body, so Stop bot and a cancelled bot
start leave the server and its world alive.
"""

from __future__ import annotations

from pathlib import Path
from types import SimpleNamespace

import pytest

from praxion_harness.body import BodyRun, ServerSession, start_session


class FakeProcess:
    def __init__(self) -> None:
        self.terminated = False
        self.returncode = None

    def poll(self):
        return self.returncode

    def terminate(self) -> None:
        self.terminated = True

    def wait(self, timeout=None):
        return 0


class FakeServer:
    def __init__(self) -> None:
        self.stop_calls = 0
        self.keep = False
        self.port = 25565

    def stop(self) -> None:
        self.stop_calls += 1


def _body_run(tmp_path: Path, *, owns_server: bool) -> tuple[BodyRun, FakeProcess, FakeProcess, FakeServer]:
    server = FakeServer()
    forwarder = FakeProcess()
    body = FakeProcess()
    run = BodyRun(
        tmp_path / "attempt",
        server,  # type: ignore[arg-type]
        forwarder,  # type: ignore[arg-type]
        tmp_path / "forwarder.out",
        body,  # type: ignore[arg-type]
        tmp_path / "body.out",
        referee=SimpleNamespace(),  # type: ignore[arg-type]
        fixture=SimpleNamespace(),  # type: ignore[arg-type]
        keep=True,
        owns_server=owns_server,
    )
    return run, forwarder, body, server


def test_a_borrowed_body_stops_its_own_processes_and_not_the_server(tmp_path) -> None:
    run, forwarder, body, server = _body_run(tmp_path, owns_server=False)

    run.stop()

    assert body.terminated is True
    assert forwarder.terminated is True
    assert server.stop_calls == 0


def test_an_owning_body_still_stops_its_server(tmp_path) -> None:
    run, _, _, server = _body_run(tmp_path, owns_server=True)

    run.stop()

    assert server.stop_calls == 1


def test_a_borrowed_server_session_does_not_stop_the_server() -> None:
    server = FakeServer()
    session = ServerSession(
        Path("world"),
        server,  # type: ignore[arg-type]
        referee=SimpleNamespace(),  # type: ignore[arg-type]
        fixture=SimpleNamespace(),  # type: ignore[arg-type]
        keep=True,
        owns_server=False,
    )

    session.stop()

    assert server.stop_calls == 0


def test_a_borrowed_start_refuses_a_second_setup_before_touching_anything() -> None:
    # The server's setup ran once at start_server; a borrowed body must not
    # reset the clock or mob spawning on every bot restart.
    with pytest.raises(ValueError, match="already ran its setup"):
        start_session(303, server=SimpleNamespace(), setup=lambda *args: None)

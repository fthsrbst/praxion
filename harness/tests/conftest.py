"""Pytest fixtures shared by the harness tests.

Live-server tests get their servers from ``server_factory``. The factory stops every
server it started, keeps its run directory when the test failed (so the log can be
inspected) and prints that path. A session fixture stops anything still alive at the
end, so an interrupted test never leaves a Java process behind.

``needs_player`` tests also get a connected headless bot: ``server_factory`` starts
``body/target/release/praxion-headless`` against every server it creates for such a
test and waits until the bot prints ``joined``. The bot's output goes to
``<run_dir>/bot-Bot1.out`` next to the server log, and the bot is stopped before the
server it talked to.
"""

from __future__ import annotations

import os
import subprocess
import time
import uuid
from pathlib import Path

import pytest

from praxion_harness.paths import repo_root
from praxion_harness.server import TestServer, stop_active_servers

#: The name the headless bot logs in with; ``Referee(..., player=BOT_NAME)``.
BOT_NAME = "Bot1"
#: Seconds from process start to the ``joined`` line. Login is one round trip; this
#: only covers a cold process and a busy machine.
BOT_JOIN_TIMEOUT = 60.0


def headless_binary() -> Path:
    """The release binary of the headless body."""
    name = "praxion-headless.exe" if os.name == "nt" else "praxion-headless"
    return repo_root() / "body" / "target" / "release" / name


class HeadlessBot:
    """A running headless bot and its output file."""

    def __init__(self, process: subprocess.Popen, out_path: Path, out):
        self.process = process
        self.out_path = out_path
        self._out = out

    def output(self) -> str:
        try:
            return self.out_path.read_text(encoding="utf-8", errors="replace")
        except OSError:
            return ""

    def stop(self) -> None:
        try:
            self.process.terminate()
        except Exception:
            pass
        try:
            self.process.wait(timeout=10)
        except Exception:
            try:
                self.process.kill()
            except Exception:
                pass
        try:
            self._out.close()
        except Exception:
            pass


def start_headless_bot(server: TestServer, name: str = BOT_NAME) -> HeadlessBot:
    """Start the bot against ``server`` and wait for its ``joined`` line.

    Raises ``RuntimeError`` with the bot's output when it never joins, so a failed
    test says what the bot did.
    """
    binary = headless_binary()
    if not binary.exists():
        pytest.skip(
            f"headless bot is not built ({binary}); run: cd body && cargo build --release"
        )

    out_path = Path(server.run_dir) / f"bot-{name}.out"
    out = out_path.open("w", encoding="utf-8", errors="replace")
    process = subprocess.Popen(
        [str(binary), "--server", f"127.0.0.1:{server.port}", "--name", name],
        stdout=out,
        stderr=subprocess.STDOUT,
    )
    bot = HeadlessBot(process, out_path, out)
    deadline = time.monotonic() + BOT_JOIN_TIMEOUT
    while time.monotonic() < deadline:
        if process.poll() is not None:
            bot.stop()
            raise RuntimeError(
                f"the headless bot exited before joining ({out_path}):\n{bot.output()}"
            )
        if "joined" in bot.output():
            return bot
        time.sleep(0.1)
    bot.stop()
    raise RuntimeError(
        f"the headless bot did not print joined within {BOT_JOIN_TIMEOUT:.0f}s "
        f"({out_path}):\n{bot.output()}"
    )


@pytest.hookimpl(hookwrapper=True)
def pytest_runtest_makereport(item, call):
    outcome = yield
    report = outcome.get_result()
    setattr(item, f"rep_{report.when}", report)


@pytest.fixture
def players(request):
    """Start headless bots for a test and stop them when it ends.

    Yields a callable ``start(server, name=BOT_NAME)`` that connects a bot to
    ``server`` and returns its name once the bot has printed ``joined``.
    """
    started: list = []

    def start(server: TestServer, name: str = BOT_NAME) -> str:
        started.append(start_headless_bot(server, name))
        return name

    yield start

    for bot in started:
        bot.stop()


@pytest.fixture
def server_factory(request):
    created = []

    def make(seed, **kwargs):
        run_dir = repo_root() / "runs" / f"test-{uuid.uuid4().hex}"
        srv = TestServer(run_dir, seed, **kwargs)
        created.append(srv)
        srv.start()
        # A `needs_player` test needs somebody online on this very server.
        if request.node.get_closest_marker("needs_player") is not None:
            request.getfixturevalue("players")(srv)
        return srv

    yield make

    failed = any(
        getattr(getattr(request.node, f"rep_{when}", None), "failed", False)
        for when in ("setup", "call")
    )
    kept = []
    for srv in created:
        if failed:
            srv.keep = True
        srv.stop()
        if srv.keep:
            kept.append(srv.run_dir)
    if kept:
        print("kept failed-test run directories: " + ", ".join(str(p) for p in kept))


@pytest.fixture(scope="session", autouse=True)
def _stop_leftover_servers():
    yield
    for path in stop_active_servers():
        print(f"kept run directory: {path}")

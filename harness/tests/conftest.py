"""Pytest fixtures shared by the harness tests.

Live-server tests get their servers from ``server_factory``. The factory stops every
server it started, keeps its run directory when the test failed (so the log can be
inspected) and prints that path. A session fixture stops anything still alive at the
end, so an interrupted test never leaves a Java process behind.
"""

from __future__ import annotations

import uuid

import pytest

from praxion_harness.paths import repo_root
from praxion_harness.server import TestServer, stop_active_servers


@pytest.hookimpl(hookwrapper=True)
def pytest_runtest_makereport(item, call):
    outcome = yield
    report = outcome.get_result()
    setattr(item, f"rep_{report.when}", report)


@pytest.fixture
def server_factory(request):
    created = []

    def make(seed, **kwargs):
        run_dir = repo_root() / "runs" / f"test-{uuid.uuid4().hex}"
        srv = TestServer(run_dir, seed, **kwargs)
        created.append(srv)
        srv.start()
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

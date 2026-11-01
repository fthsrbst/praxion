"""Live-server integration tests (``python -m pytest -q -m server``).

Every server comes from ``server_factory``: it is stopped even when an assertion
fails, and its run directory is deleted unless the test failed (then it is kept
and its path printed).
"""

import os
import subprocess
import sys
from pathlib import Path

import pytest

from praxion_harness.cleanup import run_in_use
from praxion_harness.fixtures import FixtureChannel, GameRules, fixture_log_path
from praxion_harness.referee import Referee


def _fixtures(srv):
    return FixtureChannel(
        srv.rcon, fixture_log_path(srv.run_dir), run_id=srv.run_dir.name
    )


@pytest.mark.server
def test_server_comes_up_and_time_works(server_factory):
    srv = server_factory("111")
    print(
        f"STARTUP seed=111 done={srv.startup_seconds:.3f}s "
        f"ready={srv.ready_seconds:.3f}s"
    )
    assert srv.done_at is not None
    assert srv.rcon is not None
    assert srv.log_path.exists()

    ref = Referee(srv.rcon)
    assert 0 <= ref.time_of_day() < 24000
    channel = _fixtures(srv)
    channel.advance_time(False)
    channel.time_set("day")
    assert ref.time_of_day() == 1000
    assert "Timeline minecraft:day is at 1000 tick(s)" in srv.rcon.command("time query day")
    # Every fixture command is one JSON line in runs/<id>/fixtures.jsonl.
    assert fixture_log_path(srv.run_dir).is_file()

    # The CLI "query" path talks to the same server.
    harness_dir = Path(__file__).resolve().parents[1]
    cli = subprocess.run(
        [sys.executable, "-m", "praxion_harness", "query",
         "--rcon-port", str(srv.rcon_port), "time"],
        cwd=harness_dir, capture_output=True, text=True, timeout=60,
    )
    assert cli.returncode == 0, cli.stderr
    assert cli.stdout.strip() == "1000", cli.stdout


@pytest.mark.server
def test_block_query(server_factory):
    srv = server_factory("222")
    print(
        f"STARTUP seed=222 done={srv.startup_seconds:.3f}s "
        f"ready={srv.ready_seconds:.3f}s"
    )
    channel = _fixtures(srv)
    channel.raw("forceload add 0 0")
    channel.setblock(0, 64, 0, "minecraft:stone")

    assert srv.rcon.command("execute if block 0 64 0 minecraft:stone") == "Test passed"
    assert srv.rcon.command("execute if block 0 64 0 minecraft:dirt") == "Test failed"
    ref = Referee(srv.rcon)
    assert ref.block_is(0, 64, 0, "minecraft:stone") is True
    assert ref.block_at(0, 64, 0) == "minecraft:stone"


@pytest.mark.server
def test_gamerule_names_set_and_read_back(server_factory):
    srv = server_factory("333")
    print(
        f"STARTUP seed=333 done={srv.startup_seconds:.3f}s "
        f"ready={srv.ready_seconds:.3f}s"
    )
    channel = _fixtures(srv)
    cases = [
        (GameRules.ADVANCE_TIME, False, "false"),
        (GameRules.SPAWN_MOBS, False, "false"),
        (GameRules.KEEP_INVENTORY, True, "true"),
    ]
    for name, value, expected in cases:
        out = channel.gamerule(name, value).lower()
        assert "now set to" in out or "already set to" in out, out
        read = channel.gamerule_value(name).lower()
        assert f"currently set to {expected}" in read, read


@pytest.mark.server
def test_death_objective_exists(server_factory):
    srv = server_factory("444")
    print(
        f"STARTUP seed=444 done={srv.startup_seconds:.3f}s "
        f"ready={srv.ready_seconds:.3f}s"
    )
    ref = Referee(srv.rcon)
    out = ref.setup_death_objective()
    assert "Created new objective" in out, out
    assert "praxion_deaths" in srv.rcon.command("scoreboard objectives list")
    assert ref.death_count("Steve") == 0
    # 26.3 lets an offline name hold a deathCount score; this verifies the parser
    # and the criterion's read-back format.
    assert "Added 5" in srv.rcon.command("scoreboard players add Steve praxion_deaths 5")
    assert ref.death_count("Steve") == 5


@pytest.mark.server
def test_graceful_stop_leaves_no_java(server_factory):
    srv = server_factory("555")
    print(
        f"STARTUP seed=555 done={srv.startup_seconds:.3f}s "
        f"ready={srv.ready_seconds:.3f}s"
    )
    pid = srv.process.pid
    run_dir = srv.run_dir
    # A live server holds world/session.lock; clean-runs must see that.
    assert run_in_use(run_dir) is True
    srv.stop()
    assert not run_dir.exists()
    if os.name == "nt":
        result = subprocess.run(
            ["tasklist", "/FI", f"PID eq {pid}"], capture_output=True, text=True
        )
        assert str(pid) not in result.stdout


@pytest.mark.needs_player
@pytest.mark.server
def test_needs_player_health(server_factory):
    srv = server_factory("999")
    players = srv.rcon.command("list")
    if "0 of a max" in players or "There are 0" in players:
        pytest.skip("no player connected")
    ref = Referee(srv.rcon)
    assert "of a max" in players
    assert ref.player_health() > 0

"""Live-server tests of the referee against a connected headless bot.

Every test starts a fresh server (``server_factory``) with a headless bot online
(``needs_player``) and checks a referee fact against an independent source: a
second RCON command, a value we set with a fixture, or both.
"""

from __future__ import annotations

import re
import time

import pytest

from praxion_harness.fixtures import FixtureChannel, fixture_log_path
from praxion_harness.logtail import LogTail
from praxion_harness.referee import Referee

pytestmark = [pytest.mark.server, pytest.mark.needs_player]


def _fixtures(srv):
    return FixtureChannel(srv.rcon, fixture_log_path(srv.run_dir), run_id=srv.run_dir.name)


def _number(text):
    nums = re.findall(r"-?\d+(?:\.\d+)?", text)
    return float(nums[-1]) if nums else None


def _pos(text):
    m = re.search(r"\[\s*([-\d.eE+]+)d?,\s*([-\d.eE+]+)d?,\s*([-\d.eE+]+)d?\s*\]", text)
    if m:
        return float(m.group(1)), float(m.group(2)), float(m.group(3))
    nums = re.findall(r"-?\d+(?:\.\d+)?", text)
    if len(nums) >= 3:
        return float(nums[-3]), float(nums[-2]), float(nums[-1])
    raise ValueError(f"could not parse pos from: {text!r}")


def test_referee_health_drops_after_instant_damage(server_factory):
    srv = server_factory(7001)
    fx = _fixtures(srv)
    ref = Referee(srv.rcon, player="Bot1")

    before_out = srv.rcon.command("data get entity Bot1 Health")
    before_indep = _number(before_out)
    before_ref = ref.player_health()
    assert before_indep is not None
    assert before_ref == before_indep
    assert before_ref > 0

    fx.effect("Bot1", "minecraft:instant_damage", 1, 0)
    time.sleep(1)

    after_out = srv.rcon.command("data get entity Bot1 Health")
    after_indep = _number(after_out)
    after_ref = ref.player_health()
    assert after_indep is not None
    assert after_ref == after_indep
    assert after_ref < before_ref


def test_referee_food_drops_after_hunger(server_factory):
    srv = server_factory(7002)
    fx = _fixtures(srv)
    ref = Referee(srv.rcon, player="Bot1")

    before_out = srv.rcon.command("data get entity Bot1 foodLevel")
    before_indep = _number(before_out)
    before_ref = ref.player_food()
    assert before_indep is not None
    assert before_ref == int(before_indep)

    fx.effect("Bot1", "minecraft:hunger", 10, 255)
    time.sleep(5)

    after_out = srv.rcon.command("data get entity Bot1 foodLevel")
    after_indep = _number(after_out)
    after_ref = ref.player_food()
    assert after_indep is not None
    assert after_ref == int(after_indep)
    assert after_ref < before_ref


def test_referee_position_after_tp(server_factory):
    srv = server_factory(7003)
    fx = _fixtures(srv)
    ref = Referee(srv.rcon, player="Bot1")

    fx.tp("Bot1", 10, 80, 10)
    time.sleep(1)

    pos = ref.player_pos()
    # `tp` to integer x/z lands on the block centre, so the server reports .5.
    assert abs(pos[0] - 10.5) < 0.01
    assert abs(pos[1] - 80.0) < 0.01
    assert abs(pos[2] - 10.5) < 0.01

    # Independent source: a second RCON read of the same entity.
    out = srv.rcon.command("data get entity Bot1 Pos")
    ix, iy, iz = _pos(out)
    assert abs(ix - pos[0]) < 0.01
    assert abs(iy - pos[1]) < 0.01
    assert abs(iz - pos[2]) < 0.01
    # The headless bot has no physics yet, so it never falls: the server keeps
    # reporting y == 80.0 after the teleport instead of dropping to the ground.


# FINDING 1: Referee.player_inventory() mis-parses 26.3 data where `count` precedes `id`.
def test_referee_inventory_counts(server_factory):
    srv = server_factory(7004)
    fx = _fixtures(srv)
    ref = Referee(srv.rcon, player="Bot1")

    give_log = fx.give("Bot1", "minecraft:oak_log", 5)
    give_pick = fx.give("Bot1", "minecraft:stone_pickaxe", 1)
    assert "Gave 5" in give_log
    assert "Gave 1" in give_pick
    time.sleep(1)

    inv = ref.player_inventory()
    assert inv.get("minecraft:oak_log") == 5
    assert inv.get("minecraft:stone_pickaxe") == 1

    # Independent source: the raw entity data must name both items.
    out = srv.rcon.command("data get entity Bot1 Inventory")
    assert "minecraft:oak_log" in out
    assert "minecraft:stone_pickaxe" in out


def test_referee_dimension(server_factory):
    srv = server_factory(7005)
    ref = Referee(srv.rcon, player="Bot1")

    dim = ref.player_dimension()
    assert dim == "minecraft:overworld"

    out = srv.rcon.command("data get entity Bot1 Dimension")
    assert "minecraft:overworld" in out


def test_referee_death_count_after_kill(server_factory):
    srv = server_factory(7006)
    fx = _fixtures(srv)
    ref = Referee(srv.rcon, player="Bot1")

    ref.setup_death_objective()
    assert ref.death_count("Bot1") == 0

    fx.kill("Bot1")
    time.sleep(2)

    ref_count = ref.death_count("Bot1")
    out = srv.rcon.command("scoreboard players get Bot1 praxion_deaths")
    indep = 0
    m = re.search(r"has (-?\d+) \[", out)
    if m:
        indep = int(m.group(1))
    else:
        # "none is set" means zero.
        indep = 0
    assert ref_count == indep
    assert ref_count == 1


def test_referee_time_of_day_after_time_set(server_factory):
    srv = server_factory(7007)
    fx = _fixtures(srv)
    ref = Referee(srv.rcon, player="Bot1")

    fx.advance_time(False)
    fx.time_set(13000)
    time.sleep(0.5)

    assert ref.time_of_day() == 13000 % 24000

    out = srv.rcon.command("time query day")
    assert "13000" in out


def test_referee_is_night(server_factory):
    srv = server_factory(7008)
    fx = _fixtures(srv)
    ref = Referee(srv.rcon, player="Bot1")

    fx.time_set(13000)
    time.sleep(0.5)
    assert ref.is_night() is True
    # Independent check via the static helper.
    assert Referee.is_night_time(13000) is True

    fx.time_set(1000)
    time.sleep(0.5)
    assert ref.is_night() is False
    assert Referee.is_night_time(1000) is False


def test_referee_first_night_survived(server_factory):
    srv = server_factory(7009)
    fx = _fixtures(srv)
    fx.spawn_mobs(False)
    fx.advance_time(True)
    fx.time_set(23000)
    time.sleep(0.5)

    logtail = LogTail(srv.log_path)
    ref = Referee(srv.rcon, logtail=logtail, player="Bot1")

    deadline = time.time() + 60.0
    result = None
    while time.time() < deadline:
        result = ref.first_night_survived()
        if result is not None:
            break
        time.sleep(1.0)

    assert result is not None, "morning never arrived within 60 s"
    assert result is True

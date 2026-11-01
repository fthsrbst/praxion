"""Unit tests for the logged fixture channel."""

import json

from praxion_harness.fixtures import (
    FIXTURE_LOG_NAME,
    FixtureChannel,
    GameRules,
    fixture_log_path,
)


class FakeRcon:
    def __init__(self):
        self.commands = []

    def command(self, cmd):
        self.commands.append(cmd)
        return f"ok: {cmd}"


def test_fixture_log_path(tmp_path):
    assert fixture_log_path(tmp_path) == tmp_path / FIXTURE_LOG_NAME


def test_every_command_is_logged_as_json(tmp_path):
    log = fixture_log_path(tmp_path)
    rcon = FakeRcon()
    channel = FixtureChannel(rcon, log, run_id="run-1")

    assert channel.gamerule(GameRules.KEEP_INVENTORY, True) == "ok: gamerule keep_inventory true"
    assert channel.advance_time(False) == "ok: gamerule advance_time false"
    assert channel.spawn_mobs(False) == "ok: gamerule spawn_mobs false"
    assert channel.setblock(0, 64, 0, "minecraft:stone") == "ok: setblock 0 64 0 minecraft:stone"

    lines = log.read_text(encoding="utf-8").splitlines()
    assert len(lines) == 4
    entry = json.loads(lines[0])
    assert set(entry) == {"t", "run_id", "command", "response"}
    assert entry["run_id"] == "run-1"
    assert entry["command"] == "gamerule keep_inventory true"
    assert entry["response"] == "ok: gamerule keep_inventory true"
    assert isinstance(entry["t"], float)
    assert [json.loads(line)["command"] for line in lines] == rcon.commands


def test_26_3_gamerule_names():
    # Verified on the real 26.3 server; see tests/fixtures/observed-26.3.txt.
    assert GameRules.ADVANCE_TIME == "advance_time"
    assert GameRules.SPAWN_MOBS == "spawn_mobs"
    assert GameRules.KEEP_INVENTORY == "keep_inventory"

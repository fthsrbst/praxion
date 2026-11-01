"""Unit tests for the referee parsers; outputs are the real 26.3 formats."""

from praxion_harness.logtail import PlayerDied
from praxion_harness.referee import Referee


class FakeRcon:
    def __init__(self, responses):
        self.responses = list(responses)
        self.commands = []
    def command(self, cmd):
        self.commands.append(cmd)
        if self.responses:
            return self.responses.pop(0)
        return ""


class FakeLogTail:
    def __init__(self, events):
        self.events = list(events)

    def poll(self):
        out, self.events = self.events, []
        return out


def make(responses):
    r = FakeRcon(responses)
    ref = Referee(r, player="Steve")
    return ref, r


def test_player_health_food_pos_inventory_dimension():
    ref, r = make([
        "Steve has the following entity data: 18.5f",
        "Steve has the following entity data: 17",
        "Steve has the following entity data: [1.0d, 64.0d, 2.5d]",
        'Steve has the following entity data: [{Slot:0b, id:"minecraft:stone", count:3}, {id:"minecraft:dirt", count:2}]',
        'Steve has the following entity data: "minecraft:the_nether"',
    ])
    assert ref.player_health() == 18.5
    assert ref.player_food() == 17
    assert ref.player_pos() == (1.0, 64.0, 2.5)
    inv = ref.player_inventory()
    assert inv.get("minecraft:stone") == 3
    assert inv.get("minecraft:dirt") == 2
    assert ref.player_dimension() == "minecraft:the_nether"


def test_time_day_uses_26_3_queries():
    ref, r = make([
        "Timeline minecraft:day is at 1002 tick(s)",
        "The game time is 48000 tick(s)",
        "The game time is 48000 tick(s)",
        "Timeline minecraft:day is at 13002 tick(s)",
    ])
    assert ref.time_of_day() == 1002
    assert ref.day_count() == 2
    assert ref.game_time() == 48000
    assert ref.is_night() is True
    assert r.commands == [
        "time query day", "time query gametime", "time query gametime", "time query day",
    ]


def test_block_probe_uses_execute_if_block():
    ref, r = make([
        "The target block is not a block entity",
        "Test failed",   # air
        "Test passed",   # stone
        "Test failed",
    ])
    assert ref.block_at(0, 64, 0) == "minecraft:stone"
    assert r.commands[0] == "data get block 0 64 0"
    assert r.commands[1] == "execute if block 0 64 0 minecraft:air"
    assert ref.block_is(0, 64, 0, "minecraft:dirt") is False


def test_death_count_objective():
    ref, r = make([
        "Created new objective [praxion_deaths]",
        "Steve has 5 [praxion_deaths]",
        "Can't get value of praxion_deaths for Steve; none is set",
        "An objective already exists by that name",
    ])
    assert ref.setup_death_objective() == "Created new objective [praxion_deaths]"
    assert ref.death_count() == 5
    assert ref.death_count() == 0
    # Creating it twice is fine.
    assert "already exists" in ref.setup_death_objective()


def test_first_night_survived_counts_log_deaths():
    survived, _ = make([
        "Timeline minecraft:day is at 13000 tick(s)",
        "Timeline minecraft:day is at 1000 tick(s)",
    ])
    survived.logtail = FakeLogTail([])
    survived.observe()
    assert survived.first_night_survived() is True

    died, _ = make([
        "Timeline minecraft:day is at 13000 tick(s)",
        "Timeline minecraft:day is at 1000 tick(s)",
    ])
    died.logtail = FakeLogTail([PlayerDied("Steve", "Steve was slain by Zombie")])
    died.observe()
    assert died.first_night_survived() is False

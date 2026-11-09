"""Unit tests for the autonomy run's setup (no server)."""

from __future__ import annotations

from praxion_harness import autonomy


class FakeFixture:
    def __init__(self) -> None:
        self.calls: list[tuple] = []

    def advance_time(self, enabled) -> None:
        self.calls.append(("advance_time", enabled))

    def spawn_mobs(self, enabled) -> None:
        self.calls.append(("spawn_mobs", enabled))

    def time_set(self, value) -> None:
        self.calls.append(("time_set", value))


def test_the_default_run_is_survival_in_the_morning() -> None:
    fixture = FakeFixture()
    autonomy._survival(autonomy.DEFAULT_START_TIME)(fixture, None, None)
    assert fixture.calls == [
        ("advance_time", True),
        ("spawn_mobs", True),
        ("time_set", 0),
    ]


def test_the_start_time_is_the_one_given() -> None:
    fixture = FakeFixture()
    autonomy._survival(1000)(fixture, None, None)
    assert ("time_set", 1000) in fixture.calls


def test_peaceful_is_the_old_frozen_daytime() -> None:
    fixture = FakeFixture()
    autonomy._peaceful(fixture, None, None)
    assert fixture.calls == [
        ("advance_time", False),
        ("spawn_mobs", False),
        ("time_set", 1000),
    ]


class FakeReferee:
    def death_count(self, player) -> int:
        assert player == "Praxion"
        return 2

    def time_of_day(self) -> int:
        return 13000

    @staticmethod
    def is_night_time(ticks: int) -> bool:
        return 12542 <= ticks % 24000 <= 23460

    def first_night_survived(self):
        return None


def test_the_report_comes_from_the_referee() -> None:
    assert autonomy.survival_report(FakeReferee()) == {
        "deaths": 2,
        "time_of_day": 13000,
        "is_night": True,
        "first_night_survived": None,
    }

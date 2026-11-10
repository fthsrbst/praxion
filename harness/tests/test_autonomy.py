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


def _line(event: dict) -> str:
    import json

    return json.dumps(event) + "\n"


def test_the_desync_watch_reports_a_world_desync(tmp_path) -> None:
    log = tmp_path / "events.jsonl"
    watch = autonomy.DesyncWatch(log)
    assert watch.poll() is None, "no log yet is not a desync"
    log.write_text(_line({"seq": 1, "tick": 4, "kind": "wake_brain", "cause": "hunger", "detail": ""}))
    assert watch.poll() is None
    with log.open("a") as handle:
        handle.write(
            _line({"seq": 2, "tick": 9, "kind": "world_desync", "reason": "explode left 1 of 83 bytes unread"})
        )
    assert watch.poll() == "explode left 1 of 83 bytes unread"


def test_the_desync_watch_reads_only_new_complete_lines(tmp_path) -> None:
    log = tmp_path / "events.jsonl"
    log.write_text(_line({"seq": 1, "kind": "note", "text": "x"}))
    watch = autonomy.DesyncWatch(log)
    assert watch.poll() is None
    desync = _line({"seq": 2, "kind": "world_desync", "reason": "tee gap"})
    with log.open("a") as handle:
        handle.write(desync[:20])  # the body is still writing this line
    assert watch.poll() is None, "a partial line waits for the next poll"
    with log.open("a") as handle:
        handle.write(desync[20:])
    assert watch.poll() == "tee gap"
    assert watch.poll() is None, "an event already read is not reported again"

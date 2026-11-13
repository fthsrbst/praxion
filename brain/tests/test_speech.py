"""speech.py: Turkish templates, truncation, rate limits, evidence closing."""

from __future__ import annotations

from praxion_brain import speech
from praxion_brain.port import PortError
from praxion_brain.speech import Narrator


class Sink:
    def __init__(self) -> None:
        self.lines: list[tuple[str, dict]] = []

    def write(self, kind, **fields):
        self.lines.append((kind, fields))

    def kinds(self):
        return [kind for kind, _ in self.lines]


class Clock:
    now = 100.0

    def __call__(self):
        return self.now


def narrator(*, supported=True, fail=False):
    sink, clock, sent = Sink(), Clock(), []

    def say(text, priority):
        if fail:
            raise PortError("closed")
        sent.append((text, priority))
        return f"s-{len(sent):04d}"

    return Narrator(say_fn=say, supported=lambda: supported, evidence=sink, clock=clock), sink, clock, sent


def test_templates_are_turkish_and_filled_from_named_facts() -> None:
    assert speech.goal_done("8 taş topla", 820) == "bitti: 8 taş topla, 41 sn"
    assert speech.goal_failed("kömür al", "not_found", 920) == "olmadı: kömür al (bulamadım), 46 sn"
    assert speech.goal_failed("x", "weird_kind", None) == "olmadı: x (weird_kind)"
    assert speech.goal_started("fırın yap", "has(furnace)", reason="furnace basamağı") == (
        "sıradaki: fırın yap (has(furnace)) - neden: furnace basamağı"
    )
    assert speech.task_accepted("8 taş topla", "count(cobblestone) >= 8").startswith("tamam: 8 taş topla")
    assert speech.refused("already_met", "elinde 9 cobblestone var") == (
        "bunu yapmıyorum: zaten var (elinde 9 cobblestone var)"
    )
    assert speech.cannot_understand("blah").startswith("anlayamadım: blah.")
    assert speech.learning_started("get_furnace") == "öğreniyorum: get_furnace"
    assert speech.goal_stopped("x") == "durdurdum: x"
    assert speech.night_work() == (
        "Gece oldu; kazmam var, yeraltında çalışmaya devam ediyorum."
    )


def test_a_skill_task_is_accepted_without_empty_parentheses() -> None:
    assert speech.task_accepted("köy bul", "") == "tamam: köy bul."
    assert speech.task_accepted("köy bul", "", learning=True) == (
        "tamam: köy bul. Önce öğrenmem gerek."
    )
    assert speech.task_accepted("8 taş topla", "count(cobblestone) >= 8") == (
        "tamam: 8 taş topla (count(cobblestone) >= 8)."
    )


def test_cannot_understand_carries_the_models_unsupported_detail() -> None:
    line = speech.cannot_understand("ev yap", "ev yapmayı bilmiyorum")
    assert line == (
        "anlayamadım: ev yap. ev yapmayı bilmiyorum. "
        "Şunu dene: 8 taş getir, odun topla, durum, dur"
    )
    assert speech.cannot_understand("ev yap", "") == speech.cannot_understand("ev yap")


def test_bad_args_has_a_turkish_refusal() -> None:
    assert speech.REFUSALS_TR["bad_args"] == "argümanları anlayamadım"
    assert speech.refused("bad_args") == "bunu yapmıyorum: argümanları anlayamadım"


def test_a_line_is_one_clean_line_under_the_cap() -> None:
    long = "kelime " * 80
    line = speech.clean("a\nb\t" + long)
    assert len(line) <= speech.MAX_LINE_CHARS
    assert "\n" not in line and "\t" not in line
    assert not line.endswith("kelim"), "cut at a word boundary"


def test_a_line_is_pending_until_the_body_closes_it() -> None:
    n, sink, _clock, sent = narrator()
    assert n.say("done", "bitti: x", priority="normal", key="g-1") == "sent"
    assert sent == [("bitti: x", "normal")]
    assert sink.lines[-1][1]["status"] == "send_pending"
    assert n.open_lines == {"s-0001": "bitti: x"}
    assert n.on_event({"kind": "chat_sent", "id": "s-0001", "text": "bitti: x"})
    assert sink.kinds()[-1] == "narrate_sent" and not n.open_lines
    n.say("done", "ikinci", priority="urgent")
    assert n.on_event({"kind": "chat_dropped", "id": "s-0002", "reason": "stale"})
    assert sink.lines[-1][0] == "narrate_dropped" and sink.lines[-1][1]["reason"] == "stale"
    assert not n.on_event({"kind": "note"})


def test_a_body_owned_line_is_not_a_narration_claim() -> None:
    # The body answers `durum` itself with an id of its own (`b-0001`); the
    # narrator never opened it, so it must not write `narrate_sent` (review
    # finding 10).
    n, sink, _clock, _sent = narrator()
    before = len(sink.lines)
    assert not n.on_event({"kind": "chat_sent", "id": "b-0001", "text": "durum: boştayım"})
    assert not n.on_event({"kind": "chat_dropped", "id": "b-0002", "reason": "stale"})
    assert len(sink.lines) == before


def test_normal_lines_are_limited_and_the_newest_per_goal_wins() -> None:
    n, sink, clock, sent = narrator()
    assert n.say("start", "sıradaki: a", key="g-1") == "sent"
    assert n.say("start", "sıradaki: b", key="g-2") == "queued"
    # The goal settled before its slot: the done line replaces the pending start.
    assert n.say("done", "bitti: b", key="g-2") == "queued"
    n.pump()
    assert len(sent) == 1, "still inside the 8 s gap"
    clock.now += 8.5
    n.pump()
    assert [text for text, _ in sent] == ["sıradaki: a", "bitti: b"]


def test_urgent_never_exceeds_its_quota_and_droppable_is_dropped() -> None:
    n, sink, clock, sent = narrator()
    assert n.say("stop", "durdurdum: a", priority="urgent") == "sent"
    assert n.say("stop", "durdurdum: b", priority="urgent") == "queued"
    clock.now += 2.1
    n.pump()
    assert len(sent) == 2
    assert n.say("progress", "p1", priority="droppable") == "sent"
    assert n.say("progress", "p2", priority="droppable") == "dropped"
    assert "narrate_dropped" in sink.kinds()


def test_without_the_say_capability_lines_stay_in_the_console() -> None:
    n, sink, _clock, sent = narrator(supported=False)
    assert n.say("done", "bitti: x") == "unavailable"
    assert n.say("done", "bitti: y") == "unavailable"
    assert not sent
    assert sink.kinds().count("narration_unavailable") == 1, "reported once"
    assert sink.kinds().count("narrate") == 2


def test_a_port_error_is_recorded_not_raised() -> None:
    n, sink, _clock, _sent = narrator(fail=True)
    assert n.say("done", "bitti: x") == "dropped"
    assert "narrate_failed" in sink.kinds()


def test_plan_thought_is_the_planners_sentence_bounded_to_one_line() -> None:
    assert speech.plan_thought("Önce odun toplayacağım.") == "Önce odun toplayacağım."
    long = speech.plan_thought("Odun toplayacağım, " * 40)
    assert len(long) <= speech.MAX_LINE_CHARS and long.endswith("…")
    assert speech.plan_thought("iki\nsatır\x07") == "iki satır"

"""What the bot says in chat, and how often (talk slice 1).

Decision 2026-11-09 "Five next areas": the bot speaks Turkish. The design is
``logs/mac1109/design/talk.out.md`` section 3.2 with its critique:

* every line is a code template filled only from facts that exist (finding 11);
  the data source of each template is named in its docstring;
* all rate limiting and coalescing live here, in one place (finding 4); the body
  only enforces a flood guard, a length cap and staleness;
* an evidence line is opened when a line is sent and closed by the body's
  ``chat_sent`` / ``chat_dropped`` event (finding 9), so the log says which
  claims reached chat;
* urgent lines are never sent past their quota: they queue for the next slot
  (finding 14, the strict reading).

Nothing here decides *when* to speak: the loop calls the template, the
:class:`Narrator` decides whether the line goes out now.
"""

from __future__ import annotations

import re
import time
from collections import deque
from typing import Any, Callable, Mapping, Optional

from .port import PortError

#: Chat's wire limit is 256; the body refuses more than 200.
MAX_LINE_CHARS = 200
#: Ticks per second, for "41 sn".
TPS = 20

#: Failure kinds in plain Turkish. Unknown kinds are shown as they are.
FAILURES_TR = {
    "not_found": "bulamadım",
    "no_block": "blok yok",
    "unconfirmed": "doğrulanamadı",
    "budget": "süre doldu",
    "timeout": "zaman aşımı",
    "needs_table": "çalışma masası gerek",
    "no_materials": "malzeme yetmedi",
    "no_tool": "alet yok",
    "unsafe": "güvenli değildi",
    # The dig_down refusals (a drop beside the stair, water or lava behind a
    # wall, the depth cap): the body steps aside first, so these mean the
    # ground around is not diggable.
    "unsafe_drop": "düşüş güvenli değildi",
    "fluid": "su ya da lav vardı",
    "too_deep": "çok derine inecekti",
    "stuck": "takıldım",
    # `climb_out` (go_to_surface): no stair up can be cut from here, or the
    # climb ran out of ticks.
    "no_way_up": "yukarı çıkış yolu bulamadım",
    # The eyes are in water and the block outlasts the breath (`mine`, live
    # run A 2026-11-12): the dig is refused instead of drowned.
    "underwater": "su altındaydı",
    "out_of_reach": "erişemedim",
    "no_path": "yol bulamadım",
    # A preempted action restarted without one success (MAX_PREEMPTIONS in the
    # interpreter): the rule was digging the same block forever.
    "no_progress": "ilerleme olmadı",
    "skill_wrong": "beceri yanlış çalıştı",
    "stale_generation": "durum değişmişti",
    "unknown_skill": "bu beceriyi bilmiyorum",
    "stopped_by_user": "sen durdurdun",
    "died": "öldüm",
}

#: Why a request was refused, in Turkish (``commands.Resolution.reason``).
REFUSALS_TR = {
    "already_met": "zaten var",
    "blocked": "bu hedef geçici olarak engelli (üst üste başarısız oldu)",
    "malformed_condition": "koşulu anlayamadım",
    "high_risk": "pahalı ya da riskli, koşulunu açıkça yazarsan düşünürüm",
    "not_verifiable": "bittiğini doğrulayamam",
    # The autonomous path may still refuse a repeat; never leak the raw key.
    "same_as_last": "aynı görevi az önce istedim",
    "unknown_skill": "böyle bir becerim yok",
    "bad_args": "argümanları anlayamadım",
    "unsupported": "henüz yapamıyorum",
}

TRY_HINT = "Şunu dene: 8 taş getir, odun topla, durum, dur"


def clean(text: str, limit: int = MAX_LINE_CHARS) -> str:
    """One line: no control characters, collapsed spaces, cut at a word boundary
    (never inside the last word when a space is available)."""
    text = re.sub(r"\s+", " ", "".join(ch if ch.isprintable() else " " for ch in text)).strip()
    if len(text) <= limit:
        return text
    cut = text[: limit - 1]
    space = cut.rfind(" ")
    if space > limit // 2:
        cut = cut[:space]
    return cut.rstrip(" ,;:-") + "…"


def seconds(ticks: Optional[int]) -> Optional[int]:
    return None if ticks is None else max(0, round(int(ticks) / TPS))


# ---------------------------------------------------------------------- #
# Templates. Each names where its values come from.
# ---------------------------------------------------------------------- #
def goal_started(text: str, condition: str, *, reason: Optional[str] = None) -> str:
    """Autonomous goal start. ``text``/``condition``: the validated proposal;
    ``reason``: the ladder rung it serves or its origin, both known to code."""
    line = f"sıradaki: {text} ({condition})"
    if reason:
        line += f" - neden: {reason}"
    return clean(line)


def plan_thought(thought: str) -> str:
    """The planner's one-sentence thought, in its own first-person Turkish.

    It is model prose, but it is the only line whose whole point is to say what
    the bot intends (decision 2026-11-12 "Demo night"); ``clean`` bounds it.
    """
    return clean(thought)


def task_accepted(text: str, condition: str, *, learning: bool = False) -> str:
    """The operator's task was understood. ``condition`` is the validated one;
    a skill task has none, so no empty parentheses appear."""
    tail = " Önce öğrenmem gerek." if learning else ""
    detail = f" ({condition})" if condition else ""
    return clean(f"tamam: {text}{detail}.{tail}")


def goal_done(text: str, ticks: Optional[int]) -> str:
    """``GoalRecord.text`` and ticks from the settled receipt."""
    took = seconds(ticks)
    return clean(f"bitti: {text}" + (f", {took} sn" if took is not None else ""))


def goal_failed(text: str, failure_kind: Optional[str], ticks: Optional[int]) -> str:
    """``failure_kind`` is the body's typed kind (never prose from a model)."""
    kind = failure_kind or "bilinmeyen"
    plain = FAILURES_TR.get(kind, kind)
    took = seconds(ticks)
    return clean(f"olmadı: {text} ({plain})" + (f", {took} sn" if took is not None else ""))


def goal_stopped(text: str) -> str:
    return clean(f"durdurdum: {text}")


def goal_resumed(text: str) -> str:
    """A preempted goal comes back; the body restarts its skill from the top."""
    return clean(f"devam ediyorum: {text} (beceri baştan başlıyor)")


def learning_started(skill: str) -> str:
    """``skill`` is the name the admission job writes."""
    return clean(f"öğreniyorum: {skill}")


def learning_done(skill: str) -> str:
    return clean(f"öğrendim: {skill}, şimdi kullanıyorum")


def refused(reason: str, detail: Optional[str] = None) -> str:
    plain = REFUSALS_TR.get(reason, reason)
    extra = f" ({detail})" if detail and reason == "already_met" else ""
    return clean(f"bunu yapmıyorum: {plain}{extra}")


def died(cause: Optional[str]) -> str:
    """The body's ``died`` wake; ``cause`` is its detail (the death message)."""
    return clean(f"öldüm: {(cause or '').strip() or 'bilinmiyor'}")


def world_desync() -> str:
    """The body's ``world_desync`` wake: its world mirror is no longer valid."""
    return "dünyayı göremiyorum, duruyorum"


def night_work() -> str:
    """Night work starts for the first time in a night (decision 2026-11-13).

    The facts behind it are code: ``survival.night_work`` (a pickaxe held, the
    body healthy, night). Said once per night by the loop.
    """
    return "Gece oldu; kazmam var, yeraltında çalışmaya devam ediyorum."


def cannot_understand(text: str, detail: Optional[str] = None) -> str:
    """``detail`` is the map call's ``unsupported`` line, when it gave one."""
    line = f"anlayamadım: {text}."
    if detail:
        line += f" {detail.strip().rstrip('.')}."
    return clean(f"{line} {TRY_HINT}")


def paused() -> str:
    return "durdum; devam demeni bekliyorum"


def resumed() -> str:
    return "devam ediyorum"


# ---------------------------------------------------------------------- #
# The narrator
# ---------------------------------------------------------------------- #
#: (minimum seconds between two lines, lines per minute) per priority.
LIMITS: dict[str, tuple[float, int]] = {
    "urgent": (2.0, 10),
    "normal": (8.0, 4),
    "droppable": (60.0, 1),
}


class _Line:
    def __init__(self, kind: str, text: str, priority: str, key: Optional[str]) -> None:
        self.kind, self.text, self.priority, self.key = kind, text, priority, key


class Narrator:
    """Rate-limits, coalesces and sends lines; keeps the evidence honest.

    ``say_fn(text, priority) -> line_id`` writes to the port (it may raise
    :class:`PortError`); ``supported()`` says whether the body can speak. Every
    line, sent or not, is also an evidence line, so the console feed shows it.
    """

    def __init__(
        self,
        *,
        say_fn: Callable[[str, str], str],
        supported: Callable[[], bool],
        evidence: Any,
        clock: Callable[[], float] = time.monotonic,
    ) -> None:
        self._say = say_fn
        self._supported = supported
        self._evidence = evidence
        self._clock = clock
        self._sent: dict[str, deque[float]] = {name: deque() for name in LIMITS}
        self._pending: list[_Line] = []
        #: line id -> text, until the body answers (finding 9).
        self.open_lines: dict[str, str] = {}
        self._unavailable_logged = False

    # -- public ---------------------------------------------------------- #
    def say(
        self,
        kind: str,
        text: str,
        *,
        priority: str = "normal",
        key: Optional[str] = None,
    ) -> str:
        """Speak ``text``. Returns ``sent``, ``queued``, ``dropped`` or ``unavailable``."""
        text = clean(text)
        if not text:
            return "dropped"
        if priority not in LIMITS:
            raise ValueError(f"unknown priority {priority!r}")
        if not self._supported():
            if not self._unavailable_logged:
                self._unavailable_logged = True
                self._evidence.write("narration_unavailable")
            self._evidence.write("narrate", topic=kind, text=text, priority=priority, status="console_only")
            return "unavailable"
        # A newer line of the same goal supersedes an older pending one: never
        # "next: X" after "done: X" (design 3.2).
        if key is not None:
            self._pending = [line for line in self._pending if line.key != key]
        line = _Line(kind, text, priority, key)
        if self._slot_free(priority):
            return self._send(line)
        if priority == "droppable":
            self._evidence.write(
                "narrate_dropped", topic=kind, text=text, priority=priority, reason="rate"
            )
            return "dropped"
        self._pending.append(line)
        self._evidence.write("narrate_queued", topic=kind, text=text, priority=priority)
        return "queued"

    def pump(self) -> None:
        """Send queued lines whose slot has come, urgent first, oldest first."""
        if not self._pending:
            return
        for line in sorted(self._pending, key=lambda item: item.priority != "urgent"):
            if self._slot_free(line.priority):
                self._pending.remove(line)
                self._send(line)

    def on_event(self, event: Mapping[str, Any]) -> bool:
        """Close the evidence line of a ``chat_sent``/``chat_dropped`` event.

        Only an id this narrator opened counts: the body composes its own lines
        (`b-0001`, its `durum` answer), and they were never a claim of this
        narrator (review finding 10).
        """
        kind = event.get("kind")
        if kind not in ("chat_sent", "chat_dropped"):
            return False
        line_id = str(event.get("id") or "")
        text = self.open_lines.pop(line_id, None)
        if text is None:
            return False
        if kind == "chat_sent":
            self._evidence.write("narrate_sent", line_id=line_id, text=text)
        else:
            self._evidence.write(
                "narrate_dropped",
                line_id=line_id,
                text=text,
                reason=event.get("reason"),
            )
        return True

    # -- internals -------------------------------------------------------- #
    def _slot_free(self, priority: str) -> bool:
        gap, per_minute = LIMITS[priority]
        now = self._clock()
        sent = self._sent[priority]
        while sent and now - sent[0] >= 60.0:
            sent.popleft()
        if sent and now - sent[-1] < gap:
            return False
        return len(sent) < per_minute

    def _send(self, line: _Line) -> str:
        try:
            line_id = self._say(line.text, line.priority)
        except PortError as error:
            self._evidence.write(
                "narrate_failed", topic=line.kind, text=line.text, error=str(error)
            )
            return "dropped"
        self._sent[line.priority].append(self._clock())
        self.open_lines[line_id] = line.text
        # The line is a claim only once the body confirms it: until then it is
        # pending, and `narrate_sent`/`narrate_dropped` closes it.
        self._evidence.write(
            "narrate",
            topic=line.kind,
            line_id=line_id,
            text=line.text,
            priority=line.priority,
            status="send_pending",
        )
        return "sent"


__all__ = [
    "FAILURES_TR",
    "LIMITS",
    "MAX_LINE_CHARS",
    "Narrator",
    "REFUSALS_TR",
    "TRY_HINT",
    "cannot_understand",
    "clean",
    "died",
    "goal_done",
    "goal_failed",
    "goal_resumed",
    "goal_started",
    "goal_stopped",
    "learning_done",
    "learning_started",
    "night_work",
    "paused",
    "plan_thought",
    "refused",
    "resumed",
    "seconds",
    "task_accepted",
    "world_desync",
]

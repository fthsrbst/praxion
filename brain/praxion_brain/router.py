"""The Jev router: play a known skill, write a new one, or ask the writer.

One TypeSafe System One request answers one yes/no question per known skill.
Code, not Jev, makes the decision; a Jev API error surfaces as an error and is
never read as "no".
"""

from __future__ import annotations

import json
from typing import Literal

from pydantic import BaseModel, Field
from typesafe_sdk import Noul, TypeSafeClient

from . import config
from .evidence import Evidence
from .goal import Goal
from .ledger import Ledger, estimate_cost
from .skills import SkillCatalog

#: A single skill above this probability plays.
PLAY_THRESHOLD = 0.7
#: Every skill below this probability means the goal needs a new skill.
WRITE_THRESHOLD = 0.3
#: The planner's own named skill plays at or above this probability: the
#: planner already chose it with the arguments in view, so Jev only vetoes. The
#: bar is the write threshold (below it Jev says "no"). At 0.5 a correct choice
#: was thrown away live: `hunt(pig, porkchop)` for `count(porkchop) >= 2`
#: scored 0.49, because Jev sees the skill's doc but not its arguments, and the
#: loop went to write `get_porkchop` instead (run B, seed 37960, 2026-11-12).
HINT_THRESHOLD = WRITE_THRESHOLD
#: The Jev model the router calls and the loop estimates with.
DEFAULT_MODEL = "jev-latest"
#: Child skills that other skills call (``do cut_log(`` ...). They are never a
#: goal's own skill: Jev scored the helper ``cut_log`` level with ``get_wood``
#: and the helper once won (run A, seed 584146). Hidden from routing and from
#: the planner's skill list.
PUBLIC_EXCLUDE = frozenset(
    {"cut_log", "hunt_food", "get_crafting_table_near", "search_surface", "find_stone"}
)


class Route(BaseModel):
    kind: Literal["play", "write", "ask"]
    skill: str | None = None
    scores: dict[str, float] = Field(default_factory=dict)


def public_skills(skills: SkillCatalog) -> SkillCatalog:
    """The catalog without the helper skills of :data:`PUBLIC_EXCLUDE`."""
    return SkillCatalog(
        skills=[skill for skill in skills.skills if skill.name not in PUBLIC_EXCLUDE]
    )


def question_text(skill: str, goal: Goal) -> str:
    return f"Does running skill `skills.{skill}` give the player `{goal.item}`?"


def decide(scores: dict[str, float]) -> tuple[str, str | None]:
    """Turn the probabilities into ``(kind, skill)``."""
    high = [name for name, score in scores.items() if score > PLAY_THRESHOLD]
    if len(high) == 1:
        return "play", high[0]
    if len(high) > 1:
        # Highest score wins; the name breaks a tie deterministically.
        return "play", max(high, key=lambda name: (scores[name], name))
    if all(score < WRITE_THRESHOLD for score in scores.values()):
        return "write", None
    return "ask", None


def route(
    goal: Goal,
    skills: SkillCatalog,
    *,
    client: TypeSafeClient | None = None,
    ledger: Ledger | None = None,
    evidence: Evidence | None = None,
    model: str = DEFAULT_MODEL,
    hint: str | None = None,
) -> Route:
    """Ask Jev about every public skill and decide from the probabilities.

    Code decides first where code can: a known skill named exactly
    ``get_<item>`` plays without a Jev call. ``hint`` is the skill the planner
    named: it plays when Jev scores it at :data:`HINT_THRESHOLD` or more, and
    the normal decision runs otherwise.
    """
    ledger = ledger if ledger is not None else Ledger()
    evidence = evidence if evidence is not None else Evidence(config.local_brain_dir())

    # Helpers are never a goal's own skill; only public skills are considered.
    skills = public_skills(skills)

    # `oak_planks` is served by `get_planks`: try the item, then its last word.
    for name in (f"get_{goal.item}", f"get_{goal.item.rsplit('_', 1)[-1]}"):
        if name in skills.names:
            evidence.write(
                "router_decision",
                decision="play",
                skill=name,
                scores={},
                hint=hint,
                source="exact_name",
            )
            return Route(kind="play", skill=name, scores={})

    # Jev sees what a person would look at: the doc line and the rules, which
    # name what the skill crafts or mines. With the doc alone, `get_planks`
    # scored 0.21 for "oak planks" and the brain wrote a duplicate skill
    # (first autonomous run, 2026-11-08).
    state = {
        "goal": goal.text,
        "item": goal.item,
        "skills": {
            skill.name: {"doc": skill.doc, "rules": skill.source} for skill in skills.skills
        },
    }
    questions = {
        skill.name: Noul(instructions=question_text(skill.name, goal))
        for skill in skills.skills
    }
    if not questions:
        evidence.write("router_empty", reason="no known skills", goal=goal.model_dump())
        return Route(kind="write", skill=None, scores={})

    payload = json.dumps(
        {
            "state": state,
            "questions": {name: question.instructions for name, question in questions.items()},
        },
        sort_keys=True,
    )
    input_estimate = max(1, len(payload) // 4)
    ledger.ensure("typesafe", estimate_cost(model, input_estimate, 0))
    evidence.write(
        "router_request",
        provider="typesafe",
        model=model,
        state=state,
        questions={name: question.instructions for name, question in questions.items()},
    )

    if client is None:
        with TypeSafeClient() as created:
            response = created.system_one(state=state, questions=questions)
    else:
        response = client.system_one(state=state, questions=questions)

    noul_answers = getattr(response, "nouls", {})
    missing = sorted(set(questions) - set(noul_answers))
    if missing:
        raise RuntimeError(f"Jev did not answer for skills: {', '.join(missing)}")
    scores = {name: float(noul_answers[name].noul) for name in questions}

    usage = getattr(response, "usage", None)
    input_tokens = int(getattr(usage, "input_tokens", 0) or 0)
    output_tokens = int(getattr(usage, "output_tokens", 0) or 0)
    evidence.write(
        "router_response",
        provider="typesafe",
        model=getattr(response, "model", model),
        scores=scores,
        input_tokens=input_tokens,
        output_tokens=output_tokens,
    )
    ledger.record("typesafe", model, input_tokens, output_tokens, "route")

    if hint is not None and scores.get(hint, 0.0) >= HINT_THRESHOLD:
        kind, skill, source = "play", hint, "hint"
    else:
        kind, skill = decide(scores)
        source = "jev"
    result = Route(kind=kind, skill=skill, scores=scores)
    evidence.write(
        "router_decision",
        decision=kind,
        skill=skill,
        scores=scores,
        hint=hint,
        source=source,
    )
    return result


__all__ = [
    "DEFAULT_MODEL",
    "HINT_THRESHOLD",
    "PLAY_THRESHOLD",
    "PUBLIC_EXCLUDE",
    "WRITE_THRESHOLD",
    "Route",
    "decide",
    "public_skills",
    "question_text",
    "route",
]

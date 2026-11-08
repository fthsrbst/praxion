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


class Route(BaseModel):
    kind: Literal["play", "write", "ask"]
    skill: str | None = None
    scores: dict[str, float] = Field(default_factory=dict)


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
    model: str = "jev-latest",
) -> Route:
    """Ask Jev about every known skill and decide from the probabilities."""
    ledger = ledger if ledger is not None else Ledger()
    evidence = evidence if evidence is not None else Evidence(config.local_brain_dir())

    state = {
        "goal": goal.text,
        "item": goal.item,
        "skills": {skill.name: {"doc": skill.doc} for skill in skills.skills},
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

    kind, skill = decide(scores)
    result = Route(kind=kind, skill=skill, scores=scores)
    evidence.write(
        "router_decision",
        decision=kind,
        skill=skill,
        scores=scores,
    )
    return result


__all__ = [
    "PLAY_THRESHOLD",
    "WRITE_THRESHOLD",
    "Route",
    "decide",
    "question_text",
    "route",
]

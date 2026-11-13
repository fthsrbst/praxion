"""The planning call: a short queue of goals and one sentence of thinking.

Decision 2026-11-12 "Demo night": one model call returns 1 to 5 goals in order
plus a first-person thought, and the loop plays the queue goal by goal, validating
each goal in code when its turn comes. This replaces the one-goal-per-call
proposal of :mod:`praxion_brain.proposer`, which left the body idle for the
length of every call.

The prompt keeps the stable, cacheable part (rules, primitives, facts and the
public skills) as the system prefix and the briefing as the only user message.
The planner model is the default; after two invalid plans in a row, or for the
first plan after a death, the call escalates to the stronger model. Every request
and response goes to :class:`~praxion_brain.evidence.Evidence` and its cost to the
:class:`~praxion_brain.ledger.Ledger`, exactly like the writer and the proposer.

Code validation of the goals is :func:`praxion_brain.curriculum.validate` and
stays outside this module (the loop runs it per goal).

Spec: ``docs/proposals/2026-11-12-demo-night.md``.
"""

from __future__ import annotations

from typing import Any, Optional

from pydantic import BaseModel, Field

from . import config
from .evidence import Evidence
from .ledger import Ledger, estimate_cost
from .router import PUBLIC_EXCLUDE, public_skills
from .skills import SkillCatalog, fact_names, primitive_names

# ---------------------------------------------------------------------- #
# Prompt text. Keep every fixed string here so the prompt can be audited.
# ---------------------------------------------------------------------- #
PLAN_RULES = """\
You plan the next goals of Praxion, a Minecraft agent that plays through skills.
Praxion plays Minecraft survival like a competent human player. Its aims:

- stay alive;
- reach iron tools: coal, raw iron, a furnace, iron ingots, an iron pickaxe;
- make torches and set up a camp: a crafting table, a furnace, a chest and
  torches, all placed at one spot;
- keep food.

There is NO fixed order. Choose the order yourself from the situation: the
inventory, health and food, the time of day, what is nearby, what failed
recently and the persona in the briefing. A persona is a play style; let it
shape the order and the risks you take, never the rules below. The ladder in
the briefing is only a yardstick of progress, not a required sequence.

## The plan

- Answer with 1 to 5 goals, in the order they are played.
- Every goal must be reachable now, or after the earlier goals of the same
  plan. The code plays them one after another and re-checks each goal when its
  turn comes; the plan is dropped and made again after a failure, a death, a
  survival interruption or an operator task.
- Never repeat a goal that the briefing marks as blocked or as failed
  recently; plan something else, or a different way to the same end.
- Do not gather what the inventory already covers. Count what the plan itself
  will produce: do not plan a second stock of an item an earlier goal of the
  same plan already makes.
- When the briefing names a skill being written, its goal cannot be played yet:
  plan something else.
- A `## Night` section in the briefing, when present, limits the plan to the
  skills it lists: plan only those skills, until morning.

## One goal

- `text` is one short imperative sentence, for example "get a furnace".
- `skill` is the exact name of a known skill that serves this goal, with
  `args` its arguments as text (for example `["4"]`), or `null` when no known
  skill fits: a new skill is then written for the goal. Never invent a skill
  name; use only the names under "Known skills". A skill name that is not
  listed there is an internal helper, not a goal: never name it, even when it
  shows up in the briefing or in the history of recent goals.
- `condition` is guard text over inventory facts: `has(item)`,
  `count(item) >= n`, joined with `and`/`not` and parentheses. Use the item ids
  the skills use; for wood use the tags (`#logs`, `#planks`), never one variant
  such as `oak_planks`. The body verifies the goal with it after the skill
  settles.
- The condition must be false now, or become false-then-true through the plan
  order: a goal that needs an item an earlier goal of the same plan makes may
  name it. Prefer the goal's own item (`has(wooden_pickaxe)`) over an
  intermediate ingredient: the skills gather their ingredients themselves.
- `condition` may be `""` only for a goal that names a known skill whose own
  success is the goal (finding something, moving, hunting). The skill's `done`
  then settles it.
- `rationale` is at most 200 characters and is only logged.

## Underground

`## Now` shows `pos x,y,z`. When y is under 55 and `## Surroundings` shows no
logs or animals, the body is below ground (it dug a tunnel or fell into a
cave), and wood, food, animals, villages and exploring all fail from there.
The first goal of a plan that needs the surface is then `go_to_surface`, with
`condition` `""`; plan the surface goals after it. A plan that only mines
underground does not need it. At night the `## Night` section decides: the body
stays down.

## The thought

`thought` is ONE short Turkish sentence in the first person (at most 200
characters) saying what Praxion intends and why. It is spoken aloud to the
operator, so write it as speech, for example "Önce odun toplayıp bir çalışma
masası yapacağım, çünkü gece yaklaşıyor."

Answer with the structured object only; do not write prose or code.
"""

DEFAULT_MODEL = "gpt-6.1-terra"
DEFAULT_ESCALATION_MODEL = "gpt-6.1-sol"
#: Reasoning effort of the default planner call. Measured 2026-11-12 on one
#: briefing: `gpt-6.1-terra` answered in 9.6 s with "low" and 4.6 s with "none",
#: with plans of the same quality; the body stands still for the whole call.
DEFAULT_EFFORT = "none"
#: The escalated call (two invalid plans, or the first plan after a death) is
#: the one place where thinking longer is worth the wait.
ESCALATION_EFFORT = "low"
#: Estimated input tokens of one planning call, used before the call.
INPUT_ESTIMATE = 3_000
#: Estimated output tokens of one plan (used before the call).
OUTPUT_ESTIMATE = 700
#: Escalate after this many invalid plans in a row.
ESCALATE_AFTER = 2
#: The most goals one plan may hold; a longer plan keeps its first goals.
MAX_GOALS = 5


class PlannedGoal(BaseModel):
    """One goal of a plan (the contract of ``2026-11-12-demo-night.md``)."""

    text: str
    #: Guard text; ``""`` only for a skill-verified goal.
    condition: str
    #: An exact known public skill name, or ``None``.
    skill: Optional[str] = None
    #: The skill's arguments, as text.
    args: list[str] = Field(default_factory=list)
    #: At most 200 characters; the loop cuts a longer one before logging it.
    rationale: str = ""


class Plan(BaseModel):
    """Structured output of the planning call."""

    #: One Turkish sentence, narrated in chat and the console.
    thought: str
    #: 1 to 5 goals, in play order; the loop checks the shape.
    goals: list[PlannedGoal]


# ---------------------------------------------------------------------- #
def _skill_lines(catalog: SkillCatalog) -> str:
    lines = []
    for skill in public_skills(catalog).skills:
        params = ", ".join(str(param) for param in skill.params)
        line = f"{skill.name}({params})"
        if skill.doc:
            line += f" - {skill.doc}"
        lines.append(line)
    return "\n".join(lines) if lines else "(none)"


def public_briefing(briefing: str) -> str:
    """The briefing without the catalog lines of the helper skills.

    The briefing's ``## Skills`` section lists the whole catalog, helpers
    included, and the planner named them (run D, 2026-11-12: ``cut_log``,
    ``hunt_food`` and ``search_surface``, eight steps skipped). Only the lines
    of that section are cut; a helper named elsewhere (a recent goal) stays.
    """
    kept: list[str] = []
    in_skills = False
    for line in briefing.splitlines():
        if line.startswith("## "):
            in_skills = line.startswith("## Skills")
        elif in_skills and any(line.startswith(f"{name}(") for name in PUBLIC_EXCLUDE):
            continue
        kept.append(line)
    text = "\n".join(kept)
    return text + "\n" if briefing.endswith("\n") else text


def system_prompt(catalog: SkillCatalog) -> str:
    """The stable system prefix: rules, primitives, facts and public skills."""
    primitives = ", ".join(primitive_names(catalog)) or "none"
    facts = ", ".join(fact_names(catalog)) or "none"
    return (
        f"{PLAN_RULES}\n"
        "## Primitives used by the known skills\n\n"
        f"{primitives}\n\n"
        "## Facts available (the ones the known skills use)\n\n"
        f"{facts}\n\n"
        "## Known skills\n\n"
        f"{_skill_lines(catalog)}\n"
    )


def model_for(
    invalid_count: int,
    after_death: bool,
    model: str,
    escalation_model: Optional[str],
) -> str:
    """The model a call uses: escalate after two invalid plans or after a death."""
    if escalation_model and (invalid_count >= ESCALATE_AFTER or after_death):
        return escalation_model
    return model


def estimate(
    invalid_count: int = 0,
    after_death: bool = False,
    model: str = DEFAULT_MODEL,
    escalation_model: Optional[str] = DEFAULT_ESCALATION_MODEL,
) -> float:
    """The governor's pre-call estimate, at the model this call will really use.

    The escalation model is twice as dear, so a fixed cheap-model estimate would
    let the budget brake miss the real price (A5).
    """
    chosen = model_for(invalid_count, after_death, model, escalation_model)
    return estimate_cost(chosen, INPUT_ESTIMATE, OUTPUT_ESTIMATE)


def shape_problem(plan: Plan) -> Optional[str]:
    """Why ``plan`` has no usable shape, or ``None`` when it does.

    More than :data:`MAX_GOALS` goals is not a problem: the loop plays the first
    ones. An empty plan, or a goal without text, is.
    """
    if not plan.goals:
        return "no_goals"
    if any(not goal.text.strip() for goal in plan.goals[:MAX_GOALS]):
        return "empty_text"
    return None


def _usage_tokens(usage: Any) -> tuple[int, int]:
    return (
        int(getattr(usage, "input_tokens", 0) or 0),
        int(getattr(usage, "output_tokens", 0) or 0),
    )


def plan(
    briefing: str,
    catalog: SkillCatalog,
    *,
    invalid_count: int = 0,
    after_death: bool = False,
    model: str = DEFAULT_MODEL,
    escalation_model: Optional[str] = DEFAULT_ESCALATION_MODEL,
    client: Any | None = None,
    ledger: Ledger | None = None,
    evidence: Evidence | None = None,
    effort: Optional[str] = None,
) -> Plan:
    """Ask the planner model for a plan, logging the exchange and cost."""
    ledger = ledger if ledger is not None else Ledger()
    evidence = evidence if evidence is not None else Evidence(config.local_brain_dir())
    chosen = model_for(invalid_count, after_death, model, escalation_model)
    if effort is None:
        effort = ESCALATION_EFFORT if chosen != model else DEFAULT_EFFORT

    system = system_prompt(catalog)
    briefing = public_briefing(briefing)
    input_estimate = max(1, (len(system) + len(briefing)) // 4)
    ledger.ensure("openai", estimate_cost(chosen, input_estimate, OUTPUT_ESTIMATE))
    evidence.write(
        "plan_request",
        provider="openai",
        model=chosen,
        invalid_count=invalid_count,
        after_death=after_death,
        system=system,
        briefing=briefing,
    )

    if client is None:
        from openai import OpenAI

        client = OpenAI()
    response = client.responses.parse(
        model=chosen,
        instructions=system,
        input=[{"role": "user", "content": briefing}],
        text_format=Plan,
        reasoning={"effort": effort},
    )

    parsed = getattr(response, "output_parsed", None)
    input_tokens, output_tokens = _usage_tokens(getattr(response, "usage", None))
    evidence.write(
        "plan_response",
        provider="openai",
        model=chosen,
        invalid_count=invalid_count,
        after_death=after_death,
        parsed=parsed.model_dump() if isinstance(parsed, BaseModel) else parsed,
        input_tokens=input_tokens,
        output_tokens=output_tokens,
        response_id=getattr(response, "id", None),
    )
    ledger.record("openai", chosen, input_tokens, output_tokens, "plan")

    if parsed is None:
        raise RuntimeError(
            f"the planning call returned no parsed Plan (response "
            f"{getattr(response, 'id', 'unknown')})"
        )
    if not isinstance(parsed, Plan):
        parsed = Plan.model_validate(parsed)
    return parsed


__all__ = [
    "DEFAULT_ESCALATION_MODEL",
    "DEFAULT_MODEL",
    "ESCALATE_AFTER",
    "INPUT_ESTIMATE",
    "MAX_GOALS",
    "OUTPUT_ESTIMATE",
    "PLAN_RULES",
    "Plan",
    "PlannedGoal",
    "estimate",
    "model_for",
    "plan",
    "public_briefing",
    "shape_problem",
    "system_prompt",
]

"""The curriculum proposal call: one structured goal per pick.

The prompt follows proposal section 4.3: a stable, cacheable system prefix (the
game rules, the primitive catalog and the known skills) and the briefing as the
only user message. The cheap writer model is the default; after two invalid
proposals the caller passes ``invalid_count >= 2`` and the call escalates to a
stronger model. Every request and response goes to
:class:`~praxion_brain.evidence.Evidence` and its cost to the
:class:`~praxion_brain.ledger.Ledger`, exactly like the writer.

The structured output is :class:`~praxion_brain.curriculum.GoalProposal`; code
validation is :func:`praxion_brain.curriculum.validate` and stays outside this
module.

Spec: ``docs/proposals/2026-11-08-demo2-loop.md`` section 4.3.
"""

from __future__ import annotations

from typing import Any

from pydantic import BaseModel

from . import config
from .curriculum import GoalProposal
from .evidence import Evidence
from .ledger import Ledger, estimate_cost
from .skills import SkillCatalog, fact_names, primitive_names

# ---------------------------------------------------------------------- #
# Prompt text. Keep every fixed string here so the prompt can be audited.
# ---------------------------------------------------------------------- #
PROPOSAL_RULES = """\
You plan the next goal of Praxion, a Minecraft agent that plays through skills.

- Propose exactly ONE next goal.
- The goal must be reachable with the known skills plus at most one new skill.
- Prefer the first unmet ladder rung, unless a prerequisite for it is missing.
- Never repeat a goal that the briefing marks as blocked.
- When the briefing names a skill being written, its goal cannot be played
  yet: propose a different goal that the known skills reach, preferably one
  that stocks what the next rungs need (more cobblestone, logs, a spare tool).
- `condition` is guard text over inventory facts: `has(item)`,
  `count(item) >= n`, joined with `and`/`not` and parentheses. Use the item
  ids the skills and ladder use; for wood use the tags (`#logs`, `#planks`),
  never one variant such as `oak_planks`.
- Prefer the ladder rung's own item as the goal (`has(wooden_pickaxe)`), not an
  intermediate ingredient: the skills gather their ingredients themselves.
- The condition must be false in the current world. Its truth is how the body
  verifies the goal after the skill settles.
- `text` is one short imperative sentence, for example "get a furnace".
- `rung` is the ladder rung this goal serves, or omitted when none fits.
- `rationale` is at most 200 characters and is only logged.
- Answer with the structured object only; do not write prose or code.
"""

DEFAULT_MODEL = "gpt-6-luna"
DEFAULT_ESCALATION_MODEL = "gpt-6.1-terra"
#: Estimated input tokens of one proposal call, used before the call.
INPUT_ESTIMATE = 2_500
#: Estimated output tokens of one proposal (used before the call).
OUTPUT_ESTIMATE = 300
#: Escalate after this many invalid proposals (proposal section 4.3).
ESCALATE_AFTER = 2


# ---------------------------------------------------------------------- #
def _skill_lines(catalog: SkillCatalog) -> str:
    lines = []
    for skill in catalog.skills:
        params = ", ".join(str(param) for param in skill.params)
        line = f"{skill.name}({params})"
        if skill.doc:
            line += f" - {skill.doc}"
        lines.append(line)
    return "\n".join(lines) if lines else "(none)"


def system_prompt(catalog: SkillCatalog) -> str:
    """The stable system prefix: rules, primitives, facts and known skills."""
    primitives = ", ".join(primitive_names(catalog)) or "none"
    facts = ", ".join(fact_names(catalog)) or "none"
    return (
        f"{PROPOSAL_RULES}\n"
        "## Primitives used by the known skills\n\n"
        f"{primitives}\n\n"
        "## Facts available (the ones the known skills use)\n\n"
        f"{facts}\n\n"
        "## Known skills\n\n"
        f"{_skill_lines(catalog)}\n"
    )


def model_for(invalid_count: int, model: str, escalation_model: str | None) -> str:
    """The model a call uses: escalate after two invalid proposals."""
    if escalation_model and invalid_count >= ESCALATE_AFTER:
        return escalation_model
    return model


def estimate(
    invalid_count: int = 0,
    model: str = DEFAULT_MODEL,
    escalation_model: str | None = DEFAULT_ESCALATION_MODEL,
) -> float:
    """The governor's pre-call estimate, at the model this call will really use.

    ``propose`` escalates to the stronger, ten times dearer model after two
    invalid proposals, so a fixed cheap-model estimate would let the budget
    brake miss the real price (A5).
    """
    chosen = model_for(invalid_count, model, escalation_model)
    return estimate_cost(chosen, INPUT_ESTIMATE, OUTPUT_ESTIMATE)


def _usage_tokens(usage: Any) -> tuple[int, int]:
    return (
        int(getattr(usage, "input_tokens", 0) or 0),
        int(getattr(usage, "output_tokens", 0) or 0),
    )


def propose(
    briefing: str,
    catalog: SkillCatalog,
    *,
    invalid_count: int = 0,
    model: str = DEFAULT_MODEL,
    escalation_model: str | None = DEFAULT_ESCALATION_MODEL,
    client: Any | None = None,
    ledger: Ledger | None = None,
    evidence: Evidence | None = None,
    effort: str = "low",
) -> GoalProposal:
    """Ask the curriculum model for one goal, logging the exchange and cost."""
    ledger = ledger if ledger is not None else Ledger()
    evidence = evidence if evidence is not None else Evidence(config.local_brain_dir())
    chosen = model_for(invalid_count, model, escalation_model)

    system = system_prompt(catalog)
    input_estimate = max(1, (len(system) + len(briefing)) // 4)
    ledger.ensure("openai", estimate_cost(chosen, input_estimate, OUTPUT_ESTIMATE))
    evidence.write(
        "proposal_request",
        provider="openai",
        model=chosen,
        invalid_count=invalid_count,
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
        text_format=GoalProposal,
        reasoning={"effort": effort},
    )

    parsed = getattr(response, "output_parsed", None)
    input_tokens, output_tokens = _usage_tokens(getattr(response, "usage", None))
    evidence.write(
        "proposal_response",
        provider="openai",
        model=chosen,
        invalid_count=invalid_count,
        parsed=parsed.model_dump() if isinstance(parsed, BaseModel) else parsed,
        input_tokens=input_tokens,
        output_tokens=output_tokens,
        response_id=getattr(response, "id", None),
    )
    ledger.record("openai", chosen, input_tokens, output_tokens, "propose_goal")

    if parsed is None:
        raise RuntimeError(
            f"the proposal call returned no parsed GoalProposal (response "
            f"{getattr(response, 'id', 'unknown')})"
        )
    if not isinstance(parsed, GoalProposal):
        parsed = GoalProposal.model_validate(parsed)
    return parsed


__all__ = [
    "DEFAULT_ESCALATION_MODEL",
    "DEFAULT_MODEL",
    "ESCALATE_AFTER",
    "INPUT_ESTIMATE",
    "OUTPUT_ESTIMATE",
    "PROPOSAL_RULES",
    "estimate",
    "model_for",
    "propose",
    "system_prompt",
]

"""The LLM skill writer.

The prompt is split in two: a stable, cacheable system prefix (the DSL rules,
the full text of every core skill and the primitive/fact list taken
from them) and a small task message (the goal, the required skill name and, for
a repair, the previous source, the typed failure and its trace digest).

Every request and response is written to :class:`~praxion_brain.evidence.Evidence`
and its token cost to the :class:`~praxion_brain.ledger.Ledger`.
"""

from __future__ import annotations

from typing import Any

from pydantic import BaseModel

from . import config
from .evidence import Evidence
from .goal import Goal
from .ledger import Ledger, estimate_cost
from .skills import SkillCatalog, catalog_text, fact_names, primitive_names

# ---------------------------------------------------------------------- #
# Prompt text. Keep every fixed string here so the prompt can be audited.
# ---------------------------------------------------------------------- #
DSL_RULES = """\
You write exactly one Praxion skill in the .skill rule-list DSL.

Shape:
  skill <name>(<params>)
    doc "<one line>"
    rules:
      <guard> -> <action>
      ...

Rules of the DSL:
- A skill is a list of rules `guard -> action`, the top rule first.
- Each tick the first rule whose guard is true fires; a later rule never fires
  while an earlier one is true.
- `done` ends the skill successfully; `fail(<kind>)` ends it with a typed
  failure (a word such as `no_furnace_fuel`).
- An action is one primitive call, `do <skill>(args)` to run another skill, or
  `done`. A continuous action may end with `until <guard>`.
- Guards use the facts that the core skills use, in the form `name(args)`, with
  `and`, `or`, `not` and the literal `true`.
- A numeric fact is compared with `<`, `<=`, `==`, `!=`, `>=` or `>`, for
  example `count(cobblestone) >= 8`; `has(item)` and `near(block, radius)` are
  already true or false.
- Facts and arguments may use item ids, tags such as `#logs`, parameters as
  `$n`, and integer counts. Prefer a tag over one variant (`#logs`, not
  `oak_log`; `#planks`, not `oak_planks`): a skill that searches for one wood
  type fails wherever that tree does not grow.
- A skill may call the core skills listed below, but not itself. A core
  skill counts as one step; at most two levels of new skills may nest.
- A skill has at most 30 rules.
- The skill must be named exactly as requested.
- Put only the skill text in `skill_source` (no prose, no markdown fences) and
  one short paragraph on how it works in `rationale`.
"""

TASK_HEADER = """\
Write the skill now.
Goal: {goal}
Required skill name: {skill_name}
The skill must leave the player with at least {count} {item}.
"""

REPAIR_HEADER = """\
The previous attempt failed. Repair it.

Previous source:
{previous_source}

Failure kind: {kind}
Trace digest:
{digest}

Write the corrected skill now. Keep the required skill name: {skill_name}.
"""


class SkillDraft(BaseModel):
    skill_source: str
    rationale: str


class FailureContext(BaseModel):
    kind: str
    digest: str
    previous_source: str | None = None


# ---------------------------------------------------------------------- #
def _system_prompt(catalog: SkillCatalog) -> str:
    # Only core skills: practice runs and the checker load the core set plus the
    # candidate, so a call into an active (learned) skill would never resolve.
    catalog = catalog.model_copy(
        update={"skills": [skill for skill in catalog.skills if skill.origin == "core"]}
    )
    primitives = ", ".join(primitive_names(catalog)) or "none"
    facts = ", ".join(fact_names(catalog)) or "none"
    return (
        f"{DSL_RULES}\n"
        "## Primitives used by the core skills\n\n"
        f"{primitives}\n\n"
        "## Facts available (the ones used by the core skills)\n\n"
        f"{facts}\n\n"
        "## The core skills\n\n"
        f"{catalog_text(catalog)}\n"
    )


def _task_text(goal: Goal, failure: FailureContext | None) -> str:
    task = TASK_HEADER.format(
        goal=goal.text,
        skill_name=goal.skill_name,
        count=goal.count,
        item=goal.item,
    )
    if failure is not None:
        task += "\n" + REPAIR_HEADER.format(
            previous_source=failure.previous_source or "(not available)",
            kind=failure.kind,
            digest=failure.digest,
            skill_name=goal.skill_name,
        )
    return task


def _usage_tokens(usage: Any) -> tuple[int, int]:
    return (
        int(getattr(usage, "input_tokens", 0) or 0),
        int(getattr(usage, "output_tokens", 0) or 0),
    )


def write(
    goal: Goal,
    catalog: SkillCatalog,
    failure: FailureContext | None = None,
    model: str = "gpt-6-luna",
    *,
    client: Any | None = None,
    ledger: Ledger | None = None,
    evidence: Evidence | None = None,
    effort: str = "low",
) -> SkillDraft:
    """Ask the writer model for one skill, logging the exchange and its cost."""
    ledger = ledger if ledger is not None else Ledger()
    evidence = evidence if evidence is not None else Evidence(config.local_brain_dir())
    purpose = "repair_skill" if failure is not None else "write_skill"

    system = _system_prompt(catalog)
    task = _task_text(goal, failure)
    input_estimate = max(1, (len(system) + len(task)) // 4)
    ledger.ensure("openai", estimate_cost(model, input_estimate, 2_000))
    evidence.write(
        "writer_request",
        provider="openai",
        model=model,
        purpose=purpose,
        system=system,
        task=task,
    )

    if client is None:
        from openai import OpenAI

        client = OpenAI()
    response = client.responses.parse(
        model=model,
        instructions=system,
        input=[{"role": "user", "content": task}],
        text_format=SkillDraft,
        reasoning={"effort": effort},
    )

    parsed = getattr(response, "output_parsed", None)
    input_tokens, output_tokens = _usage_tokens(getattr(response, "usage", None))
    evidence.write(
        "writer_response",
        provider="openai",
        model=model,
        purpose=purpose,
        parsed=parsed.model_dump() if isinstance(parsed, BaseModel) else parsed,
        input_tokens=input_tokens,
        output_tokens=output_tokens,
        response_id=getattr(response, "id", None),
    )
    ledger.record("openai", model, input_tokens, output_tokens, purpose)

    if parsed is None:
        raise RuntimeError(
            f"the writer returned no parsed SkillDraft (response "
            f"{getattr(response, 'id', 'unknown')})"
        )
    if not isinstance(parsed, SkillDraft):
        parsed = SkillDraft.model_validate(parsed)
    return parsed


__all__ = [
    "DSL_RULES",
    "FailureContext",
    "REPAIR_HEADER",
    "SkillDraft",
    "TASK_HEADER",
    "write",
]

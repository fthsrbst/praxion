"""The persona a run plays: one fixed style per world, chosen by a stable hash.

Decision 2026-11-12 "Demo night" (variety): the planner chooses its own order,
but a model that sees the same briefing every run plays the same run. Each run
therefore draws one persona from this fixed list, seeded by the world id, and
the planner sees it next to the surroundings. ``PRAXION_PERSONA=<id>`` pins one
for a demo or a test.
"""

from __future__ import annotations

import hashlib
import os
from typing import NamedTuple

#: The environment variable that pins a persona by id.
PERSONA_ENV = "PRAXION_PERSONA"


class Persona(NamedTuple):
    id: str
    style: str


PERSONAS: tuple[Persona, ...] = (
    Persona(
        "cautious_builder",
        "A cautious builder who secures a camp (crafting table, furnace, chest, torches) "
        "before going underground.",
    ),
    Persona(
        "bold_miner",
        "A bold miner who goes for stone and ore early and accepts some risk for speed.",
    ),
    Persona(
        "hunter",
        "A hunter who secures food and a sword first, then expands from a full stomach.",
    ),
    Persona(
        "explorer",
        "An explorer who looks around and learns what is near before settling on a plan.",
    ),
    Persona(
        "speedrunner",
        "An efficient speedrunner who takes the shortest path to iron tools and skips detours.",
    ),
    Persona(
        "tidy_crafter",
        "A tidy crafter who over-prepares: a full set of tools and torches before each push.",
    ),
)


def pick(world_id: str) -> Persona:
    """The persona of ``world_id``: ``PRAXION_PERSONA`` when set, else a stable hash.

    The hash is SHA-256, never :func:`hash`, so the same world gets the same
    persona in every process. An unknown id in the environment is an error: a
    typo must not silently fall back to a random style.
    """
    pinned = os.environ.get(PERSONA_ENV, "").strip()
    if pinned:
        for persona in PERSONAS:
            if persona.id == pinned:
                return persona
        known = ", ".join(persona.id for persona in PERSONAS)
        raise ValueError(f"{PERSONA_ENV}={pinned!r} is not a persona id (known: {known})")
    digest = hashlib.sha256(world_id.encode("utf-8")).digest()
    return PERSONAS[int.from_bytes(digest[:8], "big") % len(PERSONAS)]


__all__ = ["PERSONAS", "PERSONA_ENV", "Persona", "pick"]

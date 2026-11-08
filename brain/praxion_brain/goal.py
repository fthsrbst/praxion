"""The goal a skill must reach.

The completion condition is computable outside the body: the referee's
inventory must hold at least ``count`` of ``item``.
"""

from __future__ import annotations

from pydantic import BaseModel, Field


class Goal(BaseModel):
    text: str
    item: str
    count: int = Field(default=1, ge=1)

    @property
    def skill_name(self) -> str:
        """The skill name the writer must use (``furnace`` -> ``get_furnace``)."""
        return f"get_{self.item}"


__all__ = ["Goal"]

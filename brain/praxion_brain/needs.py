"""What a goal needs: recipes and tags from the pinned jar against the inventory.

Run 6 (2026-11-09) ended with 96 cobblestone: no part of the brain knew that a
furnace costs eight, so "enough" did not exist and eighteen stock goals were
proposed and played. This module is that knowledge: pure functions over the
export ``praxion-body export-gamedata`` writes (``local/gamedata/26.3.json``).
The decision is 2026-11-09 "Five next areas"; the design is
``logs/mac1109/design/smarter.out.md`` section 3.A with the changes of its
critique (findings 6 and 7).

Three sources feed a goal item, in this order:

1. ``smelting`` when the item is a smelting output. Items such as the iron ingot
   also have a crafting recipe (nine ingots from a block); the furnace route is
   the one a player takes, so it wins. Fuel is not modelled.
2. ``recipes``: the first recipe the jar lists, as ``craft`` uses it.
3. Neither: the item is gathered or mined (coal, raw iron, cobblestone). Such
   a rung is *inert*: it has no needs, the gate does not fire and the model's
   pick stands. The loop logs that mode once per pick so the inert case is
   visible rather than silent (critique finding 7).

A crafting table is never counted as an inventory need: the body places its own
(run 6 held none at the end). It is reported as a flag only.

An unknown item or tag ends the branch and marks the goal ``unknown``; every
caller then skips the gate, which is today's behaviour (fail safe).
"""

from __future__ import annotations

import hashlib
import json
import tomllib
from dataclasses import dataclass, field
from pathlib import Path
from typing import Iterable, Mapping, Optional

from pydantic import BaseModel, Field, ValidationError

from . import config
from .conditions import (
    _normalized_inventory,
    item_of_condition,
    items_of_condition,
    normalize_item,
)

#: The export's file name under ``local/gamedata/``.
EXPORT_NAME = "26.3.json"
#: How many recipe levels below the goal are expanded: furnace -> cobblestone;
#: wooden_axe -> planks -> logs.
DEFAULT_DEPTH = 2

#: Plan modes. ``recipe``/``smelting`` gate; ``inert``/``unknown`` do not.
MODE_RECIPE = "recipe"
MODE_SMELTING = "smelting"
MODE_INERT = "inert"
MODE_UNKNOWN = "unknown"


class GamedataError(RuntimeError):
    """The export exists but cannot be used."""


class StaleGamedata(GamedataError):
    """The export was made from another jar than the one on this machine."""


class Recipe(BaseModel):
    """One crafting recipe: ``inputs`` (item or ``#tag`` -> count) make ``out``."""

    out: int = 1
    table: bool = False
    inputs: dict[str, int] = Field(default_factory=dict)


class Smelt(BaseModel):
    """The items a furnace turns into one output item (alternatives)."""

    inputs: list[str] = Field(default_factory=list)
    seconds: int = 10


class Gamedata(BaseModel):
    """The export: recipes, smelting and item tags of one pinned jar."""

    version: str
    jar_sha256: str
    recipes: dict[str, Recipe] = Field(default_factory=dict)
    smelting: dict[str, Smelt] = Field(default_factory=dict)
    tags: dict[str, list[str]] = Field(default_factory=dict)


# ---------------------------------------------------------------------- #
# Loading
# ---------------------------------------------------------------------- #
def default_path() -> Path:
    return config.local_dir() / "gamedata" / EXPORT_NAME


def jar_path() -> Path:
    """The server jar: ``server_jar`` of ``local/paths.toml``, else the default."""
    default = config.repo_root() / "third_party" / "minecraft" / "server-26.3.jar"
    paths = config.local_dir() / "paths.toml"
    try:
        value = tomllib.loads(paths.read_text(encoding="utf-8")).get("server_jar")
    except (OSError, tomllib.TOMLDecodeError):
        return default
    if not isinstance(value, str) or not value.strip():
        return default
    path = Path(value)
    return path if path.is_absolute() else config.repo_root() / path


def jar_sha256(jar: Path) -> str:
    digest = hashlib.sha256()
    with jar.open("rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def load(path: Path | None = None, *, jar: Path | None = None) -> Optional[Gamedata]:
    """The export, or ``None`` when the file is absent (every need check is then off).

    An export made from another jar than ``jar`` (default: :func:`jar_path`) is
    a :class:`StaleGamedata` error, not a silent drift: a repinned jar needs a
    regenerated export. When the jar itself is not on this machine the hash
    cannot be checked and the export is used as it is.
    """
    source = Path(path) if path is not None else default_path()
    try:
        text = source.read_text(encoding="utf-8")
    except FileNotFoundError:
        return None
    except OSError as error:
        raise GamedataError(f"{source}: {error}") from error
    try:
        data = Gamedata.model_validate(json.loads(text))
    except (json.JSONDecodeError, ValidationError) as error:
        raise GamedataError(f"{source}: not a gamedata export: {error}") from error
    jar_file = Path(jar) if jar is not None else jar_path()
    if jar_file.is_file():
        actual = jar_sha256(jar_file)
        if actual != data.jar_sha256:
            raise StaleGamedata(
                f"{source} was exported from jar {data.jar_sha256[:12]}, "
                f"but {jar_file} is {actual[:12]}: run `praxion-body export-gamedata`"
            )
    return data


# ---------------------------------------------------------------------- #
# Results
# ---------------------------------------------------------------------- #
class Need(BaseModel):
    """One requirement of a goal."""

    item: str  # concrete item or ``#tag``
    need: int  # total required
    have: int  # held in the inventory (all matching items)
    missing: int  # still short after the inventory was spent on earlier needs
    depth: int  # 0 = a direct input of the goal
    via: list[str] = Field(default_factory=list)  # the chain from the goal

    def render(self) -> str:
        return f"{self.item} {self.need} (have {self.have})"


@dataclass
class Plan:
    """The expansion of one goal against one inventory."""

    goal: str
    count: int
    mode: str
    rows: list[Need] = field(default_factory=list)
    #: Leaf shortfalls: what the inventory cannot cover even after crafting.
    unmet: list[Need] = field(default_factory=list)
    table: bool = False

    @property
    def gating(self) -> bool:
        return self.mode in (MODE_RECIPE, MODE_SMELTING)

    @property
    def covered(self) -> bool:
        return self.gating and not self.unmet


class _Unknown(Exception):
    """An item or tag the export does not define."""


def _members(item: str, data: Gamedata) -> list[str]:
    if not item.startswith("#"):
        return [item]
    members = data.tags.get(item[1:])
    if members is None:
        raise _Unknown(item)
    return members


def _source(item: str, data: Gamedata) -> Optional[tuple[str, object]]:
    """``("smelting", Smelt)`` or ``("recipe", Recipe)`` for a concrete item."""
    if item.startswith("#"):
        return None
    smelt = data.smelting.get(item)
    if smelt is not None:
        # An ore block smelts into its own drop (coal ore -> coal): mining gives
        # that item directly, so an ore is no input. A rung left with no input
        # is mined, hence inert.
        inputs = [name for name in smelt.inputs if not name.endswith("_ore")]
        if inputs:
            return (MODE_SMELTING, Smelt(inputs=inputs, seconds=smelt.seconds))
    if item in data.recipes:
        return (MODE_RECIPE, data.recipes[item])
    return None


def _held(item: str, inventory: Mapping[str, int], data: Gamedata) -> int:
    return sum(inventory.get(member, 0) for member in _members(item, data))


def _take(item: str, amount: int, remaining: dict[str, int], data: Gamedata) -> int:
    """Spend up to ``amount`` of ``item`` from ``remaining``; return what was taken."""
    taken = 0
    # Largest holding first: a tag draws on the stack the inventory has most of.
    for member in sorted(_members(item, data), key=lambda m: (-remaining.get(m, 0), m)):
        if taken >= amount:
            break
        spend = min(remaining.get(member, 0), amount - taken)
        if spend > 0:
            remaining[member] -= spend
            taken += spend
    return taken


def _representative(item: str, remaining: Mapping[str, int], data: Gamedata) -> Optional[str]:
    """The concrete item to craft when a tag is short: the member whose own
    inputs the inventory holds most of, ties by name (so oak logs in the bag
    make oak planks, not birch)."""
    best: Optional[tuple[int, str]] = None
    for member in sorted(_members(item, data)):
        source = _source(member, data)
        if source is None:
            continue
        kind, entry = source
        names = entry.inputs if kind == MODE_SMELTING else list(entry.inputs)  # type: ignore[attr-defined]
        score = sum(_held(name, remaining, data) for name in names)
        if best is None or score > best[0]:
            best = (score, member)
    return best[1] if best is not None else None


def _smelt_input(smelt: Smelt, remaining: Mapping[str, int], data: Gamedata) -> str:
    """The alternative the inventory holds most of, else the first."""
    return max(
        smelt.inputs,
        key=lambda candidate: (_held(candidate, remaining, data), -smelt.inputs.index(candidate)),
    )


def explain(
    goal: str,
    inventory: Mapping[str, int],
    data: Gamedata,
    *,
    count: int = 1,
    depth: int = DEFAULT_DEPTH,
) -> Plan:
    """Expand ``count`` of ``goal`` into inputs, spending the inventory once.

    A multi-output recipe is never over-ordered: ``crafts = ceil(count / out)``
    and each input is needed ``crafts * n`` times, so four sticks cost the same
    two planks as one.
    """
    goal = normalize_item(goal)
    have = _normalized_inventory(inventory)
    plan = Plan(goal=goal, count=max(1, int(count)), mode=MODE_INERT)
    try:
        if _source(goal, data) is None:
            # Validate a tag goal now so an unknown tag reads as unknown.
            _members(goal, data)
            return plan
        remaining = dict(have)
        plan.mode = _source(goal, data)[0]  # type: ignore[index]
        _expand(goal, plan.count, 0, [], remaining, have, data, depth, plan)
    except _Unknown:
        plan.mode = MODE_UNKNOWN
        plan.rows.clear()
        plan.unmet.clear()
    return plan


def _expand(
    item: str,
    wanted: int,
    level: int,
    via: list[str],
    remaining: dict[str, int],
    inventory: Mapping[str, int],
    data: Gamedata,
    depth: int,
    plan: Plan,
) -> None:
    source = _source(item, data)
    if source is None:
        return
    kind, entry = source
    if kind == MODE_SMELTING:
        smelt: Smelt = entry  # type: ignore[assignment]
        if not smelt.inputs:
            return
        inputs = {_smelt_input(smelt, remaining, data): wanted}
    else:
        recipe: Recipe = entry  # type: ignore[assignment]
        if recipe.table:
            plan.table = True
        crafts = -(-wanted // max(1, recipe.out))
        inputs = {name: crafts * amount for name, amount in recipe.inputs.items()}
    chain = [*via, item]
    for name, need in inputs.items():
        taken = _take(name, need, remaining, data)
        missing = need - taken
        row = Need(
            item=name,
            need=need,
            have=_held(name, inventory, data),
            missing=missing,
            depth=level,
            via=chain,
        )
        plan.rows.append(row)
        if missing <= 0:
            continue
        crafted = name if not name.startswith("#") else _representative(name, remaining, data)
        if level < depth and crafted is not None and _source(crafted, data) is not None:
            _expand(crafted, missing, level + 1, chain, remaining, inventory, data, depth, plan)
        else:
            plan.unmet.append(row.model_copy(update={"need": missing, "missing": missing}))


def missing_for(
    goal: str,
    inventory: Mapping[str, int],
    data: Gamedata,
    *,
    count: int = 1,
    depth: int = DEFAULT_DEPTH,
) -> list[Need]:
    """What the inventory cannot cover for ``count`` of ``goal`` (empty when covered)."""
    return explain(goal, inventory, data, count=count, depth=depth).unmet


def covered(
    goal: str,
    inventory: Mapping[str, int],
    data: Gamedata,
    *,
    count: int = 1,
    depth: int = DEFAULT_DEPTH,
) -> bool:
    """Whether the inventory already holds everything ``goal`` needs.

    False for an inert or unknown goal: nothing can be covered that has no
    recipe, so callers that gate on this must also check :attr:`Plan.gating`.
    """
    return explain(goal, inventory, data, count=count, depth=depth).covered


# ---------------------------------------------------------------------- #
# Serving a rung: the names a goal may gather for it
# ---------------------------------------------------------------------- #
def inputs_of(
    goal: str, data: Gamedata, *, depth: int = DEFAULT_DEPTH
) -> tuple[set[str], set[str]]:
    """``(direct, closure)``: the inputs of ``goal``, and those of its inputs.

    Inventory-blind: it answers "could this item serve the goal", not "is it
    missing". Tags stay as ``#tag`` names; :func:`matches` compares them.
    """
    direct: set[str] = set()
    closure: set[str] = set()

    def walk(item: str, level: int) -> None:
        source = _source(item, data)
        if source is None:
            return
        kind, entry = source
        names = (
            list(entry.inputs)  # type: ignore[attr-defined]
            if kind == MODE_SMELTING
            else list(entry.inputs.keys())  # type: ignore[attr-defined]
        )
        for name in names:
            closure.add(name)
            if level == 0:
                direct.add(name)
            if level < depth:
                members = [name] if not name.startswith("#") else data.tags.get(name[1:], [])
                for member in members[:1] if name.startswith("#") else members:
                    walk(member, level + 1)

    walk(normalize_item(goal), 0)
    return direct, closure


def matches(item: str, names: Iterable[str], data: Gamedata) -> bool:
    """Whether ``item`` is one of ``names`` or belongs to a tag among them (or the reverse)."""
    item = normalize_item(item)
    for name in names:
        if item == name:
            return True
        if name.startswith("#") and item in data.tags.get(name[1:], ()):
            return True
        if item.startswith("#") and name in data.tags.get(item[1:], ()):
            return True
    return False


@dataclass(frozen=True)
class RungNeed:
    """The next ladder rung's needs against the current inventory."""

    rung: str
    goal: str
    count: int
    plan: Plan
    direct: frozenset[str]
    closure: frozenset[str]

    @property
    def mode(self) -> str:
        return self.plan.mode

    @property
    def gating(self) -> bool:
        return self.plan.gating

    @property
    def covered(self) -> bool:
        return self.plan.covered


def rung_need(
    rung_id: str,
    rung_condition: str,
    inventory: Mapping[str, int],
    data: Gamedata,
) -> Optional[RungNeed]:
    """The needs of a ladder rung, from its own condition (``has(furnace)``).

    ``None`` when the condition has no single item (a compound rung): there is
    nothing to expand.
    """
    goal = item_of_condition(rung_condition)
    if goal is None:
        return None
    from .conditions import count_of_condition

    count = count_of_condition(rung_condition)
    plan = explain(goal, inventory, data, count=count)
    direct, closure = inputs_of(goal, data)
    return RungNeed(
        rung=rung_id,
        goal=goal,
        count=count,
        plan=plan,
        direct=frozenset(direct),
        closure=frozenset(closure),
    )


def _concrete(
    item: str, inventory: Mapping[str, int], data: Gamedata
) -> Optional[str]:
    """The concrete item a tagged need should be gathered as, or ``None``.

    The briefing must name an item the brain can evaluate:
    ``#stone_tool_materials`` is outside the condition evaluator's tag table, so
    a goal spelled with it cannot be judged and the writer is left to guess. The
    member the inventory already holds most of continues the material the run is
    using (cobblestone in run 6, not blackstone); with none held every member
    would be a guess, so the tag stays as it is.
    """
    if not item.startswith("#"):
        return item
    members = _members(item, data)
    ranked = sorted(
        members, key=lambda member: (-_held(member, inventory, data), member)
    )
    if ranked and _held(ranked[0], inventory, data) > 0:
        return ranked[0]
    return None


def _shortfall(row: Need, inventory: Mapping[str, int], data: Gamedata) -> str:
    """One unmet need for the briefing, with a tag resolved to a member."""
    concrete = _concrete(row.item, inventory, data)
    if concrete is not None and concrete != row.item:
        return f"{concrete} (or {row.item}) {row.missing} more"
    return f"{row.item} {row.missing} more"


def need_line(
    need: Optional[RungNeed], inventory: Mapping[str, int], data: Gamedata
) -> str:
    """The one briefing line for the next rung (``## Needs``)."""
    if need is None:
        return "next rung: no single item; no need check"
    have = _normalized_inventory(inventory)
    held = sum(
        amount for key, amount in have.items() if key == need.goal
    ) if not need.goal.startswith("#") else 0
    head = f"{need.rung}: {min(held, need.count)} of {need.count}"
    plan = need.plan
    if plan.mode == MODE_INERT:
        return f"{head}; gathered, not crafted; no needs to check"
    if plan.mode == MODE_UNKNOWN:
        return f"{head}; recipe unknown; no needs to check"
    parts = [row.render() for row in plan.rows if row.depth == 0]
    text = f"{head}; needs {', '.join(parts) if parts else 'nothing'}"
    if plan.table:
        text += " at a crafting table"
    if plan.covered:
        return f"{text} -> covered"
    short = ", ".join(_shortfall(row, inventory, data) for row in plan.unmet)
    return f"{text} -> missing {short}"


# ---------------------------------------------------------------------- #
# The gate
# ---------------------------------------------------------------------- #
@dataclass(frozen=True)
class Gate:
    """Everything check 6, check 7 and the drift brake need for one pick."""

    data: Gamedata
    need: Optional[RungNeed]
    #: Items of the last two proposals, newest first.
    recent_items: tuple[str, ...] = ()
    #: The drift brake is on: only goals serving the next rung pass.
    brake: bool = False
    #: The next rung is covered and its skill is being written, so its goal
    #: cannot be played yet: only a later rung's own goal is worth proposing.
    waiting: bool = False
    #: The items of the unmet rungs after the next one (``coal``, ``raw_iron``...).
    later_items: frozenset[str] = frozenset()


@dataclass(frozen=True)
class Rejection:
    reason: str  # "not_needed" | "repeat_item"
    detail: str


def _check_compound(gate: Gate, need: RungNeed, items: list[str]) -> Optional[Rejection]:
    """The waiting rule, the brake and check 6 for a compound condition.

    A compound names no single item, so check 7 ("the same item again") has no
    subject and stays for the simple shapes. The strict modes cannot let such a
    goal through, though: run 6's stock goals only had to gain one ``and`` to
    slip past every rule. A compound passes while any of its parts serves the
    rung the way the matching simple rule asks for.
    """
    if not items:
        return None
    serves_goal = any(item == need.goal for item in items)
    if gate.waiting and not serves_goal and not any(
        item in gate.later_items for item in items
    ):
        return Rejection(
            "not_needed",
            f"{need.goal} is covered and waiting for its skill; "
            f"none of {', '.join(items)} serves a later rung",
        )
    if gate.brake and not serves_goal and not any(
        matches(item, need.direct, gate.data) for item in items
    ):
        return Rejection(
            "not_needed",
            f"drift brake: none of {', '.join(items)} serves {need.goal}",
        )
    # Check 6: a compound that only gathers inputs the inventory already covers.
    if need.covered and not serves_goal and all(
        matches(item, need.closure, gate.data) for item in items
    ):
        return Rejection(
            "not_needed",
            f"every item of {', '.join(items)} is an input of {need.goal} "
            "and the inventory already covers it",
        )
    return None


def check(gate: Gate, condition: str) -> Optional[Rejection]:
    """Checks 6 and 7 and the brake for one proposed condition.

    Only a simple ``has(X)``/``count(X) >= n`` shape names one item; a compound
    is judged part by part by :func:`_check_compound`. An inert or unknown next
    rung switches the whole gate off (critique finding 7): nothing is known about
    what serves it, so the model's pick stands. That includes the brake's
    "serve the next rung" half: during a mined rung (coal, raw iron) the brake
    is off and drift is not braked, which is accepted because a mined rung has
    no inputs to steer towards (review finding 9).
    """
    need = gate.need
    if need is None or not need.gating:
        return None
    try:
        item = item_of_condition(condition)
        items = items_of_condition(condition) if item is None else []
    except Exception:  # noqa: BLE001 - validate() already parsed it
        return None
    if item is None:
        return _check_compound(gate, need, items)
    serves = item == need.goal or matches(item, need.direct, gate.data)

    # Check 6: gathering an input the next rung already holds enough of.
    if need.covered and matches(item, need.closure, gate.data):
        return Rejection(
            "not_needed",
            f"{item} is an input of {need.goal} and the inventory already covers it",
        )
    # Waiting for the skill of a covered rung (design A6 step 3, generalised by
    # the slice-1 brief: "zero stock proposals"): gathering anything that is no
    # later rung's own goal is stock, however it is spelled. The model may
    # still propose a later rung (coal, iron) and the body plays it.
    if gate.waiting and item != need.goal and item not in gate.later_items:
        return Rejection(
            "not_needed",
            f"{need.goal} is covered and waiting for its skill; "
            f"{item} serves no later rung",
        )
    # The drift brake: five flat goals, so only the next rung's own item or a
    # direct input of its recipe may be proposed.
    if gate.brake and not serves:
        return Rejection(
            "not_needed",
            f"drift brake: only {need.goal} or its direct inputs may be proposed",
        )
    # Check 7 with the usefulness exemption (critique finding 6): the same item
    # again is only refused while the next rung is covered; once a craft has
    # used the stock up, gathering it again is legitimate.
    if need.covered and any(item == recent for recent in gate.recent_items):
        return Rejection(
            "repeat_item",
            f"{item} was one of the last two goals and {need.goal} is covered",
        )
    return None


__all__ = [
    "DEFAULT_DEPTH",
    "EXPORT_NAME",
    "Gamedata",
    "GamedataError",
    "Gate",
    "MODE_INERT",
    "MODE_RECIPE",
    "MODE_SMELTING",
    "MODE_UNKNOWN",
    "Need",
    "Plan",
    "Recipe",
    "Rejection",
    "RungNeed",
    "Smelt",
    "StaleGamedata",
    "check",
    "covered",
    "default_path",
    "explain",
    "inputs_of",
    "jar_path",
    "jar_sha256",
    "load",
    "matches",
    "missing_for",
    "need_line",
    "rung_need",
]

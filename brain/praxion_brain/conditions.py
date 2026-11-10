"""Guard conditions over inventory facts, evaluated in Python.

A condition is guard text in the rule-list syntax (proposal section 4.2). The
body will own the real evaluator later; for now this module understands the
inventory subset the curriculum needs:

* ``has(item)``
* ``count(item) <op> n`` with ``< <= == != >= >``
* ``and``, ``not``, ``true`` (``false`` is accepted as its obvious sibling)
* parentheses for grouping

The tokenizer mirrors the supported part of ``body/crates/rules/src/lexer.rs``:
ASCII identifiers ``[A-Za-z_][A-Za-z0-9_]*``, adjacent ``namespace:path``
names, ``#tag`` names, negative integer literals, ``$parameter`` tokens and
``#`` comments that run to the end of the line. A ``#`` followed by a letter or
``_`` is a tag, exactly as in the body; any other ``#`` starts a comment.

Item names and inventory keys may carry a ``minecraft:`` namespace; both sides
are normalized to the bare id by :func:`normalize_item`, mirroring
``normalize_id`` in ``body/crates/rules/src/lib.rs`` and
``gamedata::normalize``. Tags use the same rule, so ``#minecraft:logs`` is
``#logs``.

Evaluation is three-valued: ``True``, ``False`` or ``None``. ``None`` means the
condition is not verifiable here (an unsupported fact such as ``near``, an
unknown tag, a bare identifier, a ``$parameter``, or a string argument). A
*malformed* condition raises :class:`ConditionError` instead, so a bad proposal
is rejected rather than silently treated as unknown.

Survival facts (decision 2026-11-09 "Five next areas", survival slice 3): the
zero-argument calls ``count_food()``, ``health()``, ``food()`` (ints) and
``is_night()`` (bool) are decidable when :func:`evaluate` is given the snapshot
``facts`` next to the inventory, so a food or night goal is *verifiable* and
its settlement is ``done`` or ``failed``, not ``unverified``. They mirror
``body/crates/facts/src/lib.rs``; without ``facts`` they stay unknown.

Tags: ``#logs`` counts every inventory key ending in ``_log`` and ``#planks``
every key ending in ``_planks``; these two come from ``body/skills``. Any other
tag is unverifiable. This small table must be joined to the gamedata tag tables
when the body's ``check-guard`` step lands.

Spec: ``docs/proposals/2026-11-08-demo2-loop.md`` sections 4.1, 4.2 and 5.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any, Mapping, Optional, Union

#: Comparison operators this evaluator understands.
COMPARISONS = frozenset({"<", "<=", "==", "!=", ">=", ">"})
#: Tag -> inventory key suffix. Only tags used by ``body/skills`` are listed.
#: The body's ``gamedata`` tag tables are the eventual source (see module doc).
TAGS: dict[str, str] = {"#logs": "_log", "#planks": "_planks"}

#: The survival facts a condition may call without arguments, and their type.
FACT_CALLS: dict[str, str] = {
    "count_food": "int",
    "health": "int",
    "food": "int",
    "is_night": "bool",
}

#: Items that count as food for ``count_food()``: the vanilla edible items a
#: first-night body can hold, raw or cooked. The body counts every stack with
#: the `Food` component (``praxion_facts::count_food``); this list is its brain
#: mirror and may lag it for rare items (unknown items count as zero, which
#: only ever asks for food a little early).
FOOD_ITEMS = frozenset(
    {
        "apple", "baked_potato", "beef", "beetroot", "bread", "carrot", "chicken",
        "cod", "cooked_beef", "cooked_chicken", "cooked_cod", "cooked_mutton",
        "cooked_porkchop", "cooked_rabbit", "cooked_salmon", "cookie",
        "dried_kelp", "golden_apple", "golden_carrot", "melon_slice", "mutton",
        "porkchop", "potato", "pumpkin_pie", "rabbit", "salmon", "sweet_berries",
        "glow_berries", "mushroom_stew", "rabbit_stew", "beetroot_soup",
        "tropical_fish", "pufferfish", "rotten_flesh", "spider_eye",
        "poisonous_potato", "chorus_fruit", "enchanted_golden_apple",
    }
)

#: The only namespace the body's ``normalize_id`` strips.
_MINECRAFT = "minecraft:"
_KEYWORDS = frozenset({"and", "or", "not", "true", "false"})
_TWO_CHAR_OPS = ("!=", "<=", "==", ">=")
_ONE_CHAR_OPS = "<>"
_ESCAPES = {'"': '"', "\\": "\\", "n": "\n", "t": "\t", "r": "\r"}


#: Names that are facts or categories, never items: inside ``has()``/``count()``
#: they would count an item that does not exist (always 0).
NOT_ITEMS = {
    "food": "count_food() for the food held, or food() for the hunger bar",
    "health": "health()",
}


class ConditionError(ValueError):
    """The guard text is not well formed."""


# --------------------------------------------------------------------------- #
# Names
# --------------------------------------------------------------------------- #
def normalize_item(name: str) -> str:
    """Strip an optional ``minecraft:`` namespace from an item or tag name.

    ``minecraft:oak_log`` becomes ``oak_log`` and ``#minecraft:logs`` becomes
    ``#logs``. This is the one helper both sides of a lookup use: inventory
    keys and the item names inside a condition.
    """
    prefix = "#" if name.startswith("#") else ""
    bare = name[1:] if prefix else name
    if bare.startswith(_MINECRAFT):
        bare = bare[len(_MINECRAFT) :]
    return prefix + bare


def _is_tag(name: str) -> bool:
    return name.startswith("#")


def _is_identifier(name: str) -> bool:
    if not name or not _is_ident_start(name[0]):
        return False
    return all(_is_ident_continue(char) for char in name[1:])


def _is_ident_start(char: str) -> bool:
    return char.isascii() and (char.isalpha() or char == "_")


def _is_ident_continue(char: str) -> bool:
    return char.isascii() and (char.isalnum() or char == "_")


def _is_digit(char: str) -> bool:
    return char.isascii() and char.isdigit()


def _read_ident(text: str, index: int) -> tuple[str, int]:
    start = index
    while index < len(text) and _is_ident_continue(text[index]):
        index += 1
    return text[start:index], index


def _merge_namespace(text: str, index: int, name: str) -> tuple[str, int]:
    """Merge an adjacent ``namespace:path`` pair, as the body's ``parse_name``."""
    if (
        index < len(text)
        and text[index] == ":"
        and index + 1 < len(text)
        and _is_ident_start(text[index + 1])
    ):
        path, end = _read_ident(text, index + 1)
        return f"{name}:{path}", end
    return name, index


# --------------------------------------------------------------------------- #
# Tokens
# --------------------------------------------------------------------------- #
@dataclass(frozen=True)
class _Token:
    kind: str
    value: str
    position: int

    def describe(self) -> str:
        if self.kind == "end":
            return "end of condition"
        return f"{self.value!r} at position {self.position}"


def _read_string(text: str, start: int) -> tuple[str, int]:
    """Read a double-quoted string with the body lexer's escapes."""
    value: list[str] = []
    index = start + 1
    while index < len(text):
        char = text[index]
        if char == "\n":
            raise ConditionError(f"unterminated string at position {start}")
        if char == '"':
            return "".join(value), index + 1
        if char != "\\":
            value.append(char)
            index += 1
            continue
        escape_at = index
        index += 1
        if index >= len(text):
            raise ConditionError(f"unterminated string at position {start}")
        escape = text[index]
        if escape in _ESCAPES:
            value.append(_ESCAPES[escape])
            index += 1
            continue
        if escape == "u":
            index += 1
            if index >= len(text) or text[index] != "{":
                raise ConditionError(
                    f"expected '{{' after '\\u' at position {escape_at}"
                )
            close = text.find("}", index + 1)
            if close == -1:
                raise ConditionError(
                    f"unterminated '\\u{{...}}' escape at position {escape_at}"
                )
            try:
                value.append(chr(int(text[index + 1 : close], 16)))
            except ValueError as error:
                raise ConditionError(
                    f"invalid '\\u{{...}}' escape at position {escape_at}"
                ) from error
            index = close + 1
            continue
        raise ConditionError(f"unknown escape '\\{escape}' at position {escape_at}")
    raise ConditionError(f"unterminated string at position {start}")


def _tokenize(text: str) -> list[_Token]:
    tokens: list[_Token] = []
    index = 0
    length = len(text)
    while index < length:
        char = text[index]
        if char in " \t\r\n":
            index += 1
            continue
        if char == "#":
            start = index
            index += 1
            if index < length and _is_ident_start(text[index]):
                name, index = _read_ident(text, index)
                name, index = _merge_namespace(text, index, name)
                tokens.append(_Token("tag", normalize_item("#" + name), start))
            else:
                # Not `#name`: a comment that runs to the end of the line.
                while index < length and text[index] != "\n":
                    index += 1
            continue
        if char == '"':
            start = index
            value, index = _read_string(text, index)
            tokens.append(_Token("str", value, start))
            continue
        if char == "$":
            start = index
            index += 1
            if index >= length or not _is_ident_start(text[index]):
                raise ConditionError(
                    f"expected a parameter name after '$' at position {start}"
                )
            name, index = _read_ident(text, index)
            tokens.append(_Token("param", name, start))
            continue
        pair = text[index : index + 2]
        if pair in _TWO_CHAR_OPS:
            tokens.append(_Token("op", pair, index))
            index += 2
            continue
        if char in _ONE_CHAR_OPS:
            tokens.append(_Token("op", char, index))
            index += 1
            continue
        if char == "-":
            if index + 1 < length and _is_digit(text[index + 1]):
                start = index
                index += 1
                while index < length and _is_digit(text[index]):
                    index += 1
                tokens.append(_Token("num", text[start:index], start))
                continue
            raise ConditionError(
                f"unexpected '-' at position {index}; "
                "only negative integers are allowed"
            )
        if _is_digit(char):
            start = index
            while index < length and _is_digit(text[index]):
                index += 1
            tokens.append(_Token("num", text[start:index], start))
            continue
        if char == "(":
            tokens.append(_Token("lp", char, index))
            index += 1
            continue
        if char == ")":
            tokens.append(_Token("rp", char, index))
            index += 1
            continue
        if char == ",":
            tokens.append(_Token("comma", char, index))
            index += 1
            continue
        if _is_ident_start(char):
            start = index
            name, index = _read_ident(text, index)
            name, index = _merge_namespace(text, index, name)
            tokens.append(_Token("id", normalize_item(name), start))
            continue
        raise ConditionError(f"unexpected character {char!r} at position {index}")
    tokens.append(_Token("end", "", length))
    return tokens


# --------------------------------------------------------------------------- #
# Syntax tree
# --------------------------------------------------------------------------- #
@dataclass(frozen=True)
class Literal:
    value: bool


@dataclass(frozen=True)
class Name:
    name: str


@dataclass(frozen=True)
class Number:
    value: int


@dataclass(frozen=True)
class String:
    value: str


@dataclass(frozen=True)
class Param:
    """``$name``: a skill parameter, which this module cannot decide."""

    name: str


@dataclass(frozen=True)
class Call:
    name: str
    args: tuple["Node", ...]


@dataclass(frozen=True)
class Compare:
    left: "Node"
    operator: str
    right: "Node"


@dataclass(frozen=True)
class Not:
    child: "Node"


@dataclass(frozen=True)
class And:
    left: "Node"
    right: "Node"


@dataclass(frozen=True)
class Or:
    """Parsed so ``a or b`` is *unverifiable*, not a syntax error."""

    left: "Node"
    right: "Node"


Node = Union[
    Literal, Name, Number, String, Param, Call, Compare, Not, And, Or
]


class _Parser:
    def __init__(self, tokens: list[_Token]) -> None:
        self._tokens = tokens
        self._index = 0

    def parse(self) -> Node:
        if self._peek().kind == "end":
            raise ConditionError("empty condition")
        node = self._expression()
        token = self._peek()
        if token.kind != "end":
            raise ConditionError(f"unexpected {token.describe()}")
        return node

    def _peek(self) -> _Token:
        return self._tokens[self._index]

    def _next(self) -> _Token:
        token = self._tokens[self._index]
        self._index += 1
        return token

    def _is_keyword(self, word: str) -> bool:
        token = self._peek()
        return token.kind == "id" and token.value == word

    def _expect(self, kind: str, what: str) -> _Token:
        token = self._peek()
        if token.kind != kind:
            raise ConditionError(f"expected {what}, found {token.describe()}")
        return self._next()

    def _expression(self) -> Node:
        node = self._and()
        while self._is_keyword("or"):
            self._next()
            node = Or(node, self._and())
        return node

    def _and(self) -> Node:
        node = self._not()
        while self._is_keyword("and"):
            self._next()
            node = And(node, self._not())
        return node

    def _not(self) -> Node:
        if self._is_keyword("not"):
            self._next()
            return Not(self._not())
        return self._comparison()

    def _comparison(self) -> Node:
        left = self._term()
        token = self._peek()
        if token.kind == "op":
            if token.value not in COMPARISONS:
                raise ConditionError(f"unknown comparison {token.describe()}")
            self._next()
            return Compare(left, token.value, self._term())
        return left

    def _term(self) -> Node:
        token = self._peek()
        if token.kind == "num":
            self._next()
            return Number(int(token.value))
        if token.kind == "str":
            self._next()
            return String(token.value)
        if token.kind == "param":
            self._next()
            return Param(token.value)
        if token.kind == "tag":
            self._next()
            return Name(token.value)
        if token.kind == "lp":
            self._next()
            node = self._expression()
            self._expect("rp", "')'")
            return node
        if token.kind == "id":
            self._next()
            if self._peek().kind == "lp":
                args = self._arguments()
                if token.value in {"has", "count"} and len(args) != 1:
                    raise ConditionError(
                        f"{token.value}() takes exactly one item name, "
                        f"found {len(args)} argument(s)"
                    )
                if (
                    token.value in {"has", "count"}
                    and isinstance(args[0], Name)
                    and args[0].name in NOT_ITEMS
                ):
                    # Run 9: the model wrote `count(food) >= 2`, which counts an
                    # item called "food" (always 0) while `get_food` stops on
                    # `count_food()`, so a goal the skill finished settled false.
                    raise ConditionError(
                        f"{args[0].name!r} is not an item: use "
                        f"{NOT_ITEMS[args[0].name]}"
                    )
                return Call(token.value, args)
            if token.value == "true":
                return Literal(True)
            if token.value == "false":
                return Literal(False)
            return Name(token.value)
        raise ConditionError(f"expected a value, found {token.describe()}")

    def _arguments(self) -> tuple[Node, ...]:
        self._expect("lp", "'('")
        args: list[Node] = []
        if self._peek().kind != "rp":
            while True:
                args.append(self._term())
                if self._peek().kind != "comma":
                    break
                self._next()
        self._expect("rp", "')'")
        return tuple(args)


def parse(text: str) -> Node:
    """Parse guard text; raises :class:`ConditionError` when malformed."""
    if not isinstance(text, str):
        raise ConditionError(f"condition must be text, got {type(text).__name__}")
    return _Parser(_tokenize(text)).parse()


# --------------------------------------------------------------------------- #
# Canonical text (used for failure-memory keys)
# --------------------------------------------------------------------------- #
def _render(node: Node) -> str:
    if isinstance(node, Literal):
        return "true" if node.value else "false"
    if isinstance(node, Name):
        return normalize_item(node.name)
    if isinstance(node, Number):
        return str(node.value)
    if isinstance(node, String):
        escaped = (
            node.value.replace("\\", "\\\\")
            .replace('"', '\\"')
            .replace("\n", "\\n")
            .replace("\t", "\\t")
            .replace("\r", "\\r")
        )
        return f'"{escaped}"'
    if isinstance(node, Param):
        return f"${node.name}"
    if isinstance(node, Call):
        args = ", ".join(_render(argument) for argument in node.args)
        return f"{node.name}({args})"
    if isinstance(node, Compare):
        return f"{_render(node.left)} {node.operator} {_render(node.right)}"
    if isinstance(node, Not):
        child = _render(node.child)
        if isinstance(node.child, (And, Or)):
            return f"not ({child})"
        return f"not {child}"
    if isinstance(node, And):
        left = _render(node.left)
        right = _render(node.right)
        if isinstance(node.left, Or):
            left = f"({left})"
        if isinstance(node.right, Or):
            right = f"({right})"
        return f"{left} and {right}"
    # Or
    return f"{_render(node.left)} or {_render(node.right)}"


def canonical_text(text: str) -> str:
    """The condition in one canonical spelling.

    Parsed text loses its whitespace, spelling and ``minecraft:`` namespaces,
    so ``count(cobblestone)>=8`` and ``count(minecraft:cobblestone) >= 8``
    share one key. Malformed text raises :class:`ConditionError`; callers that
    need a key for legacy records should fall back to whitespace collapse.
    """
    return _render(parse(text))


# --------------------------------------------------------------------------- #
# Inventory lookup
# --------------------------------------------------------------------------- #
def _item_known(item: str) -> bool:
    item = normalize_item(item)
    if _is_tag(item):
        return item in TAGS
    return _is_identifier(item) and item not in _KEYWORDS


def _normalized_inventory(inventory: Mapping[str, Any]) -> dict[str, int]:
    """Inventory totals with every key normalized to its bare item id."""
    totals: dict[str, int] = {}
    for key, value in inventory.items():
        name = normalize_item(str(key))
        totals[name] = totals.get(name, 0) + _amount(value)
    return totals


def _count(normalized: Mapping[str, int], item: str) -> Optional[int]:
    item = normalize_item(item)
    if item.startswith("#"):
        suffix = TAGS.get(item)
        if suffix is None:
            return None
        return sum(
            amount for key, amount in normalized.items() if key.endswith(suffix)
        )
    return normalized.get(item, 0)


def _amount(value: Any) -> int:
    try:
        return int(value)
    except (TypeError, ValueError):
        return 0


def _fact_call(node: Node) -> Optional[str]:
    """The name of a zero-argument survival fact call, else ``None``."""
    if isinstance(node, Call) and not node.args and node.name in FACT_CALLS:
        return node.name
    return None


def _fact_value(
    name: str, inventory: Mapping[str, int], facts: Optional[Mapping[str, Any]]
) -> Optional[Union[int, bool]]:
    """One survival fact from the snapshot ``facts``; ``None`` when unknown."""
    if name == "count_food":
        if facts is None:
            return None
        return sum(amount for key, amount in inventory.items() if key in FOOD_ITEMS)
    if facts is None:
        return None
    if name == "is_night":
        value = facts.get("is_night")
        return value if isinstance(value, bool) else None
    value = facts.get("health" if name == "health" else "food")
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    return int(value // 1)


def _call_item(node: Node, name: str) -> Optional[str]:
    if not isinstance(node, Call) or node.name != name or len(node.args) != 1:
        return None
    argument = node.args[0]
    if not isinstance(argument, Name):
        return None
    return argument.name


def _compare(operator: str, left: int, right: int) -> bool:
    if operator == "<":
        return left < right
    if operator == "<=":
        return left <= right
    if operator == "==":
        return left == right
    if operator == "!=":
        return left != right
    if operator == ">=":
        return left >= right
    return left > right


# --------------------------------------------------------------------------- #
# Verifiability (the subset this module can decide)
# --------------------------------------------------------------------------- #
def _verifiable(node: Node) -> bool:
    if isinstance(node, Literal):
        return True
    if isinstance(node, Not):
        return _verifiable(node.child)
    if isinstance(node, And):
        return _verifiable(node.left) and _verifiable(node.right)
    if isinstance(node, Call):
        if _fact_call(node) is not None:
            return FACT_CALLS[node.name] == "bool"
        item = _call_item(node, "has")
        return item is not None and _item_known(item)
    if isinstance(node, Compare):
        if node.operator not in COMPARISONS or not isinstance(node.right, Number):
            return False
        fact = _fact_call(node.left)
        if fact is not None:
            return FACT_CALLS[fact] == "int"
        item = _call_item(node.left, "count")
        return item is not None and _item_known(item)
    # Name, Number, String, Param, Or and unknown calls are outside the subset.
    return False


def is_verifiable(text: str) -> bool:
    """Whether this module can decide the condition from inventory alone.

    Raises :class:`ConditionError` for malformed text; a well-formed condition
    outside the subset (``near``, ``or``, ``$param``, a string argument, ...)
    returns ``False``.
    """
    return _verifiable(parse(text))


# --------------------------------------------------------------------------- #
# Three-valued evaluation
# --------------------------------------------------------------------------- #
def _evaluate(
    node: Node,
    inventory: Mapping[str, int],
    facts: Optional[Mapping[str, Any]] = None,
) -> Optional[bool]:
    if isinstance(node, Literal):
        return node.value
    if isinstance(node, Not):
        value = _evaluate(node.child, inventory, facts)
        return None if value is None else not value
    if isinstance(node, And):
        left = _evaluate(node.left, inventory, facts)
        if left is False:
            return False
        right = _evaluate(node.right, inventory, facts)
        if right is False:
            return False
        if left is True and right is True:
            return True
        return None
    if isinstance(node, Call):
        fact = _fact_call(node)
        if fact is not None:
            value = _fact_value(fact, inventory, facts)
            return value if FACT_CALLS[fact] == "bool" and isinstance(value, bool) else None
        item = _call_item(node, "has")
        if item is None or not _item_known(item):
            return None
        count = _count(inventory, item)
        return None if count is None else count > 0
    if isinstance(node, Compare):
        fact = _fact_call(node.left)
        if fact is not None:
            if FACT_CALLS[fact] != "int" or node.operator not in COMPARISONS or not isinstance(node.right, Number):
                return None
            value = _fact_value(fact, inventory, facts)
            if value is None or isinstance(value, bool):
                return None
            return _compare(node.operator, value, node.right.value)
        item = _call_item(node.left, "count")
        if (
            item is None
            or not _item_known(item)
            or node.operator not in COMPARISONS
            or not isinstance(node.right, Number)
        ):
            return None
        count = _count(inventory, item)
        return None if count is None else _compare(node.operator, count, node.right.value)
    # Name, Number, String, Param and Or are not decisive here.
    return None


def evaluate(
    text: str,
    inventory: Mapping[str, Any],
    facts: Optional[Mapping[str, Any]] = None,
) -> Optional[bool]:
    """Evaluate guard text against an inventory; ``None`` means unknown.

    ``facts`` is the snapshot's body facts (``health``, ``food``, ``is_night``);
    without it a survival fact call such as ``is_night()`` is unknown.
    """
    return _evaluate(parse(text), _normalized_inventory(inventory), facts)


def item_of_condition(text: str) -> Optional[str]:
    """The item named by a simple ``has(X)`` / ``count(X) OP n`` shape.

    The name is normalized (``minecraft:furnace`` becomes ``furnace``). A tag
    comes back as the tag (``#logs``): the shape is simple, so the need gate and
    the drift brake judge it like any other item, but it yields no skill name
    (``get_#logs`` is no name). Returns ``None`` for any other shape (compound
    conditions, bare names, unsupported arguments), which tells
    :mod:`praxion_brain.curriculum` to let the writer pick a skill name.
    """
    node = parse(text)
    item = _call_item(node, "has")
    if item is not None:
        return normalize_item(item)
    if isinstance(node, Compare) and isinstance(node.right, Number):
        item = _call_item(node.left, "count")
        return normalize_item(item) if item is not None else None
    return None


def _collect_items(node: Node, found: list[str]) -> None:
    if isinstance(node, Call):
        if node.name in {"has", "count"}:
            item = _call_item(node, node.name)
            if item is not None and normalize_item(item) not in found:
                found.append(normalize_item(item))
        for argument in node.args:
            _collect_items(argument, found)
    elif isinstance(node, Compare):
        _collect_items(node.left, found)
        _collect_items(node.right, found)
    elif isinstance(node, (And, Or)):
        _collect_items(node.left, found)
        _collect_items(node.right, found)
    elif isinstance(node, Not):
        _collect_items(node.child, found)


def items_of_condition(text: str) -> list[str]:
    """Every item a ``has(X)`` / ``count(X) OP n`` names in the condition.

    A compound condition names no single item, but its parts do; the need gate
    judges those parts so a stock goal cannot slip past the waiting rule or the
    drift brake by gaining one ``and``. Order is textual, names are normalized
    as in :func:`item_of_condition`, and tags stay tags (``#logs``). Malformed
    text raises :class:`ConditionError` like :func:`parse`.
    """
    found: list[str] = []
    _collect_items(parse(text), found)
    return found


def count_of_condition(text: str) -> int:
    """The ``n`` of a ``count(X) >= n`` shape, else 1; never below 1.

    This is the order of magnitude a goal asks for, which the slice's
    :class:`~praxion_brain.goal.Goal` carries to the referee and the writer.
    """
    node = parse(text)
    if isinstance(node, Compare) and isinstance(node.right, Number):
        return max(1, node.right.value)
    return 1


__all__ = [
    "And",
    "Call",
    "Compare",
    "COMPARISONS",
    "FACT_CALLS",
    "FOOD_ITEMS",
    "ConditionError",
    "Literal",
    "Name",
    "Node",
    "Not",
    "Number",
    "Or",
    "Param",
    "String",
    "TAGS",
    "canonical_text",
    "count_of_condition",
    "evaluate",
    "is_verifiable",
    "item_of_condition",
    "items_of_condition",
    "normalize_item",
    "parse",
]

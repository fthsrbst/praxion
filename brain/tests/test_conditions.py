from __future__ import annotations

import pytest

from praxion_brain.conditions import (
    ConditionError,
    canonical_text,
    count_of_condition,
    evaluate,
    is_verifiable,
    item_of_condition,
    items_of_condition,
    parse,
)

INVENTORY = {"cobblestone": 8, "oak_log": 2, "birch_log": 1}


@pytest.mark.parametrize(
    ("text", "expected"),
    [
        ("count(cobblestone) < 9", True),
        ("count(cobblestone) < 8", False),
        ("count(cobblestone) <= 8", True),
        ("count(cobblestone) <= 7", False),
        ("count(cobblestone) == 8", True),
        ("count(cobblestone) == 7", False),
        ("count(cobblestone) != 7", True),
        ("count(cobblestone) != 8", False),
        ("count(cobblestone) >= 8", True),
        ("count(cobblestone) >= 9", False),
        ("count(cobblestone) > 7", True),
        ("count(cobblestone) > 8", False),
    ],
)
def test_every_comparison_operator(text: str, expected: bool) -> None:
    assert evaluate(text, INVENTORY) is expected
    assert is_verifiable(text) is True


def test_has_and_count_use_missing_items_as_zero() -> None:
    assert evaluate("has(cobblestone)", INVENTORY) is True
    assert evaluate("has(diamond)", INVENTORY) is False
    assert evaluate("count(diamond) == 0", INVENTORY) is True
    assert evaluate("has(diamond)", {}) is False


def test_and_not_and_parentheses() -> None:
    assert evaluate("has(oak_log) and count(cobblestone) >= 8", INVENTORY) is True
    assert evaluate("has(oak_log) and not has(diamond)", INVENTORY) is True
    assert evaluate("not (has(oak_log) and has(diamond))", INVENTORY) is True
    assert evaluate("not false", INVENTORY) is True
    assert evaluate("true", INVENTORY) is True
    assert is_verifiable("has(oak_log) and not has(diamond)") is True


def test_tags_count_every_matching_suffix() -> None:
    assert evaluate("count(#logs) >= 3", INVENTORY) is True
    assert evaluate("count(#logs) == 3", INVENTORY) is True
    assert evaluate("has(#logs)", INVENTORY) is True
    assert evaluate("has(#planks)", {"oak_planks": 2}) is True
    assert evaluate("count(#planks) >= 2", {"oak_planks": 2, "birch_planks": 1}) is True
    assert evaluate("count(#logs) >= 4", INVENTORY) is False


def test_unsupported_constructs_are_unverifiable() -> None:
    assert evaluate("near(#logs, 5)", INVENTORY) is None
    assert is_verifiable("near(#logs, 5)") is False
    assert evaluate("has(#unknown_tag)", INVENTORY) is None
    assert is_verifiable("has(#unknown_tag)") is False
    assert evaluate("has(oak_log) or has(diamond)", INVENTORY) is None
    assert is_verifiable("has(oak_log) or has(diamond)") is False
    # Unknown is three-valued: false AND unknown is still false.
    assert evaluate("has(diamond) and near(#logs, 5)", INVENTORY) is False
    assert evaluate("has(oak_log) and near(#logs, 5)", INVENTORY) is None


def test_not_propagates_unknown() -> None:
    assert evaluate("not near(#logs, 5)", INVENTORY) is None
    assert is_verifiable("not near(#logs, 5)") is False


def test_item_of_simple_has_and_count_shapes() -> None:
    assert item_of_condition("has(furnace)") == "furnace"
    assert item_of_condition("count(cobblestone) >= 8") == "cobblestone"
    assert item_of_condition("count(#logs) >= 1") == "#logs"
    assert item_of_condition("has(stone_pickaxe) and has(cobblestone)") is None
    assert item_of_condition("true") is None


@pytest.mark.parametrize(
    "text",
    [
        "",
        "   ",
        "has(",
        "has()",
        "count(x) >=",
        "count(x) = 1",
        "has(x) and",
        "not",
        "(has(x)",
        "has(x))",
        "count(x) >= 1 2",
        "has(x, y)",
    ],
)
def test_malformed_text_raises_with_a_position(text: str) -> None:
    with pytest.raises(ConditionError):
        evaluate(text, INVENTORY)
    with pytest.raises(ConditionError):
        is_verifiable(text)


def test_malformed_error_names_the_problem() -> None:
    with pytest.raises(ConditionError, match="position"):
        parse("has(x) and $")
    with pytest.raises(ConditionError, match="expected"):
        parse("count(x) >=")


def test_minecraft_namespace_is_normalized_on_both_sides() -> None:
    inventory = {"minecraft:oak_log": 3, "oak_log": 1, "minecraft:cobblestone": 8}
    assert evaluate("count(oak_log) == 4", inventory) is True
    assert evaluate("count(minecraft:oak_log) == 4", inventory) is True
    assert evaluate("has(minecraft:oak_log)", inventory) is True
    assert evaluate("count(minecraft:furnace) == 0", inventory) is True
    assert is_verifiable("count(minecraft:oak_log) >= 1") is True

    assert item_of_condition("has(minecraft:furnace)") == "furnace"
    assert item_of_condition("count(minecraft:oak_log) >= 1") == "oak_log"


def test_minecraft_tag_namespace_is_normalized() -> None:
    assert evaluate("count(#minecraft:logs) >= 3", INVENTORY) is True
    assert evaluate("has(#minecraft:logs)", INVENTORY) is True
    assert is_verifiable("count(#minecraft:logs) >= 1") is True
    assert item_of_condition("count(#minecraft:logs) >= 1") == "#logs"


def test_items_of_a_compound_condition_are_listed_in_text_order() -> None:
    assert items_of_condition(
        "count(#minecraft:logs) >= 4 and count(stick) >= 12"
    ) == ["#logs", "stick"]
    assert items_of_condition("has(furnace)") == ["furnace"]
    assert items_of_condition("has(a) and (not has(b) or count(c) >= 3)") == [
        "a",
        "b",
        "c",
    ]
    # Repeated names appear once; a bare name or a string names no item.
    assert items_of_condition("has(oak_log) and has(oak_log)") == ["oak_log"]
    assert items_of_condition("near(oak_log, 4)") == []
    assert items_of_condition('has("oak_log")') == []
    with pytest.raises(ConditionError):
        items_of_condition("has(x) and")


def test_identifiers_match_the_body_lexer() -> None:
    # `$` and `#` are tokens of their own, never identifier characters.
    with pytest.raises(ConditionError):
        parse("has(oak$wood)")
    with pytest.raises(ConditionError):
        parse("has(oak#wood)")
    # Adjacent `namespace:path` merges, as the body's `parse_name` does.
    assert evaluate("has(minecraft:oak_log)", INVENTORY) is True
    # A non-`minecraft` namespace parses but is not an inventory fact.
    assert evaluate("has(foo:oak_log)", INVENTORY) is None


def test_negative_integers_and_comments_are_accepted() -> None:
    assert evaluate("count(oak_log) != -1", INVENTORY) is True
    assert is_verifiable("count(oak_log) != -1") is True
    assert evaluate("count(#logs) >= 1 # from skill", INVENTORY) is True
    assert evaluate("has(oak_log) # logs", INVENTORY) is True
    # `#` followed by anything but a name starts a comment to end of line.
    assert evaluate("has(oak_log) #", INVENTORY) is True


def test_string_and_parameter_arguments_are_unverifiable() -> None:
    assert evaluate('near("table", 5)', INVENTORY) is None
    assert evaluate('has("oak_log")', INVENTORY) is None
    assert is_verifiable('has("oak_log")') is False
    assert evaluate('count(oak_log) >= "1"', INVENTORY) is None
    # A `$parameter` is valid guard text but not an inventory fact.
    assert evaluate("has($param)", {"param": 5}) is None
    assert is_verifiable("has($param)") is False


def test_canonical_text_ignores_spelling_and_namespaces() -> None:
    assert canonical_text("count(cobblestone)>=8") == "count(cobblestone) >= 8"
    assert (
        canonical_text("count(minecraft:cobblestone) >= 8")
        == "count(cobblestone) >= 8"
    )
    assert canonical_text("#logs") == "#logs"
    assert canonical_text("count(#minecraft:logs) >= 1 # x") == "count(#logs) >= 1"

def test_count_of_condition_is_the_goal_count() -> None:
    assert count_of_condition("count(cobblestone) >= 8") == 8
    assert count_of_condition("count(oak_log) >= 0") == 1  # never below one
    assert count_of_condition("has(furnace)") == 1
    assert count_of_condition("has(a) and count(b) >= 3") == 1


def test_food_and_health_are_not_items() -> None:
    """Run 9: `count(food) >= 2` counted an item called "food" (always 0)."""
    import pytest

    from praxion_brain.conditions import ConditionError, parse

    for text in ("count(food) >= 2", "has(food)", "count(health) >= 10"):
        with pytest.raises(ConditionError, match="is not an item"):
            parse(text)
    # The facts themselves and real food items still parse.
    for text in ("count_food() >= 2", "food() >= 10", "count(cooked_beef) >= 2", "has(bread)"):
        parse(text)

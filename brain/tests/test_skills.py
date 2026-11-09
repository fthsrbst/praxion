from __future__ import annotations

import json
from pathlib import Path
from types import SimpleNamespace

import pytest

from praxion_brain.skills import (
    SkillCatalog,
    SkillInfo,
    catalog_text,
    check,
    fact_names,
    load_catalog,
    parse_skill_text,
    primitive_names,
)

CORE_NAMES = {
    "get_cobblestone",
    "get_crafting_table",
    "get_iron_ingot",
    "get_crafting_table_near",
    "get_planks",
    "get_sticks",
    "get_stone_pickaxe",
    "get_stone_sword",
    "get_wood",
    "get_wooden_pickaxe",
    "find_stone",
    "find_ore",
    "search_surface",
}


def test_load_catalog_reads_the_real_core_skills(tmp_path) -> None:
    # An empty active directory: the machine's learned skills must not leak in.
    catalog = load_catalog(active_dir=tmp_path)
    assert set(catalog.names) == CORE_NAMES
    assert all(skill.origin == "core" for skill in catalog.skills)

    wood = catalog.by_name("get_wood")
    assert wood is not None
    assert wood.params == ["n: int = 1"]
    assert wood.doc == "Collect at least n logs of any wood type."
    assert wood.rules == 3

    cobblestone = catalog.by_name("get_cobblestone")
    assert cobblestone is not None
    assert cobblestone.params == ["n: int = 3"]
    assert cobblestone.rules == 6


def test_catalog_text_contains_every_skill_source() -> None:
    catalog = load_catalog()
    text = catalog_text(catalog)
    for skill in catalog.skills:
        assert skill.source.strip() in text


def test_primitive_and_fact_lists_come_from_the_core_skills() -> None:
    catalog = load_catalog()
    assert primitive_names(catalog) == [
        "branch_mine",
        "craft",
        "dig_down",
        "explore",
        "mine",
        "place",
        "smelt",
    ]
    assert fact_names(catalog) == ["count", "has", "near"]


def test_parser_handles_multiple_skills_and_comments(tmp_path) -> None:
    text = """\
# a comment with an arrow -> not a rule
skill one(x: int = 2)
  doc "One."
  rules:
    count(#logs) >= $x -> done
    true -> do two(3)

skill two()
  doc "Two."
  rules:
    true -> done
"""
    skills = parse_skill_text(text, Path("both.skill"), "core")
    assert [skill.name for skill in skills] == ["one", "two"]
    assert skills[0].params == ["x: int = 2"]
    assert skills[0].rules == 2
    assert skills[0].doc == "One."
    assert skills[1].rules == 1


def test_active_skill_replaces_a_core_skill_of_the_same_name(tmp_path) -> None:
    core = tmp_path / "core"
    active = tmp_path / "active"
    core.mkdir()
    active.mkdir()
    (core / "a.skill").write_text('skill get_x()\n  doc "core"\n  rules:\n    true -> done\n')
    (active / "a.skill").write_text('skill get_x()\n  doc "active"\n  rules:\n    true -> done\n')

    catalog = load_catalog(core, active)
    assert [skill.name for skill in catalog.skills] == ["get_x"]
    assert catalog.by_name("get_x").doc == "active"
    assert catalog.by_name("get_x").origin == "active"


def test_catalog_is_empty_when_active_does_not_exist(tmp_path) -> None:
    core = tmp_path / "core"
    core.mkdir()
    (core / "a.skill").write_text('skill get_x()\n  doc "x"\n  rules:\n    true -> done\n')
    catalog = load_catalog(core, tmp_path / "missing")
    assert catalog.names == ["get_x"]


def test_skill_catalog_by_name_returns_none_for_unknown() -> None:
    catalog = SkillCatalog(
        skills=[
            SkillInfo(
                name="get_x",
                doc="x",
                source="skill get_x()\n",
                path="x.skill",
                origin="core",
            )
        ]
    )
    assert catalog.by_name("get_x") is not None
    assert catalog.by_name("get_y") is None


def test_check_parses_the_body_json_contract(tmp_path, monkeypatch) -> None:
    candidates = tmp_path / "candidates"
    candidates.mkdir()
    binary = tmp_path / "praxion-body.exe"
    binary.write_bytes(b"stub")
    payload = {
        "ok": False,
        "candidates": [
            {
                "file": "get_x.skill",
                "skills": ["get_x"],
                "ok": False,
                "errors": [
                    {"kind": "too_many_rules", "message": "31 rules", "line": 4, "col": 2}
                ],
            }
        ],
    }

    def fake_run(command, **kwargs):
        assert command[1:3] == ["check-skills", "--candidates"]
        return SimpleNamespace(stdout=json.dumps(payload), stderr="", returncode=1)

    monkeypatch.setattr("praxion_brain.skills.subprocess.run", fake_run)
    report = check(candidates, binary=binary)
    assert report.ok is False
    assert report.candidates[0].errors[0].kind == "too_many_rules"
    assert report.candidates[0].errors[0].line == 4


def test_check_reports_a_missing_json_output(tmp_path, monkeypatch) -> None:
    candidates = tmp_path / "candidates"
    candidates.mkdir()
    binary = tmp_path / "praxion-body.exe"
    binary.write_bytes(b"stub")

    def fake_run(command, **kwargs):
        return SimpleNamespace(stdout="", stderr="boom", returncode=2)

    monkeypatch.setattr("praxion_brain.skills.subprocess.run", fake_run)
    with pytest.raises(RuntimeError, match="no JSON"):
        check(candidates, binary=binary)

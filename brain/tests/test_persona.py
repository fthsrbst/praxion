from __future__ import annotations

import subprocess
import sys

import pytest

from praxion_brain import persona


def test_there_are_six_distinct_personas_with_one_sentence_each() -> None:
    assert len(persona.PERSONAS) == 6
    assert len({item.id for item in persona.PERSONAS}) == 6
    assert all(item.style.endswith(".") and item.style.isascii() for item in persona.PERSONAS)


def test_pick_is_stable_and_covers_the_list(monkeypatch) -> None:
    monkeypatch.delenv(persona.PERSONA_ENV, raising=False)
    assert persona.pick("world-a") == persona.pick("world-a")
    drawn = {persona.pick(f"world-{n}").id for n in range(200)}
    assert drawn == {item.id for item in persona.PERSONAS}


def test_pick_does_not_depend_on_the_process_hash_seed(monkeypatch) -> None:
    monkeypatch.delenv(persona.PERSONA_ENV, raising=False)
    code = "from praxion_brain import persona; print(persona.pick('world-a').id)"
    ids = {
        subprocess.run(
            [sys.executable, "-c", code],
            env={"PYTHONHASHSEED": seed, "PATH": ""},
            capture_output=True,
            text=True,
            check=True,
        ).stdout.strip()
        for seed in ("1", "2", "3")
    }
    assert ids == {persona.pick("world-a").id}


def test_the_environment_pins_a_persona(monkeypatch) -> None:
    monkeypatch.setenv(persona.PERSONA_ENV, "hunter")
    assert persona.pick("anything").id == "hunter"


def test_an_unknown_pinned_persona_is_an_error(monkeypatch) -> None:
    monkeypatch.setenv(persona.PERSONA_ENV, "nope")
    with pytest.raises(ValueError, match="not a persona id"):
        persona.pick("anything")

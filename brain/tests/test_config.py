"""The admitted-skills directory is shared with the harness.

``PRAXION_SKILLS_ACTIVE`` is read by both ``praxion_brain.config`` and
``praxion_harness.body``, so the directory admission writes is the one the body
watches.
"""

from __future__ import annotations

from praxion_brain import config


def test_default_is_the_local_active_directory(monkeypatch):
    monkeypatch.delenv("PRAXION_SKILLS_ACTIVE", raising=False)
    assert config.active_skills_dir() == config.repo_root() / "local" / "skills" / "active"


def test_an_absolute_override_is_used_as_is(monkeypatch, tmp_path):
    monkeypatch.setenv("PRAXION_SKILLS_ACTIVE", str(tmp_path))
    assert config.active_skills_dir() == tmp_path


def test_a_relative_override_resolves_against_the_repository(monkeypatch):
    monkeypatch.setenv("PRAXION_SKILLS_ACTIVE", "local/skills/other")
    assert config.active_skills_dir() == config.repo_root() / "local" / "skills" / "other"

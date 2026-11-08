"""The watched admitted-skills directory comes from one shared source.

``PRAXION_SKILLS_ACTIVE`` is read here and by ``praxion_brain.config``, so a
custom directory cannot leave the body watching a place admission never writes.
"""

from praxion_harness.body import active_skills_dir
from praxion_harness.paths import repo_root


def test_default_is_the_brain_local_directory(monkeypatch):
    monkeypatch.delenv("PRAXION_SKILLS_ACTIVE", raising=False)
    assert active_skills_dir() == repo_root() / "local" / "skills" / "active"


def test_an_absolute_override_is_used_as_is(monkeypatch, tmp_path):
    monkeypatch.setenv("PRAXION_SKILLS_ACTIVE", str(tmp_path))
    assert active_skills_dir() == tmp_path


def test_a_relative_override_resolves_against_the_repository(monkeypatch):
    monkeypatch.setenv("PRAXION_SKILLS_ACTIVE", "local/skills/other")
    assert active_skills_dir() == repo_root() / "local" / "skills" / "other"

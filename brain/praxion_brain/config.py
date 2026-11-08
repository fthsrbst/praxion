"""Repository paths and the machine-local ``.env``.

This is the only module that loads the environment and the only module that
adds ``harness/`` to ``sys.path``; every other module imports the settings it
needs from here. Nothing in this module prints a value (the ``.env`` holds API
keys), and every path is inside the repository.
"""

from __future__ import annotations

import os
import sys
from pathlib import Path

#: Values from ``.env`` already present in the environment win; the file is a
#: convenience for a fresh shell, not an override.
_ENV_LOADED = False


def repo_root() -> Path:
    """The repository root: the nearest ancestor holding ``.git``."""
    start = Path(__file__).resolve().parent
    for directory in (start, *start.parents):
        if (directory / ".git").exists() or (directory / "versions" / "manifest.toml").exists():
            return directory
    # brain/praxion_brain/config.py -> brain -> repo.
    return Path(__file__).resolve().parent.parent.parent


def load_env(path: Path | None = None) -> None:
    """Load ``KEY=value`` lines from ``.env`` without overriding the shell.

    Supports comments, blank lines, an optional ``export`` prefix and single or
    double quotes around the value. Values are never printed. Calling twice is
    harmless.
    """
    global _ENV_LOADED
    if _ENV_LOADED:
        return
    _ENV_LOADED = True
    env_path = Path(path) if path is not None else repo_root() / ".env"
    try:
        text = env_path.read_text(encoding="utf-8")
    except OSError:
        return
    for raw in text.splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("export "):
            line = line[len("export ") :].lstrip()
        key, separator, value = line.partition("=")
        if not separator:
            continue
        key = key.strip()
        value = value.strip()
        if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
            value = value[1:-1]
        if key:
            os.environ.setdefault(key, value)


load_env()

# Give the harness package one import surface. Imported here, never at call
# sites, so `from praxion_harness...` works after any `praxion_brain` import.
_HARNESS = repo_root() / "harness"
if str(_HARNESS) not in sys.path:
    sys.path.insert(0, str(_HARNESS))


def core_skills_dir() -> Path:
    """The product-owned skill library (``body/skills``)."""
    return repo_root() / "body" / "skills"


def local_dir() -> Path:
    """Machine-local data; ignored by git."""
    return repo_root() / "local"


def candidates_dir() -> Path:
    """The latest candidate copy for human inspection."""
    return local_dir() / "skills" / "candidates"


def active_skills_dir() -> Path:
    """Admitted skills; a run gets these in addition to the core skills.

    ``PRAXION_SKILLS_ACTIVE`` is the one override the harness
    (``praxion_harness.body.active_skills_dir``) also reads, so the directory
    the body watches is always the one admission writes. A relative path is
    resolved against the repository root.
    """
    value = os.environ.get("PRAXION_SKILLS_ACTIVE")
    if value:
        path = Path(value).expanduser()
        return path if path.is_absolute() else repo_root() / path
    return local_dir() / "skills" / "active"


def local_brain_dir() -> Path:
    """Ledger, evidence and admission attempts."""
    return local_dir() / "brain"


def attempts_dir() -> Path:
    """One candidate directory per admission attempt."""
    return local_brain_dir() / "attempts"


def ledger_path() -> Path:
    return local_brain_dir() / "ledger.jsonl"


def body_binary() -> Path:
    suffix = ".exe" if os.name == "nt" else ""
    return repo_root() / "body" / "target" / "release" / f"praxion-body{suffix}"


__all__ = [
    "active_skills_dir",
    "attempts_dir",
    "body_binary",
    "candidates_dir",
    "core_skills_dir",
    "ledger_path",
    "load_env",
    "local_brain_dir",
    "local_dir",
    "repo_root",
]

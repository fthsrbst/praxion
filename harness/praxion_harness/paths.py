"""Resolve the Java runtime and the Minecraft server jar.

Resolution order (per the Demo-0 step-2 task):
  1. Environment variables ``PRAXION_JAVA`` and ``PRAXION_SERVER_JAR``.
  2. ``local/paths.toml`` keys ``java`` and ``server_jar`` (written by setup; may not exist).
  3. Otherwise a clear error.

Only stdlib. Never writes outside the repository.
"""

from __future__ import annotations

import os
import tomllib
from pathlib import Path


class PathResolutionError(RuntimeError):
    """A required path (Java or server jar) could not be resolved."""




def repo_root() -> Path:
    """The repository root: the nearest ancestor holding ``.git`` or the version manifest."""
    start = Path(__file__).resolve().parent
    for directory in (start, *start.parents):
        if (directory / ".git").exists() or (directory / "versions" / "manifest.toml").exists():
            return directory
    # Fall back to three levels up (harness/praxion_harness -> harness -> repo).
    return Path(__file__).resolve().parent.parent.parent


def _read_paths_toml() -> dict:
    candidate = repo_root() / "local" / "paths.toml"
    if not candidate.is_file():
        return {}
    try:
        text = candidate.read_text(encoding="utf-8")
    except OSError:
        return {}
    try:
        data = tomllib.loads(text)
    except tomllib.TOMLDecodeError:
        return {}
    if not isinstance(data, dict):
        return {}
    return data


def _resolve(key: str, env_name: str) -> Path:
    raw = os.environ.get(env_name)
    if raw:
        path = Path(raw).expanduser()
        if not path.is_absolute():
            path = repo_root() / path
        return path
    data = _read_paths_toml()
    value = data.get(key)
    if isinstance(value, str) and value.strip():
        path = Path(value).expanduser()
        if not path.is_absolute():
            path = repo_root() / path
        return path
    raise PathResolutionError(
        f"could not resolve {key!r}: set env var {env_name} or add {key!r} to "
        f"{repo_root() / 'local' / 'paths.toml'}"
    )


def resolve_java() -> Path:
    path = _resolve("java", "PRAXION_JAVA")
    if not path.is_file():
        raise PathResolutionError(f"java executable not found: {path}")
    return path


def resolve_server_jar() -> Path:
    path = _resolve("server_jar", "PRAXION_SERVER_JAR")
    if not path.is_file():
        raise PathResolutionError(f"server jar not found: {path}")
    return path


__all__ = ["PathResolutionError", "repo_root", "resolve_java", "resolve_server_jar"]
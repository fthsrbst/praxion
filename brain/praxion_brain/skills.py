"""Read the skill catalog and run the body's static skill checker.

The Python side does only line-level parsing: a ``skill NAME(params)`` header,
its ``doc "...")`` line and how many rules it has. The real DSL parser is the
body's, reached through ``praxion-body check-skills``.
"""

from __future__ import annotations

import json
import re
import subprocess
from pathlib import Path
from typing import Literal

from pydantic import BaseModel, Field

from . import config

_HEADER = re.compile(
    r"^skill\s+([A-Za-z_][A-Za-z0-9_]*)\s*\((.*?)\)\s*$",
    re.MULTILINE,
)
_DOC = re.compile(r'^\s*doc\s+"(.*)"\s*$', re.MULTILINE)
_CALL = re.compile(r"\b([A-Za-z_][A-Za-z0-9_]*)\s*\(")
_KEYWORDS = {"true", "false", "and", "or", "not"}


class SkillInfo(BaseModel):
    name: str
    params: list[str] = Field(default_factory=list)
    doc: str = ""
    rules: int = 0
    source: str
    path: str
    origin: Literal["core", "active"]


class SkillCatalog(BaseModel):
    skills: list[SkillInfo] = Field(default_factory=list)

    @property
    def names(self) -> list[str]:
        return [skill.name for skill in self.skills]

    def by_name(self, name: str) -> SkillInfo | None:
        for skill in self.skills:
            if skill.name == name:
                return skill
        return None


class CheckError(BaseModel):
    kind: str
    message: str
    line: int | None = None
    col: int | None = None


class CandidateReport(BaseModel):
    file: str
    skills: list[str] = Field(default_factory=list)
    ok: bool
    errors: list[CheckError] = Field(default_factory=list)


class CheckReport(BaseModel):
    ok: bool
    candidates: list[CandidateReport] = Field(default_factory=list)


# ---------------------------------------------------------------------- #
# Parsing
# ---------------------------------------------------------------------- #
def _split_params(raw: str) -> list[str]:
    raw = raw.strip()
    if not raw:
        return []
    parts: list[str] = []
    depth = 0
    start = 0
    for index, char in enumerate(raw):
        if char in "([{":
            depth += 1
        elif char in ")]}":
            depth -= 1
        elif char == "," and depth == 0:
            parts.append(raw[start:index].strip())
            start = index + 1
    parts.append(raw[start:].strip())
    return [part for part in parts if part]


def _count_rules(block: str) -> int:
    lines = block.splitlines()
    try:
        start = next(index for index, line in enumerate(lines) if line.strip().startswith("rules:"))
    except StopIteration:
        return 0
    count = 0
    for line in lines[start + 1 :]:
        stripped = line.strip()
        if not stripped or stripped.startswith("#") or stripped.startswith("//"):
            continue
        if "->" in stripped:
            count += 1
    return count


def parse_skill_text(text: str, path: Path | str, origin: Literal["core", "active"]) -> list[SkillInfo]:
    """Parse one file; a file may hold more than one skill."""
    matches = list(_HEADER.finditer(text))
    skills: list[SkillInfo] = []
    for index, match in enumerate(matches):
        end = matches[index + 1].start() if index + 1 < len(matches) else len(text)
        block = text[match.start() : end]
        doc_match = _DOC.search(block)
        skills.append(
            SkillInfo(
                name=match.group(1),
                params=_split_params(match.group(2)),
                doc=doc_match.group(1) if doc_match else "",
                rules=_count_rules(block),
                source=block.rstrip() + "\n",
                path=str(path),
                origin=origin,
            )
        )
    return skills


def _read_dir(directory: Path, origin: Literal["core", "active"]) -> list[SkillInfo]:
    if not directory.is_dir():
        return []
    skills: list[SkillInfo] = []
    for path in sorted(directory.glob("*.skill")):
        skills.extend(parse_skill_text(path.read_text(encoding="utf-8"), path, origin))
    return skills


def load_catalog(
    core_dir: Path | None = None,
    active_dir: Path | None = None,
) -> SkillCatalog:
    """Core skills first, then active skills; an active name replaces a core one."""
    core = Path(core_dir) if core_dir is not None else config.core_skills_dir()
    active = Path(active_dir) if active_dir is not None else config.active_skills_dir()
    if not core.is_dir():
        raise FileNotFoundError(f"core skills directory not found: {core}")
    merged: dict[str, SkillInfo] = {}
    for skill in _read_dir(core, "core") + _read_dir(active, "active"):
        merged[skill.name] = skill
    return SkillCatalog(skills=list(merged.values()))


# ---------------------------------------------------------------------- #
# Prompt material
# ---------------------------------------------------------------------- #
def catalog_text(catalog: SkillCatalog) -> str:
    """The full text of every skill, with a stable header for prompt caching."""
    blocks = []
    for skill in catalog.skills:
        blocks.append(
            f"### {skill.origin} skill {skill.name} ({Path(skill.path).name})\n{skill.source.rstrip()}"
        )
    return "\n\n".join(blocks)


def primitive_names(catalog: SkillCatalog) -> list[str]:
    """Actions used after ``->`` by the catalog, excluding ``do``, ``done`` and ``fail``.

    ``fail(kind)`` ends a frame with a typed failure; it is part of the DSL, not a primitive.
    """
    names: set[str] = set()
    for skill in catalog.skills:
        for line in skill.source.splitlines():
            stripped = line.strip()
            if not stripped or stripped.startswith("#") or "->" not in stripped:
                continue
            action = stripped.split("->", 1)[1].strip()
            if action.startswith("do "):
                continue
            match = _CALL.match(action)
            if match and match.group(1) != "fail":
                names.add(match.group(1))
    return sorted(names)


def fact_names(catalog: SkillCatalog) -> list[str]:
    """Calls used in guards before ``->`` (``count``, ``has``, ``near``)."""
    names: set[str] = set()
    for skill in catalog.skills:
        for line in skill.source.splitlines():
            stripped = line.strip()
            if not stripped or stripped.startswith("#") or "->" not in stripped:
                continue
            guard = stripped.split("->", 1)[0]
            names.update(
                name for name in _CALL.findall(guard) if name not in _KEYWORDS
            )
    return sorted(names)


# ---------------------------------------------------------------------- #
# Static check (body binary)
# ---------------------------------------------------------------------- #
def check(
    candidate_dir: Path | str,
    *,
    binary: Path | None = None,
    timeout: float = 60.0,
) -> CheckReport:
    """Run ``praxion-body check-skills --candidates DIR`` and parse its JSON."""
    directory = Path(candidate_dir)
    if not directory.is_dir():
        raise FileNotFoundError(f"candidate directory not found: {directory}")
    executable = Path(binary) if binary is not None else config.body_binary()
    if not executable.exists():
        raise RuntimeError(
            f"the body binary is not built ({executable}); run: cd body && cargo build --release"
        )
    command = [str(executable), "check-skills", "--candidates", str(directory)]
    try:
        completed = subprocess.run(
            command,
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            timeout=timeout,
        )
    except subprocess.TimeoutExpired as error:
        raise RuntimeError(
            f"check-skills timed out after {timeout:.0f}s for {directory}"
        ) from error
    stdout = completed.stdout.strip()
    if not stdout:
        raise RuntimeError(
            f"check-skills printed no JSON (exit {completed.returncode}) for {directory}: "
            f"{(completed.stderr or '').strip()[-2000:]}"
        )
    try:
        data = json.loads(stdout)
    except json.JSONDecodeError as error:
        raise RuntimeError(
            f"check-skills printed invalid JSON (exit {completed.returncode}) for {directory}: "
            f"{stdout[:2000]}"
        ) from error
    try:
        return CheckReport.model_validate(data)
    except Exception as error:  # pydantic's message carries the field names
        raise RuntimeError(
            f"check-skills JSON did not match the contract: {error}"
        ) from error


__all__ = [
    "CandidateReport",
    "CheckError",
    "CheckReport",
    "SkillCatalog",
    "SkillInfo",
    "catalog_text",
    "check",
    "fact_names",
    "load_catalog",
    "parse_skill_text",
    "primitive_names",
]

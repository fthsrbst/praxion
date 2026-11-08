"""Admission: write, check, practise, repair, and only then go active.

The control flow is deterministic and the three expensive steps are injected
callables, so the flow is unit-tested without a model, a Jev key or a server.
"""

from __future__ import annotations

import shutil
from datetime import datetime, timezone
from pathlib import Path
from typing import Callable, Literal, Sequence

from pydantic import BaseModel, Field

from . import config
from .evidence import Evidence
from .goal import Goal
from .ledger import Ledger
from .practice import PracticeResult, practice
from .skills import (
    CandidateReport,
    CheckReport,
    SkillCatalog,
    check,
    load_catalog,
)
from .writer import FailureContext, SkillDraft, write

WriteFn = Callable[..., SkillDraft]
CheckFn = Callable[[Path], CheckReport]
PracticeFn = Callable[..., PracticeResult]


class AttemptRecord(BaseModel):
    attempt: int
    model: str
    candidate_path: str
    static_ok: bool
    check: CheckReport | None = None
    practices: list[PracticeResult] = Field(default_factory=list)
    failure_kind: str | None = None
    failure_digest: str | None = None


class AdmissionReport(BaseModel):
    goal: Goal
    status: Literal["admitted", "rejected"]
    skill_name: str
    active_path: str | None = None
    attempts: list[AttemptRecord] = Field(default_factory=list)
    repairs_used: int = 0
    reason: str | None = None
    report_path: str | None = None


# ---------------------------------------------------------------------- #
def _candidate(report: CheckReport, filename: str) -> CandidateReport | None:
    for candidate in report.candidates:
        if Path(candidate.file).name == filename:
            return candidate
    return None


def _static_failure(report: CheckReport, filename: str) -> tuple[str, str]:
    candidate = _candidate(report, filename)
    if candidate is None:
        return "no_skill", f"check-skills found no skill in {filename}"
    errors = candidate.errors
    if not errors:
        return "check", f"check-skills rejected {filename} without a typed error"
    lines = []
    for error in errors:
        location = ""
        if error.line is not None:
            location = f" (line {error.line}"
            if error.col is not None:
                location += f", col {error.col}"
            location += ")"
        lines.append(f"{error.kind}: {error.message}{location}")
    return errors[0].kind, "\n".join(lines)


def _practice_failure(result: PracticeResult) -> tuple[str, str]:
    kind = result.failure_kind or ("goal_not_met" if not result.passed else "practice")
    return kind, result.digest


def _write_report(report: AdmissionReport, evidence: Evidence) -> AdmissionReport:
    evidence.directory.mkdir(parents=True, exist_ok=True)
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S%fZ")
    path = evidence.directory / f"admission-{report.skill_name}-{stamp}.json"
    report.report_path = str(path)
    path.write_text(report.model_dump_json(indent=2), encoding="utf-8")
    return report


def admit(
    goal: Goal,
    writer_model: str = "gpt-6-luna",
    seeds: Sequence[int] = (101, 202, 303),
    held_out: int = 404,
    max_repairs: int = 3,
    *,
    max_ticks: int = 12_000,
    write_fn: WriteFn | None = None,
    check_fn: CheckFn | None = None,
    practice_fn: PracticeFn | None = None,
    evidence: Evidence | None = None,
    ledger: Ledger | None = None,
    catalog: SkillCatalog | None = None,
    attempts_root: Path | None = None,
    candidates_root: Path | None = None,
    active_dir: Path | None = None,
    escalation_model: str | None = "gpt-6.1-terra",
    effort: str = "low",
) -> AdmissionReport:
    """Run the admission loop. Returns a report that is also written as JSON."""
    write_fn = write_fn if write_fn is not None else write
    check_fn = check_fn if check_fn is not None else check
    practice_fn = practice_fn if practice_fn is not None else practice
    evidence = evidence if evidence is not None else Evidence(config.local_brain_dir())
    ledger = ledger if ledger is not None else Ledger()
    catalog = catalog if catalog is not None else load_catalog()
    attempts_root = Path(attempts_root) if attempts_root is not None else config.attempts_dir()
    candidates_root = (
        Path(candidates_root) if candidates_root is not None else config.candidates_dir()
    )
    active_dir = Path(active_dir) if active_dir is not None else config.active_skills_dir()
    attempts_root.mkdir(parents=True, exist_ok=True)
    candidates_root.mkdir(parents=True, exist_ok=True)

    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S%fZ")
    filename = f"{goal.skill_name}.skill"
    failure: FailureContext | None = None
    attempts: list[AttemptRecord] = []
    active_path: str | None = None
    admitted = False
    reason: str | None = None

    for attempt in range(max_repairs + 1):
        model = writer_model
        if attempt >= 2 and writer_model == "gpt-6-luna" and escalation_model:
            model = escalation_model

        attempt_dir = attempts_root / f"{stamp}-{attempt}"
        attempt_dir.mkdir(parents=True, exist_ok=True)
        draft = write_fn(
            goal=goal,
            catalog=catalog,
            failure=failure,
            model=model,
            ledger=ledger,
            evidence=evidence,
            effort=effort,
        )
        candidate_path = attempt_dir / filename
        candidate_path.write_text(draft.skill_source, encoding="utf-8")
        shutil.copyfile(candidate_path, candidates_root / filename)

        record = AttemptRecord(
            attempt=attempt,
            model=model,
            candidate_path=str(candidate_path),
            static_ok=False,
        )
        evidence.write(
            "admission_attempt",
            attempt=attempt,
            model=model,
            candidate_path=str(candidate_path),
            rationale=draft.rationale,
        )

        report = check_fn(attempt_dir)
        record.check = report
        record.static_ok = bool(report.ok) and _candidate(report, filename) is not None
        candidate_report = _candidate(report, filename)
        if candidate_report is not None:
            record.static_ok = record.static_ok and bool(candidate_report.ok)

        if not record.static_ok:
            kind, digest = _static_failure(report, filename)
            record.failure_kind = kind
            record.failure_digest = digest
            attempts.append(record)
            evidence.write(
                "admission_static_failure",
                attempt=attempt,
                failure_kind=kind,
                digest=digest,
            )
            failure = FailureContext(
                kind=kind,
                digest=digest,
                previous_source=draft.skill_source,
            )
            continue

        failed: PracticeResult | None = None
        for seed in (*seeds, held_out):
            result = practice_fn(
                skill=goal.skill_name,
                goal=goal,
                candidate_dir=attempt_dir,
                seed=seed,
                max_ticks=max_ticks,
            )
            record.practices.append(result)
            evidence.write(
                "admission_practice",
                attempt=attempt,
                seed=seed,
                passed=result.passed,
                outcome=result.outcome,
                run_dir=result.run_dir,
                digest=result.digest,
            )
            if not result.passed:
                failed = result
                break

        if failed is not None:
            kind, digest = _practice_failure(failed)
            record.failure_kind = kind
            record.failure_digest = digest
            attempts.append(record)
            failure = FailureContext(
                kind=kind,
                digest=digest,
                previous_source=draft.skill_source,
            )
            continue

        attempts.append(record)
        admitted = True
        active_dir.mkdir(parents=True, exist_ok=True)
        target = active_dir / filename
        shutil.copyfile(candidate_path, target)
        active_path = str(target)
        break

    if not admitted:
        reason = f"rejected after {len(attempts)} attempt(s)"

    report = AdmissionReport(
        goal=goal,
        status="admitted" if admitted else "rejected",
        skill_name=goal.skill_name,
        active_path=active_path,
        attempts=attempts,
        repairs_used=max(0, len(attempts) - 1),
        reason=reason,
    )
    report = _write_report(report, evidence)
    evidence.write("admission_report", report=report.model_dump())
    return report


__all__ = ["AdmissionReport", "AttemptRecord", "admit"]

"""Admission: write, check, practise, repair, and only then go active.

The control flow is deterministic and the three expensive steps are injected
callables, so the flow is unit-tested without a model, a Jev key or a server.
"""

from __future__ import annotations

import json
import shutil
from datetime import datetime, timezone
from pathlib import Path
from typing import Callable, Literal, Sequence

from pydantic import BaseModel, Field

from . import config
from .evidence import Evidence
from .goal import Goal
from .ledger import Ledger, estimate_cost
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

#: Seeds used when a body failure makes a practice seed unusable.
SPARE_SEEDS = (505, 606, 707)
#: Body failures an admission tolerates before it stops without a verdict.
BODY_FAILURE_LIMIT = 3
#: Estimated tokens of one writer call, used by the governor before admission.
WRITE_INPUT_ESTIMATE = 4_000
WRITE_OUTPUT_ESTIMATE = 2_000
#: Attempts before the writer escalates to the stronger model.
WRITE_ESCALATE_AFTER = 2


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
    status: Literal["admitted", "rejected", "inconclusive_body"]
    skill_name: str
    active_path: str | None = None
    attempts: list[AttemptRecord] = Field(default_factory=list)
    repairs_used: int = 0
    body_failures: int = 0
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


def write_model(
    writer_model: str,
    attempt: int,
    escalation_model: str | None = "gpt-6.1-terra",
) -> str:
    """The model one admission attempt uses.

    One owner for the escalation rule: the attempt's call and the governor's
    pre-call estimate must agree (A5).
    """
    if (
        attempt >= WRITE_ESCALATE_AFTER
        and writer_model == "gpt-6-luna"
        and escalation_model
    ):
        return escalation_model
    return writer_model


def estimate(writer_model: str, *, attempt: int = 0) -> float:
    """The governor's pre-call estimate for one writer call of this admission."""
    return estimate_cost(
        write_model(writer_model, attempt), WRITE_INPUT_ESTIMATE, WRITE_OUTPUT_ESTIMATE
    )


def _record_body_failure(path: Path, result: PracticeResult) -> None:
    """Append one body failure for the orchestrator to triage."""
    path.parent.mkdir(parents=True, exist_ok=True)
    entry = {
        "seed": result.seed,
        "run_dir": result.run_dir,
        "failure_kind": result.failure_kind,
        "digest": result.digest,
    }
    with path.open("a", encoding="utf-8") as handle:
        handle.write(json.dumps(entry) + "\n")


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
    spare_seeds: Sequence[int] = SPARE_SEEDS,
    body_failures_path: Path | None = None,
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
    body_failures_path = (
        Path(body_failures_path)
        if body_failures_path is not None
        else config.local_brain_dir() / "body_failures.jsonl"
    )
    attempts_root.mkdir(parents=True, exist_ok=True)
    candidates_root.mkdir(parents=True, exist_ok=True)

    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S%fZ")
    filename = f"{goal.skill_name}.skill"
    failure: FailureContext | None = None
    attempts: list[AttemptRecord] = []
    active_path: str | None = None
    admitted = False
    inconclusive = False
    reason: str | None = None
    body_failures = 0
    spares = list(spare_seeds)
    required_passes = len(seeds)

    for attempt in range(max_repairs + 1):
        model = write_model(writer_model, attempt, escalation_model)

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
        passed_seeds: list[int] = []
        held_out_passed = False
        seeds_to_run = [*seeds, held_out]

        index = 0
        while index < len(seeds_to_run):
            seed = seeds_to_run[index]
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
                failure_class=result.failure_class,
                run_dir=result.run_dir,
                digest=result.digest,
            )
            if result.passed:
                passed_seeds.append(seed)
                if seed == held_out:
                    held_out_passed = True
                index += 1
                continue

            if result.failure_class != "body":
                failed = result
                break

            body_failures += 1
            _record_body_failure(body_failures_path, result)
            evidence.write(
                "admission_body_failure",
                attempt=attempt,
                seed=seed,
                failure_kind=result.failure_kind,
                run_dir=result.run_dir,
            )
            if body_failures >= BODY_FAILURE_LIMIT or not spares:
                inconclusive = True
                reason = (
                    f"inconclusive_body after {len(attempts) + 1} attempt(s): "
                    f"{body_failures} body failure(s)"
                )
                break
            seeds_to_run[index] = spares.pop(0)

        if inconclusive:
            attempts.append(record)
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

        training_passes = sum(1 for seed in passed_seeds if seed != held_out)
        if not (held_out_passed and training_passes >= required_passes):
            attempts.append(record)
            inconclusive = True
            reason = (
                f"inconclusive_body after {len(attempts)} attempt(s): "
                f"{body_failures} body failure(s), no held-out pass"
            )
            break

        attempts.append(record)
        admitted = True
        active_dir.mkdir(parents=True, exist_ok=True)
        target = active_dir / filename
        shutil.copyfile(candidate_path, target)
        active_path = str(target)
        break

    if not admitted and not inconclusive:
        reason = f"rejected after {len(attempts)} attempt(s)"

    status: Literal["admitted", "rejected", "inconclusive_body"]
    if admitted:
        status = "admitted"
    elif inconclusive:
        status = "inconclusive_body"
    else:
        status = "rejected"

    report = AdmissionReport(
        goal=goal,
        status=status,
        skill_name=goal.skill_name,
        active_path=active_path,
        attempts=attempts,
        repairs_used=max(0, len(attempts) - 1),
        body_failures=body_failures,
        reason=reason,
    )
    report = _write_report(report, evidence)
    evidence.write("admission_report", report=report.model_dump())
    return report


__all__ = [
    "BODY_FAILURE_LIMIT",
    "SPARE_SEEDS",
    "WRITE_ESCALATE_AFTER",
    "WRITE_INPUT_ESTIMATE",
    "WRITE_OUTPUT_ESTIMATE",
    "AdmissionReport",
    "AttemptRecord",
    "admit",
    "estimate",
    "write_model",
]

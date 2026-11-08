from __future__ import annotations

import json
from pathlib import Path

import pytest

from praxion_brain.admission import (
    WRITE_ESCALATE_AFTER,
    WRITE_INPUT_ESTIMATE,
    WRITE_OUTPUT_ESTIMATE,
    AdmissionCancelled,
    admit,
    estimate,
    write_model,
)
from praxion_brain.cancel import CancelToken
from praxion_brain.evidence import Evidence
from praxion_brain.goal import Goal
from praxion_brain.ledger import Ledger, estimate_cost
from praxion_brain.practice import PracticeResult
from praxion_brain.skills import (
    CandidateReport,
    CheckError,
    CheckReport,
    load_catalog,
)
from praxion_brain.writer import SkillDraft

SKILL = "get_furnace"


def _goal() -> Goal:
    return Goal(text="have a furnace", item="furnace")


def _ok_report() -> CheckReport:
    return CheckReport(
        ok=True,
        candidates=[CandidateReport(file=f"{SKILL}.skill", skills=[SKILL], ok=True)],
    )


def _failed_report(kind: str = "too_many_rules", message: str = "31 rules") -> CheckReport:
    return CheckReport(
        ok=False,
        candidates=[
            CandidateReport(
                file=f"{SKILL}.skill",
                skills=[],
                ok=False,
                errors=[CheckError(kind=kind, message=message, line=3, col=1)],
            )
        ],
    )


class WriterSpy:
    def __init__(self, sources: list[str] | None = None) -> None:
        self.sources = sources or ['skill get_furnace()\n  doc "x"\n  rules:\n    true -> done\n']
        self.calls: list[dict] = []

    def __call__(self, goal, catalog, failure, model, **kwargs):
        self.calls.append({"goal": goal, "failure": failure, "model": model, **kwargs})
        source = self.sources[min(len(self.calls) - 1, len(self.sources) - 1)]
        return SkillDraft(skill_source=source, rationale="because")


class CheckSpy:
    def __init__(self, reports: list[CheckReport]) -> None:
        self.reports = list(reports)
        self.directories: list[Path] = []

    def __call__(self, directory):
        self.directories.append(Path(directory))
        if not self.reports:
            raise AssertionError("more checks than reports")
        return self.reports.pop(0)


def _practice(fail=lambda attempt, seed: False):
    """``fail`` returns False/None to pass, True/"skill" for a skill failure
    and "body" for a body failure."""
    calls: list[dict] = []

    def run(*, skill, goal, candidate_dir, seed, max_ticks, cancel=None):
        attempt = int(Path(candidate_dir).name.rsplit("-", 1)[-1])
        calls.append(
            {
                "skill": skill,
                "goal": goal,
                "candidate_dir": Path(candidate_dir),
                "seed": seed,
                "max_ticks": max_ticks,
                "attempt": attempt,
            }
        )
        decision = fail(attempt, seed)
        if decision == "body":
            failure_class = "body"
            outcome = "failed(no_block)"
            failure_kind = "no_block"
        elif decision:
            failure_class = "skill"
            outcome = "failed(no_rule)"
            failure_kind = "no_rule"
        else:
            failure_class = None
            outcome = "done"
            failure_kind = None
        passed = failure_class is None
        return PracticeResult(
            passed=passed,
            outcome=outcome,
            ticks=100,
            receipts={"mine": 1} if passed else {},
            deaths=0,
            run_dir=f"/runs/seed-{seed}",
            digest="failure kind: none"
            if passed
            else f"failure kind: {failure_kind}\nrule_selected get_furnace rule=3",
            seed=seed,
            failure_kind=failure_kind,
            failure_class=failure_class,
        )

    run.calls = calls  # type: ignore[attr-defined]
    return run


def _run(tmp_path, **kwargs):
    defaults = dict(
        writer_model="gpt-6-luna",
        seeds=(101, 202, 303),
        held_out=404,
        write_fn=WriterSpy(),
        check_fn=CheckSpy([_ok_report()] * 10),
        practice_fn=_practice(),
        evidence=Evidence(tmp_path / "evidence"),
        ledger=Ledger(tmp_path / "ledger.jsonl"),
        catalog=load_catalog(),
        attempts_root=tmp_path / "attempts",
        candidates_root=tmp_path / "candidates",
        active_dir=tmp_path / "active",
        body_failures_path=tmp_path / "body_failures.jsonl",
        max_ticks=50,
    )
    defaults.update(kwargs)
    return admit(_goal(), **defaults)


def test_clean_attempt_is_admitted_and_copied_active(tmp_path) -> None:
    writer = WriterSpy(["skill get_furnace()\n  doc \"clean\"\n  rules:\n    true -> done\n"])
    practice = _practice()
    report = _run(tmp_path, write_fn=writer, practice_fn=practice)

    assert report.status == "admitted"
    assert report.repairs_used == 0
    assert len(report.attempts) == 1
    active = tmp_path / "active" / f"{SKILL}.skill"
    assert active.read_text(encoding="utf-8") == writer.sources[0]
    assert report.active_path == str(active)
    assert (tmp_path / "candidates" / f"{SKILL}.skill").is_file()
    assert [call["seed"] for call in practice.calls] == [101, 202, 303, 404]
    assert all(call["max_ticks"] == 50 for call in practice.calls)
    assert report.report_path is not None and Path(report.report_path).is_file()
    saved = json.loads(Path(report.report_path).read_text(encoding="utf-8"))
    assert saved["status"] == "admitted"


def test_static_failure_goes_back_to_the_writer_then_passes(tmp_path) -> None:
    writer = WriterSpy(["bad source", "good source"])
    check = CheckSpy([_failed_report(), _ok_report()])
    report = _run(tmp_path, write_fn=writer, check_fn=check)

    assert report.status == "admitted"
    assert report.repairs_used == 1
    assert len(report.attempts) == 2
    assert report.attempts[0].static_ok is False
    assert report.attempts[0].failure_kind == "too_many_rules"
    assert writer.calls[0]["failure"] is None
    assert writer.calls[1]["failure"].kind == "too_many_rules"
    assert writer.calls[1]["failure"].previous_source == "bad source"
    assert "31 rules" in writer.calls[1]["failure"].digest
    assert check.directories[0] != check.directories[1]


def test_practice_failure_goes_back_to_the_writer_then_passes(tmp_path) -> None:
    writer = WriterSpy()
    practice = _practice(fail=lambda attempt, seed: attempt == 0)
    report = _run(tmp_path, write_fn=writer, practice_fn=practice)

    assert report.status == "admitted"
    assert len(report.attempts) == 2
    assert report.repairs_used == 1
    assert len(report.attempts[0].practices) == 1  # stopped at the first failure
    assert report.attempts[0].practices[0].failure_class == "skill"
    assert writer.calls[1]["failure"].kind == "no_rule"
    assert "rule_selected get_furnace rule=3" in writer.calls[1]["failure"].digest
    assert [call["seed"] for call in practice.calls] == [101, 101, 202, 303, 404]


def test_held_out_failure_is_a_failure_too(tmp_path) -> None:
    writer = WriterSpy()
    practice = _practice(fail=lambda attempt, seed: attempt == 0 and seed == 404)
    report = _run(tmp_path, write_fn=writer, practice_fn=practice)

    assert report.status == "admitted"
    assert len(report.attempts) == 2
    assert [call["seed"] for call in practice.calls[:5]] == [101, 202, 303, 404, 101]


def test_repair_cap_is_respected(tmp_path) -> None:
    writer = WriterSpy()
    check = CheckSpy([_failed_report()] * 10)
    report = _run(tmp_path, write_fn=writer, check_fn=check, max_repairs=2)

    assert report.status == "rejected"
    assert len(writer.calls) == 3  # initial attempt plus two repairs
    assert len(report.attempts) == 3
    assert report.repairs_used == 2
    assert report.reason == "rejected after 3 attempt(s)"
    assert not (tmp_path / "active" / f"{SKILL}.skill").exists()


def test_repair_after_two_failures_escalates_the_model(tmp_path) -> None:
    writer = WriterSpy()
    check = CheckSpy([_failed_report()] * 10)
    _run(tmp_path, write_fn=writer, check_fn=check, max_repairs=2)

    assert [call["model"] for call in writer.calls] == [
        "gpt-6-luna",
        "gpt-6-luna",
        "gpt-6.1-terra",
    ]


def test_no_escalation_for_a_model_that_is_not_luna(tmp_path) -> None:
    writer = WriterSpy()
    check = CheckSpy([_failed_report()] * 10)
    _run(
        tmp_path,
        writer_model="gpt-6.1-terra",
        write_fn=writer,
        check_fn=check,
        max_repairs=2,
    )
    assert [call["model"] for call in writer.calls] == ["gpt-6.1-terra"] * 3


def test_body_failure_uses_a_spare_seed_without_a_repair(tmp_path) -> None:
    writer = WriterSpy()
    practice = _practice(
        fail=lambda attempt, seed: "body" if attempt == 0 and seed == 101 else False
    )
    report = _run(tmp_path, write_fn=writer, practice_fn=practice)

    assert report.status == "admitted"
    assert report.repairs_used == 0
    assert len(writer.calls) == 1
    assert [call["seed"] for call in practice.calls] == [101, 505, 202, 303, 404]
    assert report.body_failures == 1
    assert report.attempts[0].practices[0].failure_class == "body"

    lines = (tmp_path / "body_failures.jsonl").read_text(encoding="utf-8").splitlines()
    assert len(lines) == 1
    entry = json.loads(lines[0])
    assert entry["seed"] == 101
    assert entry["run_dir"] == "/runs/seed-101"
    assert entry["failure_kind"] == "no_block"
    assert "failure kind: no_block" in entry["digest"]


def test_three_body_failures_end_inconclusive_body(tmp_path) -> None:
    writer = WriterSpy()
    practice = _practice(fail=lambda attempt, seed: "body" if attempt == 0 else False)
    report = _run(tmp_path, write_fn=writer, practice_fn=practice)

    assert report.status == "inconclusive_body"
    assert report.body_failures == 3
    assert report.repairs_used == 0
    assert len(writer.calls) == 1
    assert [call["seed"] for call in practice.calls] == [101, 505, 606]
    assert not (tmp_path / "active" / f"{SKILL}.skill").exists()
    saved = json.loads(Path(report.report_path).read_text(encoding="utf-8"))
    assert saved["status"] == "inconclusive_body"
    assert saved["body_failures"] == 3


def test_body_failure_on_held_out_stays_inconclusive(tmp_path) -> None:
    writer = WriterSpy()
    practice = _practice(
        fail=lambda attempt, seed: "body" if attempt == 0 and seed == 404 else False
    )
    report = _run(tmp_path, write_fn=writer, practice_fn=practice)

    assert report.status == "inconclusive_body"
    assert report.body_failures == 1
    assert not (tmp_path / "active" / f"{SKILL}.skill").exists()


def test_mixed_body_and_skill_failures(tmp_path) -> None:
    writer = WriterSpy()

    def fail(attempt, seed):
        if attempt == 0 and seed == 101:
            return "body"
        if attempt == 0 and seed == 202:
            return "skill"
        return False

    practice = _practice(fail=fail)
    report = _run(tmp_path, write_fn=writer, practice_fn=practice)

    assert report.status == "admitted"
    assert report.repairs_used == 1
    assert report.body_failures == 1
    assert [call["seed"] for call in practice.calls] == [
        101,
        505,
        202,
        101,
        202,
        303,
        404,
    ]
    assert writer.calls[1]["failure"].kind == "no_rule"
    lines = (tmp_path / "body_failures.jsonl").read_text(encoding="utf-8").splitlines()
    assert len(lines) == 1


# --------------------------------------------------------------------------- #
# Cancel: a timed-out or stopped admission never copies its skill active
# --------------------------------------------------------------------------- #
def _passing(seed: int) -> PracticeResult:
    return PracticeResult(
        passed=True,
        outcome="done",
        ticks=10,
        run_dir=f"/runs/seed-{seed}",
        digest="ok",
        seed=seed,
    )


def _events(tmp_path, kind: str) -> list[dict]:
    """Every evidence line of ``kind``, oldest first."""
    path = tmp_path / "evidence" / "brain.jsonl"
    return [
        entry
        for entry in (
            json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()
        )
        if entry["kind"] == kind
    ]


def _cancelling_practice(cancel: CancelToken, *, seeds: tuple[int, ...]):
    """A practice room that cancels the admission on one of ``seeds``."""
    calls: list[int] = []

    def run(*, skill, goal, candidate_dir, seed, max_ticks, cancel=None):
        calls.append(seed)
        if seed in seeds:
            cancel.set()
        return _passing(seed)

    run.calls = calls  # type: ignore[attr-defined]
    return run


def test_a_cancel_between_practice_runs_never_copies_the_skill_active(tmp_path) -> None:
    cancel = CancelToken()
    practice = _cancelling_practice(cancel, seeds=(101,))

    with pytest.raises(AdmissionCancelled):
        _run(tmp_path, practice_fn=practice, cancel=cancel)

    assert practice.calls == [101]  # stopped before the next seed's server
    assert not (tmp_path / "active" / f"{SKILL}.skill").exists()
    stopped = _events(tmp_path, "admission_cancelled")
    assert [(entry["stage"], entry["attempt"]) for entry in stopped] == [("practice", 0)]


def test_a_cancel_during_the_held_out_run_never_copies_the_skill_active(tmp_path) -> None:
    cancel = CancelToken()
    practice = _cancelling_practice(cancel, seeds=(404,))

    with pytest.raises(AdmissionCancelled):
        _run(tmp_path, practice_fn=practice, cancel=cancel)

    assert practice.calls == [101, 202, 303, 404]  # every run passed, still no copy
    assert not (tmp_path / "active" / f"{SKILL}.skill").exists()
    # The last boundary is the copy: the cancel landed after the final run.
    stopped = _events(tmp_path, "admission_cancelled")
    assert [(entry["stage"], entry["attempt"]) for entry in stopped] == [("active", 0)]
    # No report either: nothing may claim this attempt was admitted.
    assert not list((tmp_path / "evidence").glob("admission-*.json"))
    # The run got as far as writing the candidate: only the active copy is the
    # line this test guards.
    assert (tmp_path / "candidates" / f"{SKILL}.skill").is_file()


def test_a_cancel_before_the_first_attempt_spends_nothing(tmp_path) -> None:
    cancel = CancelToken()
    cancel.set()
    writer = WriterSpy()

    with pytest.raises(AdmissionCancelled):
        _run(tmp_path, write_fn=writer, cancel=cancel)

    assert writer.calls == []
    assert not (tmp_path / "active" / f"{SKILL}.skill").exists()

def test_the_writer_model_escalates_after_two_attempts() -> None:
    assert write_model("gpt-6-luna", 0) == "gpt-6-luna"
    assert write_model("gpt-6-luna", 1) == "gpt-6-luna"
    assert write_model("gpt-6-luna", WRITE_ESCALATE_AFTER) == "gpt-6.1-terra"
    # A writer that is already stronger does not "escalate" to terra.
    assert write_model("gpt-6.1-sol", 2) == "gpt-6.1-sol"
    assert write_model("gpt-6-luna", 2, None) == "gpt-6-luna"


def test_the_writer_estimate_uses_the_model_the_attempt_really_uses() -> None:
    assert estimate("gpt-6-luna") == pytest.approx(
        estimate_cost("gpt-6-luna", WRITE_INPUT_ESTIMATE, WRITE_OUTPUT_ESTIMATE)
    )
    assert estimate("gpt-6-luna", attempt=2) == pytest.approx(
        estimate_cost("gpt-6.1-terra", WRITE_INPUT_ESTIMATE, WRITE_OUTPUT_ESTIMATE)
    )


def test_a_practice_run_that_sees_the_cancel_first_records_where_it_stopped(
    tmp_path,
) -> None:
    cancel = CancelToken()

    def killed(*, skill, goal, candidate_dir, seed, max_ticks, cancel):
        cancel.set()
        raise AdmissionCancelled("the practice stack was killed")

    with pytest.raises(AdmissionCancelled):
        _run(tmp_path, practice_fn=killed, cancel=cancel)

    assert not (tmp_path / "active" / f"{SKILL}.skill").exists()
    stopped = _events(tmp_path, "admission_cancelled")
    assert [(entry["stage"], entry["attempt"]) for entry in stopped] == [("practice", 0)]

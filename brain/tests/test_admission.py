from __future__ import annotations

import json
from pathlib import Path

from praxion_brain.admission import admit
from praxion_brain.evidence import Evidence
from praxion_brain.goal import Goal
from praxion_brain.ledger import Ledger
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
    calls: list[dict] = []

    def run(*, skill, goal, candidate_dir, seed, max_ticks):
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
        passed = not fail(attempt, seed)
        return PracticeResult(
            passed=passed,
            outcome="done" if passed else "failed(no_rule)",
            ticks=100,
            receipts={"mine": 1} if passed else {},
            deaths=0,
            run_dir=f"/runs/seed-{seed}",
            digest="failure kind: none" if passed else "failure kind: no_rule\nrule_selected get_furnace rule=3",
            seed=seed,
            failure_kind=None if passed else "no_rule",
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

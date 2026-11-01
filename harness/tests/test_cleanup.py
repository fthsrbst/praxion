"""Unit tests for stale run-directory cleanup."""

import os
import time

import praxion_harness.cleanup as cleanup
from praxion_harness.server import keep_runs_by_default


def test_keep_runs_env(monkeypatch):
    monkeypatch.setenv("PRAXION_KEEP_RUNS", "1")
    assert keep_runs_by_default() is True
    monkeypatch.setenv("PRAXION_KEEP_RUNS", "0")
    assert keep_runs_by_default() is False


def _make_run(root, name, *, age_hours=0.0):
    run = root / name
    (run / "server" / "world").mkdir(parents=True)
    stamp = time.time() - age_hours * 3600
    os.utime(run, (stamp, stamp))
    return run


def test_run_in_use_needs_a_session_lock(tmp_path):
    run = _make_run(tmp_path, "test-a")
    assert cleanup.run_in_use(run) is False
    lock = cleanup.session_lock_path(run)
    lock.write_bytes(b"\x00\x00\x00")
    assert cleanup.run_in_use(run) is False


def test_clean_runs_only_touches_harness_prefixes(monkeypatch, tmp_path):
    runs = tmp_path / "runs"
    runs.mkdir()
    doomed = [_make_run(runs, n) for n in ("test-1", "measure-1", "explore-1")]
    kept = [_make_run(runs, n) for n in ("fwd-live", "raw-seq", "cli-123")]

    monkeypatch.setattr(cleanup, "repo_root", lambda: tmp_path)
    deleted, skipped = cleanup.clean_runs()

    assert sorted(p.name for p in deleted) == ["explore-1", "measure-1", "test-1"]
    assert skipped == []
    assert all(not p.exists() for p in doomed)
    assert all(p.exists() for p in kept)


def test_clean_runs_honours_age(monkeypatch, tmp_path):
    runs = tmp_path / "runs"
    runs.mkdir()
    old = _make_run(runs, "test-old", age_hours=3)
    new = _make_run(runs, "test-new", age_hours=0)

    monkeypatch.setattr(cleanup, "repo_root", lambda: tmp_path)
    deleted, skipped = cleanup.clean_runs(older_than_hours=1)

    assert deleted == [old]
    assert not old.exists() and new.exists()
    assert skipped == []


def test_clean_runs_dry_run_keeps_everything(monkeypatch, tmp_path):
    runs = tmp_path / "runs"
    runs.mkdir()
    run = _make_run(runs, "test-1")

    monkeypatch.setattr(cleanup, "repo_root", lambda: tmp_path)
    deleted, skipped = cleanup.clean_runs(dry_run=True)

    assert deleted == [run]
    assert skipped == []
    assert run.exists()

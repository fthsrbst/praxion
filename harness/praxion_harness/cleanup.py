"""Stale run-directory cleanup for ``python -m praxion_harness clean-runs``.

Only harness-generated prefixes are touched (``test-``, ``measure-``, ``explore-``).
A directory whose ``server/world/session.lock`` is locked by a running server is
skipped; other workers may own their own run directories.
"""

from __future__ import annotations

import os
import shutil
import time
from pathlib import Path
from typing import List, Optional, Tuple

from .paths import repo_root

RUN_PREFIXES = ("test-", "measure-", "explore-")
SESSION_LOCK = Path("server") / "world" / "session.lock"


def runs_dir() -> Path:
    return repo_root() / "runs"


def session_lock_path(run_dir: Path) -> Path:
    return Path(run_dir) / SESSION_LOCK


def _file_is_unlocked(path: Path) -> bool:
    """True when this process can take an exclusive lock on the file."""
    try:
        with path.open("r+b") as fh:
            if os.name == "nt":
                import msvcrt

                fh.seek(0)
                try:
                    msvcrt.locking(fh.fileno(), msvcrt.LK_NBLCK, 1)
                except OSError:
                    return False
                try:
                    fh.seek(0)
                    msvcrt.locking(fh.fileno(), msvcrt.LK_UNLCK, 1)
                except OSError:
                    pass
                return True
            import fcntl

            try:
                fcntl.flock(fh.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
            except OSError:
                return False
            fcntl.flock(fh.fileno(), fcntl.LOCK_UN)
            return True
    except OSError:
        # Cannot even open it: assume a live server holds it.
        return False


def run_in_use(run_dir: Path) -> bool:
    """True when a server is still holding this run's world session lock."""
    lock = session_lock_path(run_dir)
    if not lock.is_file():
        return False
    return not _file_is_unlocked(lock)


def is_harness_run(run_dir: Path) -> bool:
    return Path(run_dir).is_dir() and Path(run_dir).name.startswith(RUN_PREFIXES)


def clean_runs(
    *,
    older_than_hours: Optional[float] = None,
    dry_run: bool = False,
    now: Optional[float] = None,
) -> Tuple[List[Path], List[Path]]:
    """Delete stale harness run directories.

    Returns ``(deleted, skipped)``. ``skipped`` holds directories that are in use
    or could not be removed. With ``older_than_hours`` only directories whose
    modification time is older than that are considered.
    """
    current = time.time() if now is None else now
    deleted: List[Path] = []
    skipped: List[Path] = []
    root = runs_dir()
    if not root.is_dir():
        return deleted, skipped
    for run_dir in sorted(root.iterdir()):
        if not is_harness_run(run_dir):
            continue
        if older_than_hours is not None:
            age_hours = (current - run_dir.stat().st_mtime) / 3600.0
            if age_hours < older_than_hours:
                continue
        if run_in_use(run_dir):
            skipped.append(run_dir)
            continue
        if dry_run:
            deleted.append(run_dir)
            continue
        try:
            shutil.rmtree(run_dir)
        except OSError:
            skipped.append(run_dir)
        else:
            deleted.append(run_dir)
    return deleted, skipped


__all__ = [
    "RUN_PREFIXES",
    "clean_runs",
    "is_harness_run",
    "run_in_use",
    "runs_dir",
    "session_lock_path",
]

from __future__ import annotations

from praxion_brain.records import settled_status


def test_settled_status_is_the_four_way_rule() -> None:
    # A7: only a body-verified condition makes a goal a success.
    assert settled_status("done", "true", False) == ("done", None)
    assert settled_status("done", "false", False) == ("failed", "skill_wrong")
    assert settled_status("done", "unknown", False) == ("unverified", None)
    assert settled_status("done", None, False) == ("unverified", None)
    # A goal known to be unverifiable is not punished for an unknown condition.
    assert settled_status("done", "unknown", True) == ("done", None)
    # A failed play keeps the body's typed failure kind.
    assert settled_status("failed", None, False, "not_found") == ("failed", "not_found")
    assert settled_status("budget", None, False) == ("failed", "budget")

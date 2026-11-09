"""Guard the raw-store / perception boundary in the body crates.

Decision-making code (primitives, facts, reflexes, interpreter, skills) must get world facts only
through praxion_perception; a raw-store reader call (World::block_state, surface_y, entity,
entities, block_description) outside the allowlist is a violation (architecture §5.1, decision
2026-11-09). harness/tests/perception_boundary_baseline.json may only shrink; regenerate it with
``python harness/tests/test_perception_boundary.py --write-baseline``.

The scan is textual, not a Rust parser: everything from a line's first ``//`` is dropped (so
``/* */`` block comments count and a ``//`` in a string can hide a call); a skipped
``#[cfg(test)] mod name { ... }`` region ends at brace depth zero (braces in strings and comments
count too, and ``mod tests;`` skips nothing here — its file is excluded by name); test-only files
(``test_world.rs``, ``tests.rs``, anything under ``tests/``) are never scanned.
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path, PurePosixPath

REPO_ROOT = Path(__file__).resolve().parents[2]
BASELINE = Path(__file__).resolve().parent / "perception_boundary_baseline.json"
CALL_RE = re.compile(r"\.(block_state|surface_y|entity|entities|block_description)\(")
# headless actuator, receipts/driver, and the store/producers/perception layer: architecture §5.1.
ALLOWED_CRATES = frozenset({"decoder", "headless", "perception", "receipts", "world"})
# The planner's DirectBlocks adapter is a store reader by design: it feeds the planner snapshot.
ALLOWED_FILES = frozenset({"body/crates/body/src/driver.rs", "body/crates/planner/src/world.rs"})
TEST_ONLY_NAMES = frozenset({"test_world.rs", "tests.rs"})


def is_scanned(rel: str) -> bool:
    """Whether a repository-relative path is subject to the boundary rule."""
    path = PurePosixPath(rel)
    if rel in ALLOWED_FILES or path.name in TEST_ONLY_NAMES or "tests" in path.parts:
        return False
    if len(path.parts) >= 4 and path.parts[0:2] == ("body", "crates"):
        return path.parts[2] not in ALLOWED_CRATES
    return True


def cfg_test_ranges(lines: list[str]) -> list[tuple[int, int]]:
    """Line ranges (inclusive) of inline ``#[cfg(test)] mod ... { ... }`` blocks."""
    ranges = []
    i = 0
    while i < len(lines):
        if "#[cfg(test)]" not in lines[i]:
            i += 1
            continue
        j = i
        if not re.search(r"\bmod\b", lines[j]):
            j += 1
            while j < len(lines) and (not lines[j].strip() or lines[j].lstrip().startswith("#")):
                j += 1
        if j < len(lines) and re.search(r"\bmod\s+\w+\s*\{", lines[j]):
            depth, k = 0, j
            while k < len(lines):
                depth += lines[k].count("{") - lines[k].count("}")
                if depth <= 0:
                    break
                k += 1
            ranges.append((j, min(k, len(lines) - 1)))
            i = k + 1
        else:
            i += 1
    return ranges


def count_store_reads(text: str) -> dict[str, int]:
    """Count raw-store calls per method in one file, ignoring comments and test modules."""
    lines = [line.split("//", 1)[0] for line in text.splitlines()]
    skipped = cfg_test_ranges(lines)
    counts: dict[str, int] = {}
    for index, line in enumerate(lines):
        if any(start <= index <= end for start, end in skipped):
            continue
        for match in CALL_RE.finditer(line):
            method = match.group(1)
            counts[method] = counts.get(method, 0) + 1
    return counts


def collect_counts(root: Path = REPO_ROOT) -> dict[str, dict[str, int]]:
    """Scan ``body/crates/*/src/**/*.rs`` and return ``{path: {method: count}}``."""
    counts: dict[str, dict[str, int]] = {}
    for path in sorted((root / "body" / "crates").glob("*/src/**/*.rs")):
        rel = path.relative_to(root).as_posix()
        found = count_store_reads(path.read_text(encoding="utf-8")) if is_scanned(rel) else {}
        if found:
            counts[rel] = found
    return counts


def baseline_json(counts: dict[str, dict[str, int]]) -> str:
    return json.dumps(counts, indent=2, sort_keys=True) + "\n"


def test_no_new_store_reads():
    """A new direct store read outside the allowlist fails the test."""
    current = collect_counts()
    baseline = json.loads(BASELINE.read_text(encoding="utf-8"))
    problems = []
    for rel in sorted(current):
        for method in sorted(current[rel]):
            count = current[rel][method]
            allowed = baseline.get(rel, {}).get(method, 0)
            if count > allowed:
                problems.append(
                    f"{rel}: {method} is called {count} time(s); the baseline "
                    f"harness/tests/perception_boundary_baseline.json allows {allowed}.\n"
                    "A raw-store read outside the allowlist breaks the perception boundary "
                    "(architecture §5.1, decision 2026-11-09); route it through praxion_perception."
                )
    assert not problems, "\n\n".join(problems)


def test_baseline_is_not_stale():
    """The baseline must match the code: a count below it fails until it is lowered."""
    current = collect_counts()
    baseline = json.loads(BASELINE.read_text(encoding="utf-8"))
    problems = [
        f"{rel}: {method} is called {current.get(rel, {}).get(method, 0)} time(s); "
        f"the baseline says {allowed}."
        for rel in sorted(baseline)
        for method in sorted(baseline[rel])
        if (allowed := baseline[rel][method]) > current.get(rel, {}).get(method, 0)
    ]
    assert not problems, (
        "\n\n".join(problems)
        + "\n\nThe baseline may only shrink: lower it to match the code.\n"
        + "Exact baseline to write:\n"
        + baseline_json(current)
    )


def test_scanner_small_cases():
    cfg = "fn f() {}\n#[cfg(test)]\nmod tests {\n    fn t(w: &World) { w.block_state(p); }\n}\n"
    assert count_store_reads(cfg) == {}
    comments = "// w.block_state(p)\n/// w.surface_y(x, z)\nlet x = w.entity(1);\n"
    assert count_store_reads(comments) == {"entity": 1}
    assert count_store_reads("ctx.world.block_state(pos)\n") == {"block_state": 1}
    allowed = (
        "body/crates/headless/src/physics.rs",
        "body/crates/body/src/driver.rs",
        "body/crates/planner/src/world.rs",
        "body/crates/world/src/tests.rs",
    )
    assert not any(is_scanned(rel) for rel in allowed)
    assert is_scanned("body/crates/primitives/src/mine.rs")


def main() -> None:
    if "--write-baseline" in sys.argv[1:]:
        BASELINE.write_text(baseline_json(collect_counts()), encoding="utf-8")
        print(f"wrote {BASELINE}")
        return
    raise SystemExit("usage: python tests/test_perception_boundary.py --write-baseline")


if __name__ == "__main__":
    main()

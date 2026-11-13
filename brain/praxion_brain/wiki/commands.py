"""The ``praxion_brain wiki`` subcommands: fetch, build, search, eval."""

from __future__ import annotations

import json
from pathlib import Path

from . import extract, fetch, index, paths

#: The retrieval test used by ``eval``.
EVAL_K = 3


def fetch_pages(limit: int | None = None) -> int:
    """Download the fixed page list into ``local/wiki/raw/``."""
    titles = fetch.read_pages()
    if limit is not None:
        titles = titles[: max(0, limit)]
    print(f"wiki fetch: {len(titles)} titles from {paths.pages_path()}", flush=True)
    fetcher = fetch.Fetcher(paths.raw_dir())
    report = fetcher.fetch_all(titles)
    target = fetch.write_report(report)
    print(
        f"wiki fetch: {len(report.fetched)} new, {len(report.reused)} cached, "
        f"{len(report.missing)} missing, {len(report.failed)} failed",
        flush=True,
    )
    for result in report.missing:
        print(f"  missing: {result.title} ({result.url})")
    for result in report.failed:
        print(f"  failed: {result.title}: {result.note}")
    print(f"wiki fetch: report {target}", flush=True)
    if report.stopped:
        print(f"wiki fetch: stopped: {report.stopped}", flush=True)
        return 2
    return 0


def build_index() -> int:
    """Extract the raw HTML and build the local vector index."""
    articles, missing, empty = extract.extract_all()
    print(
        f"wiki build: extracted {len(articles)} pages to {paths.text_dir()}"
        + (f" ({len(empty)} empty: {', '.join(sorted(empty)[:10])})" if empty else ""),
        flush=True,
    )
    if missing:
        print(
            f"wiki build: {len(missing)} titles have no downloaded HTML: "
            f"{', '.join(missing[:10])}{' ...' if len(missing) > 10 else ''}"
        )
    if not articles:
        print("wiki build: nothing extracted; run `wiki fetch` first")
        return 2

    def progress(done: int, total: int) -> None:
        print(f"\rwiki build: embedded {done}/{total} chunks", end="", flush=True)

    store = index.build(progress=progress)
    print()
    print(
        f"wiki build: {len(store.chunks)} chunks, {store.dimension} dimensions, "
        f"model {store.model}"
    )
    print(f"wiki build: {paths.index_path()} and {paths.vectors_path()}")
    return 0


def search_query(query: str, k: int = 3) -> int:
    """Print the best passages for one query."""
    from . import search

    if not query.strip():
        print("wiki search: give a query, for example `wiki search \"how do I make a torch\"`")
        return 2
    hits = search(query.strip(), k=k)
    if not hits:
        print(
            f"wiki search: no results (index under {paths.wiki_dir()} missing or "
            f"unreadable; run `wiki fetch` and `wiki build`)"
        )
        return 1
    for rank, hit in enumerate(hits, start=1):
        body = hit.text.split("\n\n", 1)[-1]
        print(f"{rank}. [{hit.score:.3f}] {hit.title} - {hit.url}")
        print("   " + body[:400].replace("\n", " "))
    return 0


def _match(actual: str, expected: str) -> bool:
    return actual.strip().casefold() == expected.strip().casefold()


def evaluate(questions: Path | None = None) -> int:
    """Run the retrieval test and print hit@1 and hit@3."""
    from . import search

    path = Path(questions) if questions is not None else paths.questions_path()
    items = json.loads(path.read_text(encoding="utf-8"))
    if not items:
        print(f"wiki eval: no questions in {path}")
        return 2
    hit1 = 0
    hit3 = 0
    for item in items:
        question = str(item["question"])
        expected = str(item["title"])
        results = search(question, k=EVAL_K)
        titles = [result.title for result in results]
        first = bool(titles) and _match(titles[0], expected)
        found = any(_match(title, expected) for title in titles)
        hit1 += int(first)
        hit3 += int(found)
        top = titles[0] if titles else "-"
        print(f"{'HIT ' if first else '    '} {'ok' if found else 'MISS'}  {question!r} -> {expected!r} (top: {top!r})")
    total = len(items)
    print(f"wiki eval: {total} questions")
    print(f"wiki eval: hit@1 {hit1}/{total} = {hit1 / total:.2f}")
    print(f"wiki eval: hit@{EVAL_K} {hit3}/{total} = {hit3 / total:.2f}")
    return 0


def run(action: str, query: str = "", *, limit: int | None = None) -> int:
    """Dispatch one ``wiki`` subcommand."""
    if action == "fetch":
        return fetch_pages(limit)
    if action == "build":
        return build_index()
    if action == "search":
        return search_query(query)
    if action == "eval":
        return evaluate()
    print(f"wiki: unknown action {action!r} (fetch, build, search, eval)")
    return 2


__all__ = ["build_index", "evaluate", "fetch_pages", "run", "search_query"]

"""The wiki RAG without network and without a model download.

Covered here: the HTML extractor on a hand-written fixture, the section
chunker, ``search`` over a tiny fake index with a fake embedder, the
missing-index path, the model-mismatch path and the ``robots.txt`` refusal.
"""

from __future__ import annotations

import logging
import zlib
from pathlib import Path

import numpy as np
import pytest

import praxion_brain.wiki as wiki
from praxion_brain.wiki import extract, index
from praxion_brain.wiki.fetch import Fetcher, RobotsDenied, page_url, read_pages, slug

FIXTURE = Path(__file__).parent / "fixtures" / "wiki_article.html"

ROBOTS = """\
User-agent: *
Disallow: /w/Special:
Disallow: /w/*?action=
Disallow: /w/User:
"""


@pytest.fixture(autouse=True)
def _fresh_state():
    wiki._reset_for_tests()
    yield
    wiki._reset_for_tests()


class FakeEmbedder:
    """A deterministic bag-of-words embedder; no model, no download."""

    def __init__(self, model: str = "fake-embedder") -> None:
        self.model = model

    @staticmethod
    def _vector(text: str) -> np.ndarray:
        vector = np.zeros(64, dtype=np.float32)
        for word in text.lower().split():
            vector[zlib.crc32(word.encode()) % 64] += 1.0
        return index.normalize_rows(vector)[0]

    def embed_documents(self, texts):
        return np.vstack([self._vector(text) for text in texts])

    def embed_query(self, text: str) -> np.ndarray:
        return self._vector(text)


# --------------------------------------------------------------------------- #
# Extractor
# --------------------------------------------------------------------------- #
def test_extractor_keeps_headings_and_drops_page_furniture() -> None:
    article = extract.extract(
        FIXTURE.read_text(encoding="utf-8"), "https://minecraft.wiki/w/Test_Ore"
    )

    assert article.title == "Test Ore"
    assert article.url == "https://minecraft.wiki/w/Test_Ore"
    headings = [section.heading for section in article.sections]
    assert headings == ["Introduction", "Spawning", "Spawning > On death", "Usage"]

    text = "\n".join(section.text for section in article.sections)
    assert "raw test" in text
    assert "levels 16 and 96" in text
    assert "smelted into a test ingot" in text
    # Dropped on purpose.
    assert "Infobox noise" not in text
    assert "must be dropped" not in text
    assert "Loaded page noise" not in text
    assert "chestcontents" not in text
    assert "A reference that must be dropped" not in text
    assert "disambiguation" not in text
    assert "[1]" not in text
    assert "edit" not in text
    # Table text survives as cell paragraphs.
    assert "Test Ingot" in text


def test_extractor_tolerates_a_page_without_content() -> None:
    article = extract.extract("<html><head><title>Empty - MC</title></head></html>", "https://x/y")
    assert article.sections == []
    assert article.title == "Empty"


def test_extract_all_keeps_one_copy_of_each_article(tmp_path) -> None:
    """List titles that redirect to one article (e.g. Hunger -> Food) index once."""
    html = FIXTURE.read_text(encoding="utf-8").replace("Test Ore", "Food").replace(
        '<meta charset="utf-8">',
        '<meta charset="utf-8"><link rel="canonical" href="https://minecraft.wiki/w/Food">',
    )
    raw = tmp_path / "raw"
    raw.mkdir()
    (raw / "Food.html").write_text(html, encoding="utf-8")
    (raw / "Hunger.html").write_text(html, encoding="utf-8")

    articles, missing, empty = extract.extract_all(raw, tmp_path / "text", ["Food", "Hunger"])

    assert [article.title for article in articles] == ["Food"]
    assert missing == [] and empty == []
    assert sorted(path.name for path in (tmp_path / "text").glob("*.json")) == ["Food.json"]


def test_extract_all_mirrors_the_page_list(tmp_path) -> None:
    raw = tmp_path / "raw"
    raw.mkdir()
    (raw / "Test_Ore.html").write_text(FIXTURE.read_text(encoding="utf-8"), encoding="utf-8")
    text = tmp_path / "text"
    text.mkdir()
    (text / "Old_Page.json").write_text("{}", encoding="utf-8")

    articles, missing, empty = extract.extract_all(raw, text, ["Test Ore", "Ghost Page"])

    assert [article.title for article in articles] == ["Test Ore"]
    assert missing == ["Ghost Page"]
    assert empty == []
    # A page that left the list leaves no text behind.
    assert sorted(path.name for path in text.glob("*.json")) == ["Test_Ore.json"]


# --------------------------------------------------------------------------- #
# Chunker
# --------------------------------------------------------------------------- #
def _paragraph(words: int, marker: str) -> str:
    return " ".join([marker] * words)


def test_chunks_respect_the_word_budget_and_paragraph_bounds() -> None:
    text = "\n\n".join(
        [_paragraph(80, "alpha"), _paragraph(90, "beta"), _paragraph(100, "gamma"), _paragraph(120, "delta")]
    )
    chunks = index.chunk_section(text)
    assert len(chunks) >= 2
    for chunk in chunks:
        assert len(chunk.split()) <= index.TARGET_MAX_WORDS
    # Paragraph boundaries are kept: no chunk mixes the start of two paragraphs.
    assert chunks[0].startswith("alpha")
    assert all(set(chunk.split()) <= {"alpha", "beta", "gamma", "delta"} for chunk in chunks)


def test_a_single_huge_paragraph_is_split() -> None:
    text = _paragraph(700, "word") + "."
    chunks = index.chunk_section(text)
    assert len(chunks) >= 3
    assert all(len(chunk.split()) <= index.TARGET_MAX_WORDS for chunk in chunks)


def test_every_chunk_is_prefixed_with_title_and_heading() -> None:
    document = {
        "title": "Test Ore",
        "url": "https://minecraft.wiki/w/Test_Ore",
        "sections": [
            {"heading": "Spawning", "text": _paragraph(200, "ore")},
            {"heading": "Usage", "text": ""},
        ],
    }
    chunks = index.chunk_article(document)
    assert len(chunks) == 1
    assert chunks[0]["text"].startswith("Test Ore > Spawning\n\n")
    assert chunks[0]["heading"] == "Spawning"
    assert chunks[0]["url"] == document["url"]


# --------------------------------------------------------------------------- #
# search
# --------------------------------------------------------------------------- #
def _write_fake_index(directory: Path, model: str = "fake-embedder") -> None:
    documents = [
        {
            "title": "Apple",
            "url": "https://minecraft.wiki/w/Apple",
            "sections": [{"heading": "Introduction", "text": "An apple is a food item."}],
        },
        {
            "title": "Creeper",
            "url": "https://minecraft.wiki/w/Creeper",
            "sections": [{"heading": "Behavior", "text": "A creeper explodes near a player."}],
        },
    ]
    index.build(
        documents=documents,
        index_path=directory / "index.sqlite",
        vectors_path=directory / "vectors.npy",
        embedder=FakeEmbedder(model),
    )


def test_search_returns_snippets_from_a_tiny_index(tmp_path, monkeypatch) -> None:
    _write_fake_index(tmp_path)
    monkeypatch.setattr("praxion_brain.wiki.paths.index_path", lambda: tmp_path / "index.sqlite")
    monkeypatch.setattr("praxion_brain.wiki.paths.vectors_path", lambda: tmp_path / "vectors.npy")
    monkeypatch.setattr(wiki, "_EMBEDDER_FACTORY", lambda model: FakeEmbedder(model))

    hits = wiki.search("apple", k=2)

    assert len(hits) == 2
    assert isinstance(hits[0], wiki.Snippet)
    assert hits[0].title == "Apple"
    assert hits[0].url == "https://minecraft.wiki/w/Apple"
    assert hits[0].text.startswith("Apple > Introduction")
    assert hits[0].score > hits[1].score
    assert wiki.search("apple", k=0) == []
    assert wiki.search("   ") == []


def test_search_without_an_index_returns_empty_and_warns_once(tmp_path, monkeypatch, caplog) -> None:
    monkeypatch.setattr("praxion_brain.wiki.paths.index_path", lambda: tmp_path / "index.sqlite")
    monkeypatch.setattr("praxion_brain.wiki.paths.vectors_path", lambda: tmp_path / "vectors.npy")

    def explode(model):  # the embedder must not even be built
        raise AssertionError("embedder built without an index")

    monkeypatch.setattr(wiki, "_EMBEDDER_FACTORY", explode)

    with caplog.at_level(logging.WARNING, logger="praxion_brain.wiki"):
        assert wiki.search("creeper") == []
        assert wiki.search("creeper") == []

    warnings = [record for record in caplog.records if "wiki index missing" in record.message]
    assert len(warnings) == 1


def test_search_returns_empty_when_the_embedder_cannot_load(tmp_path, monkeypatch, caplog) -> None:
    _write_fake_index(tmp_path)
    monkeypatch.setattr("praxion_brain.wiki.paths.index_path", lambda: tmp_path / "index.sqlite")
    monkeypatch.setattr("praxion_brain.wiki.paths.vectors_path", lambda: tmp_path / "vectors.npy")

    def unavailable(model):
        raise index.EmbedderUnavailable("no model here")

    monkeypatch.setattr(wiki, "_EMBEDDER_FACTORY", unavailable)

    with caplog.at_level(logging.WARNING, logger="praxion_brain.wiki"):
        assert wiki.search("apple") == []
        assert wiki.search("apple") == []

    assert len([r for r in caplog.records if "embedder unavailable" in r.message]) == 1


def test_search_refuses_an_index_built_with_another_model(tmp_path, monkeypatch, caplog) -> None:
    _write_fake_index(tmp_path, model="model-a")
    monkeypatch.setattr("praxion_brain.wiki.paths.index_path", lambda: tmp_path / "index.sqlite")
    monkeypatch.setattr("praxion_brain.wiki.paths.vectors_path", lambda: tmp_path / "vectors.npy")
    monkeypatch.setattr(wiki, "_EMBEDDER_FACTORY", lambda model: FakeEmbedder("model-b"))

    with caplog.at_level(logging.WARNING, logger="praxion_brain.wiki"):
        assert wiki.search("apple") == []

    assert any("built with" in record.message for record in caplog.records)


def test_search_never_raises_on_a_broken_index(tmp_path, monkeypatch, caplog) -> None:
    (tmp_path / "index.sqlite").write_bytes(b"not a database")
    (tmp_path / "vectors.npy").write_bytes(b"not a matrix")
    monkeypatch.setattr("praxion_brain.wiki.paths.index_path", lambda: tmp_path / "index.sqlite")
    monkeypatch.setattr("praxion_brain.wiki.paths.vectors_path", lambda: tmp_path / "vectors.npy")

    with caplog.at_level(logging.WARNING, logger="praxion_brain.wiki"):
        assert wiki.search("apple") == []

    assert any("unreadable" in record.message for record in caplog.records)


# --------------------------------------------------------------------------- #
# Fetch rules
# --------------------------------------------------------------------------- #
def test_robots_txt_refusal_stops_the_request(tmp_path) -> None:
    def no_network(request, timeout):
        raise AssertionError(f"a request was sent to {request.full_url}")

    fetcher = Fetcher(tmp_path, robots_text=ROBOTS, opener=no_network)

    assert fetcher.allowed(page_url("Creeper")) is True
    with pytest.raises(RobotsDenied):
        fetcher.fetch("Special:Version")


def test_a_disallowed_query_url_is_never_built() -> None:
    # Page URLs stay plain articles: no query string, no API path.
    assert page_url("Iron Ore") == "https://minecraft.wiki/w/Iron_Ore"
    assert page_url("Tutorial:Mining/Diamonds") == (
        "https://minecraft.wiki/w/Tutorial:Mining/Diamonds"
    )
    # The saved file name carries no path separator or colon (Windows-safe).
    assert slug("Tutorial:Mining/Diamonds") == "Tutorial_Mining_Diamonds"


def test_robots_unavailable_fetches_nothing(tmp_path) -> None:
    def no_network(request, timeout):
        raise OSError("offline")

    fetcher = Fetcher(tmp_path, opener=no_network)
    report = fetcher.fetch_all(["Creeper", "Zombie"])

    assert report.stopped.startswith("cannot read")
    assert report.fetched == []
    assert not (tmp_path / "Creeper.html").exists()


def test_extractor_prefers_the_canonical_url() -> None:
    html = (
        '<html><head><link rel="canonical" href="https://minecraft.wiki/w/End_(biome)">'
        "<title>Short – MC</title></head><body></body></html>"
    )
    article = extract.extract(html, "https://minecraft.wiki/w/The_End_(biome)")
    assert article.url == "https://minecraft.wiki/w/End_(biome)"


def test_fetch_all_stops_on_rate_limit(tmp_path) -> None:
    class Response:
        status = 200
        url = "https://minecraft.wiki/w/X"

        def read(self):
            return b"<html></html>"

        def __enter__(self):
            return self

        def __exit__(self, *exc):
            return False

    calls: list[str] = []

    def opener(request, timeout):
        calls.append(request.full_url)
        if len(calls) == 1:
            import urllib.error

            raise urllib.error.HTTPError(request.full_url, 429, "Too Many Requests", {}, None)
        return Response()

    fetcher = Fetcher(tmp_path, robots_text=ROBOTS, opener=opener)
    report = fetcher.fetch_all(["Creeper", "Zombie"])

    assert report.stopped.startswith("HTTP 429")
    assert len(calls) == 1
    assert not (tmp_path / "Creeper.html").exists()


def test_pages_list_is_parsed_without_comments_or_duplicates(tmp_path) -> None:
    source = tmp_path / "pages.txt"
    source.write_text("# group\n\nCreeper\nZombie\nCreeper\n  Iron Ore  \n", encoding="utf-8")
    assert read_pages(source) == ["Creeper", "Zombie", "Iron Ore"]

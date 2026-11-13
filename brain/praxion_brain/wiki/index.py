"""Chunk the extracted articles, embed them and store the local index.

The index is two files under ``local/wiki/``:

- ``index.sqlite``: a ``chunks`` table (title, heading, url, text) and a
  ``meta`` table (embedding model, dimension, counts, build time, licence),
- ``vectors.npy``: one L2-normalised float32 row per chunk, in chunk-id order.

Embeddings are local and multilingual so a Turkish question can retrieve an
English passage. The first choice is a ``fastembed`` ONNX model; the model name
and dimension are stored in ``meta`` and ``search`` refuses an index built with
another model.
"""

from __future__ import annotations

import sqlite3
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Callable, Iterable, Protocol, Sequence

import numpy as np

from . import paths

#: A multilingual model under the 600 MB budget with 384 dimensions.
DEFAULT_MODEL = "sentence-transformers/paraphrase-multilingual-MiniLM-L12-v2"

#: Chunk shape: aim between these word counts, never exceed the maximum.
TARGET_MIN_WORDS = 150
TARGET_MAX_WORDS = 300

#: Models that need task prefixes; a model without a task uses empty strings.
TASK_PREFIXES = {
    "e5": ("query: ", "passage: "),
}

LICENCE = "CC BY-NC-SA 3.0 (minecraft.wiki); local index, never redistributed"


class EmbedderUnavailable(RuntimeError):
    """The embedding model could not be imported or loaded."""


class Embedder(Protocol):
    """The two calls the index needs from an embedding model."""

    model: str

    def embed_documents(self, texts: Sequence[str]) -> np.ndarray: ...

    def embed_query(self, text: str) -> np.ndarray: ...


def task_prefixes(model: str) -> tuple[str, str]:
    """``(query prefix, passage prefix)`` for a model, empty when it needs none."""
    lowered = model.lower()
    for marker, prefixes in TASK_PREFIXES.items():
        if marker in lowered:
            return prefixes
    return "", ""


def normalize_rows(matrix: np.ndarray) -> np.ndarray:
    """L2-normalise every row; cosine similarity becomes a dot product."""
    array = np.asarray(matrix, dtype=np.float32)
    if array.ndim == 1:
        array = array.reshape(1, -1)
    norms = np.linalg.norm(array, axis=1, keepdims=True)
    norms[norms == 0] = 1.0
    return array / norms


class FastEmbedder:
    """A local ONNX embedder through ``fastembed`` (the optional ``wiki`` extra)."""

    def __init__(self, model: str = DEFAULT_MODEL, cache_dir: Path | None = None) -> None:
        try:
            from fastembed import TextEmbedding
        except ImportError as error:  # pragma: no cover - depends on the install
            raise EmbedderUnavailable(
                "fastembed is not installed; run `pip install -e '.[wiki]'`"
            ) from error
        self.model = model
        directory = Path(cache_dir) if cache_dir is not None else paths.models_dir()
        directory.mkdir(parents=True, exist_ok=True)
        try:
            self._encoder = TextEmbedding(model_name=model, cache_dir=str(directory))
        except Exception as error:  # download or ONNX failure
            raise EmbedderUnavailable(f"cannot load {model}: {error}") from error
        self._query_prefix, self._passage_prefix = task_prefixes(model)

    def embed_documents(self, texts: Sequence[str]) -> np.ndarray:
        prefixed = [self._passage_prefix + text for text in texts]
        vectors = list(self._encoder.passage_embed(prefixed, batch_size=32))
        return normalize_rows(np.asarray(vectors, dtype=np.float32))

    def embed_query(self, text: str) -> np.ndarray:
        vectors = list(self._encoder.query_embed([self._query_prefix + text], batch_size=1))
        return normalize_rows(np.asarray(vectors, dtype=np.float32))[0]


# --------------------------------------------------------------------------- #
# Chunking
# --------------------------------------------------------------------------- #
def _words(text: str) -> int:
    return len(text.split())


def _split_long(paragraph: str) -> list[str]:
    """Split an over-long paragraph on sentence ends, then on words."""
    sentences = [part.strip() for part in paragraph.replace("? ", "?|").replace("! ", "!|").replace(". ", ".|").split("|")]
    pieces: list[str] = []
    current: list[str] = []
    count = 0
    for sentence in sentences:
        words = _words(sentence)
        if words > TARGET_MAX_WORDS:
            if current:
                pieces.append(" ".join(current))
                current, count = [], 0
            words_in_sentence = sentence.split()
            for start in range(0, len(words_in_sentence), TARGET_MAX_WORDS):
                pieces.append(" ".join(words_in_sentence[start : start + TARGET_MAX_WORDS]))
            continue
        if current and count + words > TARGET_MAX_WORDS:
            pieces.append(" ".join(current))
            current, count = [], 0
        current.append(sentence)
        count += words
    if current:
        pieces.append(" ".join(current))
    return [piece for piece in pieces if piece.strip()]


def chunk_section(text: str) -> list[str]:
    """Split one section's text into 150-300-word chunks on paragraph bounds."""
    paragraphs = [paragraph.strip() for paragraph in text.split("\n\n") if paragraph.strip()]
    units: list[str] = []
    for paragraph in paragraphs:
        if _words(paragraph) > TARGET_MAX_WORDS:
            units.extend(_split_long(paragraph))
        else:
            units.append(paragraph)
    chunks: list[str] = []
    current: list[str] = []
    count = 0
    for unit in units:
        words = _words(unit)
        if current and count + words > TARGET_MAX_WORDS:
            chunks.append("\n\n".join(current))
            current, count = [], 0
        current.append(unit)
        count += words
        if count >= TARGET_MAX_WORDS:
            chunks.append("\n\n".join(current))
            current, count = [], 0
    if current:
        chunks.append("\n\n".join(current))
    return chunks


def chunk_article(document: dict) -> list[dict[str, str]]:
    """Every chunk of one extracted article, prefixed ``Title > Heading``."""
    title = str(document.get("title", ""))
    url = str(document.get("url", ""))
    chunks: list[dict[str, str]] = []
    for section in document.get("sections", []):
        heading = str(section.get("heading", "")) or "Introduction"
        text = str(section.get("text", "")).strip()
        if not text:
            continue
        prefix = f"{title} > {heading}"
        for body in chunk_section(text):
            chunks.append(
                {"title": title, "heading": heading, "url": url, "text": f"{prefix}\n\n{body}"}
            )
    return chunks


def chunk_documents(documents: Iterable[dict]) -> list[dict[str, str]]:
    chunks: list[dict[str, str]] = []
    for document in documents:
        chunks.extend(chunk_article(document))
    return chunks


# --------------------------------------------------------------------------- #
# Store
# --------------------------------------------------------------------------- #
@dataclass
class Store:
    """A loaded index: chunk rows, their vectors and the metadata."""

    chunks: list[dict[str, str]]
    vectors: np.ndarray
    meta: dict[str, str]

    @property
    def model(self) -> str:
        return self.meta.get("model", "")

    @property
    def dimension(self) -> int:
        return int(self.meta.get("dim", self.vectors.shape[1] if self.vectors.size else 0))

    def search(self, query_vector: np.ndarray, k: int) -> list[tuple[dict[str, str], float]]:
        if not self.chunks or k <= 0:
            return []
        query = np.asarray(query_vector, dtype=np.float32).reshape(-1)
        if query.shape[0] != self.vectors.shape[1]:
            raise ValueError(
                f"query vector has {query.shape[0]} dimensions, index has {self.vectors.shape[1]}"
            )
        scores = self.vectors @ query
        order = np.argsort(-scores)[: min(k, len(self.chunks))]
        return [(self.chunks[int(index)], float(scores[int(index)])) for index in order]


def build(
    *,
    text: Path | None = None,
    index_path: Path | None = None,
    vectors_path: Path | None = None,
    documents: list[dict] | None = None,
    embedder: Embedder | None = None,
    progress: Callable[[int, int], None] | None = None,
) -> Store:
    """Build the index from extracted article JSON and return the stored form."""
    from .extract import load_articles

    sqlite_path = Path(index_path) if index_path is not None else paths.index_path()
    matrix_path = Path(vectors_path) if vectors_path is not None else paths.vectors_path()
    pages = documents if documents is not None else load_articles(text)
    chunks = chunk_documents(pages)
    engine = embedder or FastEmbedder()
    sqlite_path.parent.mkdir(parents=True, exist_ok=True)
    matrix_path.parent.mkdir(parents=True, exist_ok=True)

    vectors = np.zeros((0, 0), dtype=np.float32)
    if chunks:
        batches: list[np.ndarray] = []
        batch_size = 32
        for start in range(0, len(chunks), batch_size):
            batch = chunks[start : start + batch_size]
            batches.append(engine.embed_documents([chunk["text"] for chunk in batch]))
            if progress is not None:
                progress(min(start + batch_size, len(chunks)), len(chunks))
        vectors = np.vstack(batches).astype(np.float32)

    if sqlite_path.exists():
        sqlite_path.unlink()
    connection = sqlite3.connect(sqlite_path)
    try:
        connection.execute(
            "CREATE TABLE chunks (id INTEGER PRIMARY KEY, title TEXT, heading TEXT, "
            "url TEXT, text TEXT)"
        )
        connection.execute("CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT)")
        connection.executemany(
            "INSERT INTO chunks (id, title, heading, url, text) VALUES (?, ?, ?, ?, ?)",
            [
                (number, chunk["title"], chunk["heading"], chunk["url"], chunk["text"])
                for number, chunk in enumerate(chunks)
            ],
        )
        meta = {
            "model": engine.model,
            "dim": str(vectors.shape[1] if vectors.size else 0),
            "chunks": str(len(chunks)),
            "pages": str(len(pages)),
            "built_at": datetime.now(timezone.utc).isoformat(timespec="seconds"),
            "source": "https://minecraft.wiki/w/<Title>",
            "licence": LICENCE,
        }
        connection.executemany(
            "INSERT INTO meta (key, value) VALUES (?, ?)", sorted(meta.items())
        )
        connection.commit()
    finally:
        connection.close()
    np.save(matrix_path, vectors)

    stored_chunks = [
        {"title": chunk["title"], "heading": chunk["heading"], "url": chunk["url"], "text": chunk["text"]}
        for chunk in chunks
    ]
    return Store(stored_chunks, vectors, meta)


def load(
    directory: Path | None = None,
    *,
    index_path: Path | None = None,
    vectors_path: Path | None = None,
) -> Store:
    """Load an existing index; raises ``FileNotFoundError`` when it is missing."""
    if directory is not None:
        root = Path(directory)
        sqlite_path, matrix_path = root / "index.sqlite", root / "vectors.npy"
    else:
        sqlite_path = Path(index_path) if index_path is not None else paths.index_path()
        matrix_path = Path(vectors_path) if vectors_path is not None else paths.vectors_path()
    if not sqlite_path.exists() or not matrix_path.exists():
        raise FileNotFoundError(f"no wiki index at {sqlite_path.parent}")

    connection = sqlite3.connect(f"file:{sqlite_path}?mode=ro", uri=True)
    try:
        meta = dict(connection.execute("SELECT key, value FROM meta").fetchall())
        rows = connection.execute(
            "SELECT title, heading, url, text FROM chunks ORDER BY id"
        ).fetchall()
    finally:
        connection.close()
    chunks = [
        {"title": title, "heading": heading, "url": url, "text": text}
        for title, heading, url, text in rows
    ]
    vectors = np.load(matrix_path, allow_pickle=False).astype(np.float32)
    if vectors.ndim != 2 or vectors.shape[0] != len(chunks):
        raise ValueError(
            f"index is inconsistent: {len(chunks)} chunks but vectors {vectors.shape}"
        )
    return Store(chunks, vectors, meta)


__all__ = [
    "DEFAULT_MODEL",
    "Embedder",
    "EmbedderUnavailable",
    "FastEmbedder",
    "LICENCE",
    "Store",
    "TARGET_MAX_WORDS",
    "TARGET_MIN_WORDS",
    "build",
    "chunk_article",
    "chunk_documents",
    "chunk_section",
    "load",
    "normalize_rows",
    "task_prefixes",
]

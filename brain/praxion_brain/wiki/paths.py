"""Machine-local paths for the wiki RAG.

Everything the wiki RAG downloads or builds lives under ``local/wiki/`` (git
ignored). Nothing from minecraft.wiki is ever committed; see the licence note
in ``brain/README.md``.
"""

from __future__ import annotations

from pathlib import Path

from .. import config


def wiki_dir() -> Path:
    """The wiki RAG's machine-local root (``local/wiki/``)."""
    return config.local_dir() / "wiki"


def raw_dir() -> Path:
    """Raw article HTML, one file per page (``local/wiki/raw/``)."""
    return wiki_dir() / "raw"


def text_dir() -> Path:
    """Extracted article JSON, one file per page (``local/wiki/text/``)."""
    return wiki_dir() / "text"


def index_path() -> Path:
    """The SQLite chunk table (``local/wiki/index.sqlite``)."""
    return wiki_dir() / "index.sqlite"


def vectors_path() -> Path:
    """The float32 embedding matrix (``local/wiki/vectors.npy``)."""
    return wiki_dir() / "vectors.npy"


def models_dir() -> Path:
    """The local embedding-model cache (``local/wiki/models/``)."""
    return wiki_dir() / "models"


def pages_path() -> Path:
    """The fixed article list (``brain/praxion_brain/wiki/pages.txt``)."""
    return Path(__file__).with_name("pages.txt")


def report_path() -> Path:
    """The fetch report (``local/wiki/fetch_report.json``)."""
    return wiki_dir() / "fetch_report.json"


def questions_path() -> Path:
    """The retrieval test questions shipped with the package."""
    return Path(__file__).with_name("eval_questions.json")


__all__ = [
    "index_path",
    "models_dir",
    "pages_path",
    "questions_path",
    "raw_dir",
    "report_path",
    "text_dir",
    "vectors_path",
    "wiki_dir",
]

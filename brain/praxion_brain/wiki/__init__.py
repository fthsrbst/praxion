"""The local Minecraft Wiki RAG: ``search(query, k) -> list[Snippet]``.

The index is built by ``python -m praxion_brain wiki build`` under
``local/wiki/`` and is never committed or distributed. ``search`` is the only
entry point the rest of the brain uses:

- the index and the embedding model load lazily, once per process,
- loading is thread-safe,
- a missing index or an embedder that cannot load is logged once and returns
  ``[]``; ``search`` never raises.
"""

from __future__ import annotations

import logging
import threading
from typing import Callable

from pydantic import BaseModel

from . import index, paths

_LOG = logging.getLogger(__name__)

#: The model to build with; see ``index.DEFAULT_MODEL``.
MODEL = index.DEFAULT_MODEL


class Snippet(BaseModel):
    """One retrieved passage (the contract's shape)."""

    title: str
    text: str
    url: str
    score: float


_LOCK = threading.RLock()
_STATE: tuple[index.Embedder, index.Store] | None = None
_TRIED = False
_WARNED: set[str] = set()

#: Replaced by the tests so no model is downloaded.
_EMBEDDER_FACTORY: Callable[[str], index.Embedder] = index.FastEmbedder


def _log_once(key: str, message: str, *args: object) -> None:
    if key in _WARNED:
        return
    _WARNED.add(key)
    _LOG.warning(message, *args)


def _state() -> tuple[index.Embedder, index.Store] | None:
    """Load the store and its embedder once; ``None`` when unavailable."""
    global _STATE, _TRIED
    with _LOCK:
        if _TRIED:
            return _STATE
        _TRIED = True
        try:
            store = index.load()
        except FileNotFoundError:
            _log_once(
                "missing",
                "wiki index missing under %s; run `python -m praxion_brain wiki fetch` "
                "then `python -m praxion_brain wiki build`",
                paths.wiki_dir(),
            )
            return None
        except Exception as error:
            _log_once("broken", "wiki index unreadable: %s", error)
            return None
        try:
            embedder = _EMBEDDER_FACTORY(store.model)
        except Exception as error:
            _log_once("embedder", "wiki embedder unavailable: %s", error)
            return None
        built_with = getattr(embedder, "model", "")
        if built_with != store.model:
            _log_once(
                "model",
                "wiki index was built with %r but the embedder is %r; rebuild with "
                "`python -m praxion_brain wiki build`",
                store.model,
                built_with,
            )
            return None
        _STATE = (embedder, store)
        return _STATE


def search(query: str, k: int = 3) -> list[Snippet]:
    """The ``k`` best passages for a query, or ``[]``; never raises."""
    text = (query or "").strip()
    if not text or k <= 0:
        return []
    try:
        state = _state()
        if state is None:
            return []
        embedder, store = state
        with _LOCK:
            vector = embedder.embed_query(text)
        hits = store.search(vector, k)
        return [
            Snippet(title=chunk["title"], text=chunk["text"], url=chunk["url"], score=score)
            for chunk, score in hits
        ]
    except Exception as error:  # search must never take the brain down
        _log_once("failed", "wiki search failed: %s", error)
        return []


def _reset_for_tests() -> None:
    """Forget the loaded index and the warnings (tests only)."""
    global _STATE, _TRIED
    with _LOCK:
        _STATE = None
        _TRIED = False
        _WARNED.clear()


__all__ = ["MODEL", "Snippet", "search"]

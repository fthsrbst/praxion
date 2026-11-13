"""Item icons from the machine-local Minecraft client jar.

The console shows the bot's inventory. A lettered monogram is the honest
fallback, but the client's own textures make the panel readable at a glance.
Nothing is downloaded or copied to disk: the 26.3 jar that Quick Play already
launches is read in place, and the few textures an item needs become ``data:``
URLs for the browser (the console's CSP already allows ``img-src data:``).

The lookup is deliberately narrow. Only a canonical vanilla id (bare or
``minecraft:`` prefixed) reaches the jar, and only the fixed asset prefixes
below are ever opened. The item definition (``assets/minecraft/items/<id>.json``,
the 1.21.4+ format this client ships) names a model; the model's ``parent``
chain supplies elements and texture variables; the resolved textures become
either a sprite (generated item layers) or a three-face cube. A model the
resolver does not understand degrades to one representative texture, and
anything unreadable returns ``None`` so the UI keeps its monogram.
"""

from __future__ import annotations

import base64
import json
import re
import threading
import zipfile
import zlib
from collections import OrderedDict
from pathlib import Path
from typing import Any

__all__ = ["ItemIcons", "default_jar_path"]

#: Canonical vanilla item ids only: lowercase words, no namespace pieces,
#: slashes or dots. 48 covers the longest vanilla id with room to spare.
_ITEM_ID = re.compile(r"^[a-z0-9_]{1,48}$")
#: A model or texture path inside the jar: one or more lowercase segments.
_ASSET_PATH = re.compile(r"^[a-z0-9_]+(?:/[a-z0-9_.-]+)+$")
#: Prefixes an internal identifier may name; nothing else is ever opened.
_ASSET_PREFIXES = ("block/", "item/")
_PNG_MAGIC = b"\x89PNG\r\n\x1a\n"

MAX_JSON_BYTES = 256 * 1024
MAX_TEXTURE_BYTES = 2 * 1024 * 1024
MAX_MODEL_DEPTH = 8
MAX_ITEM_MODEL_DEPTH = 6
MAX_TEXTURE_HOPS = 8
MAX_LAYERS = 8
MAX_ELEMENTS = 64
MAX_CACHED_ITEMS = 512
MAX_CACHED_MODELS = 256
MAX_CACHED_TEXTURES = 512


def default_jar_path() -> Path:
    """The jar Quick Play launches: the repo-local, git-ignored 26.3 client."""
    # Importing the brain config first puts ``harness/`` on ``sys.path``; the
    # harness owns the one definition of where a client lives.
    from .. import config

    from praxion_harness.watch_client import CLIENT_ROOT, ClientPaths

    return ClientPaths(config.repo_root() / CLIENT_ROOT).version_jar


def _item_key(item: Any) -> str | None:
    """The bare id to look up, or ``None`` when it is not a canonical id."""
    if not isinstance(item, str):
        return None
    name = item[len("minecraft:") :] if item.startswith("minecraft:") else item
    return name if _ITEM_ID.fullmatch(name) else None


def _asset_path(value: Any) -> str | None:
    """A safe in-jar path from a model/texture identifier, or ``None``."""
    if not isinstance(value, str) or len(value) > 160:
        return None
    name = value[len("minecraft:") :] if value.startswith("minecraft:") else value
    if not _ASSET_PATH.fullmatch(name) or ".." in name:
        return None
    return name if name.startswith(_ASSET_PREFIXES) else None


def _remember(cache: OrderedDict[str, Any], key: str, value: Any, limit: int) -> None:
    cache[key] = value
    cache.move_to_end(key)
    while len(cache) > limit:
        cache.popitem(last=False)


def _copy(result: dict[str, Any] | None) -> dict[str, Any] | None:
    """A caller may serialise or mutate the result; the cache stays untouched."""
    if result is None:
        return None
    if result["kind"] == "sprite":
        return {"kind": "sprite", "layers": list(result["layers"])}
    return {
        "kind": "cube",
        "top": result["top"],
        "left": result["left"],
        "right": result["right"],
    }


def _merged_textures(chain: list[dict[str, Any]]) -> dict[str, str]:
    """Texture variables from the whole parent chain; the child wins."""
    textures: dict[str, str] = {}
    for model in chain:
        values = model.get("textures")
        if not isinstance(values, dict):
            continue
        for name, value in values.items():
            if isinstance(name, str) and isinstance(value, str):
                if len(name) <= 64 and len(value) <= 160:
                    textures[name] = value
    return textures


def _full_cube_faces(chain: list[dict[str, Any]]) -> dict[str, Any] | None:
    """Faces of the block-shaped element, if the model has one.

    The nearest model that declares ``elements`` decides: a child model that
    reshapes the cube (a slab, a torch) must not inherit its parent's cube.
    """
    for model in reversed(chain):
        if "elements" not in model:
            continue
        elements = model.get("elements")
        if not isinstance(elements, list):
            return None
        for element in elements[:MAX_ELEMENTS]:
            if not isinstance(element, dict):
                continue
            if element.get("from") == [0, 0, 0] and element.get("to") == [16, 16, 16]:
                faces = element.get("faces")
                return faces if isinstance(faces, dict) else None
        return None
    return None


def _find_model_ref(node: Any, depth: int = 0) -> str | None:
    """The first plain model a 26.3 item definition names.

    Most items are ``{"model": {"type": "minecraft:model", ...}}``; a few wrap
    that in ``condition``/``select``/``range_dispatch``/``composite``. The
    first branch is good enough for an icon (a bow shows its uncharged sprite),
    so this walks a bounded slice of the tree instead of implementing variants.
    """
    if depth > MAX_ITEM_MODEL_DEPTH:
        return None
    if isinstance(node, dict):
        ref = node.get("model")
        if isinstance(ref, str) and node.get("type", "minecraft:model") == "minecraft:model":
            return ref
        for key in ("fallback", "on_false", "on_true", "model", "cases", "models", "entries"):
            if key in node:
                found = _find_model_ref(node[key], depth + 1)
                if found is not None:
                    return found
        return None
    if isinstance(node, list):
        for child in node[:16]:
            found = _find_model_ref(child, depth + 1)
            if found is not None:
                return found
    return None


class ItemIcons:
    """Item icons resolved from one client jar; safe for concurrent requests.

    The jar is opened once (lazily) and kept open; item results, model JSON and
    texture data URLs are cached with fixed bounds. Every method is a no-op
    that returns ``None`` after a missing or unreadable jar, so a machine
    without the client still gets the console's lettered tiles.
    """

    def __init__(self, jar: Path | str | None = None) -> None:
        self._jar_path = Path(jar) if jar is not None else default_jar_path()
        self._lock = threading.Lock()
        self._jar: zipfile.ZipFile | None = None
        self._jar_broken = False
        self._items: OrderedDict[str, dict[str, Any] | None] = OrderedDict()
        self._models: OrderedDict[str, dict[str, Any] | None] = OrderedDict()
        self._textures: OrderedDict[str, str | None] = OrderedDict()

    def close(self) -> None:
        """Release the jar handle; later lookups return ``None``."""
        with self._lock:
            self._jar_broken = True
            self._close_jar()

    def get(self, item: str) -> dict[str, Any] | None:
        """The icon payload for ``item``, or ``None`` when it cannot be built.

        ``{"kind": "sprite", "layers": [data_url, ...]}`` or
        ``{"kind": "cube", "top": data_url, "left": data_url, "right": data_url}``.
        """
        key = _item_key(item)
        if key is None:
            return None
        with self._lock:
            if key in self._items:
                return _copy(self._items[key])
            try:
                result = self._resolve(key)
            except (
                zipfile.BadZipFile,
                OSError,
                RuntimeError,
                NotImplementedError,
                EOFError,
                ValueError,
                zlib.error,
            ):
                # A corrupt or truncated jar must not throw into the HTTP
                # handler, and must not be reopened on every request.
                self._jar_broken = True
                self._close_jar()
                result = None
            _remember(self._items, key, result, MAX_CACHED_ITEMS)
            return _copy(result)

    # -- jar access ----------------------------------------------------- #
    def _close_jar(self) -> None:
        jar, self._jar = self._jar, None
        if jar is not None:
            try:
                jar.close()
            except OSError:
                pass

    def _open_jar(self) -> zipfile.ZipFile | None:
        if self._jar is not None:
            return self._jar
        if self._jar_broken:
            return None
        try:
            self._jar = zipfile.ZipFile(self._jar_path)
        except (OSError, zipfile.BadZipFile, ValueError):
            self._jar_broken = True
            return None
        return self._jar

    def _read(self, jar: zipfile.ZipFile, name: str, limit: int) -> bytes | None:
        """One member, bounded before and after decompression."""
        try:
            info = jar.getinfo(name)
        except KeyError:
            return None
        if info.file_size > limit:
            return None
        try:
            with jar.open(info) as handle:
                data = handle.read(limit + 1)
        except (zipfile.BadZipFile, OSError, RuntimeError, NotImplementedError, EOFError):
            return None
        return data if len(data) <= limit else None

    def _read_json(self, jar: zipfile.ZipFile, name: str) -> dict[str, Any] | None:
        if name in self._models:
            return self._models[name]
        data = self._read(jar, name, MAX_JSON_BYTES)
        model: dict[str, Any] | None = None
        if data is not None:
            try:
                parsed = json.loads(data.decode("utf-8"))
            except (UnicodeDecodeError, json.JSONDecodeError):
                parsed = None
            if isinstance(parsed, dict):
                model = parsed
        _remember(self._models, name, model, MAX_CACHED_MODELS)
        return model

    def _texture_url(self, jar: zipfile.ZipFile, path: str) -> str | None:
        if path in self._textures:
            return self._textures[path]
        data = self._read(jar, f"assets/minecraft/textures/{path}.png", MAX_TEXTURE_BYTES)
        url = None
        if data is not None and data.startswith(_PNG_MAGIC):
            url = "data:image/png;base64," + base64.b64encode(data).decode("ascii")
        _remember(self._textures, path, url, MAX_CACHED_TEXTURES)
        return url

    # -- model resolution ----------------------------------------------- #
    def _model_chain(
        self,
        jar: zipfile.ZipFile,
        path: str,
        depth: int = 0,
        seen: frozenset[str] = frozenset(),
    ) -> list[dict[str, Any]]:
        if depth > MAX_MODEL_DEPTH or path in seen:
            return []
        model = self._read_json(jar, f"assets/minecraft/models/{path}.json")
        if model is None:
            return []
        parent = model.get("parent")
        chain: list[dict[str, Any]] = []
        if isinstance(parent, str) and not parent.startswith("builtin/"):
            parent_path = _asset_path(parent)
            if parent_path is not None:
                chain = self._model_chain(jar, parent_path, depth + 1, seen | {path})
        return chain + [model]

    def _concrete_texture(
        self,
        ref: Any,
        textures: dict[str, str],
        hops: int = 0,
    ) -> str | None:
        """Follow ``#variable`` references down to an in-jar texture path."""
        seen: set[str] = set()
        while isinstance(ref, str) and ref.startswith("#"):
            if hops >= MAX_TEXTURE_HOPS:
                return None
            name = ref[1:]
            if name in seen or name not in textures:
                return None
            seen.add(name)
            ref = textures[name]
            hops += 1
        return _asset_path(ref)

    def _face_url(
        self,
        jar: zipfile.ZipFile,
        faces: dict[str, Any],
        names: tuple[str, ...],
        textures: dict[str, str],
    ) -> str | None:
        for name in names:
            face = faces.get(name)
            if not isinstance(face, dict):
                continue
            path = self._concrete_texture(face.get("texture"), textures)
            if path is None:
                continue
            url = self._texture_url(jar, path)
            if url is not None:
                return url
        return None

    def _layers(self, jar: zipfile.ZipFile, textures: dict[str, str]) -> list[str]:
        names = [
            name
            for name in textures
            if re.fullmatch(r"layer\d{1,2}", name) is not None
        ]
        names.sort(key=lambda name: int(name[len("layer") :]))
        urls: list[str] = []
        for name in names[:MAX_LAYERS]:
            path = self._concrete_texture(textures[name], textures)
            if path is None:
                continue
            url = self._texture_url(jar, path)
            if url is not None:
                urls.append(url)
        return urls

    def _representative(self, jar: zipfile.ZipFile, textures: dict[str, str]) -> str | None:
        """One reliable texture for a model the resolver cannot shape."""
        preferred = ("layer0", "all", "particle", "texture", "front", "top", "side", "end", "cross")
        tried: set[str] = set()
        for name in (*preferred, *sorted(textures)):
            if name in tried:
                continue
            tried.add(name)
            path = self._concrete_texture(textures.get(name), textures)
            if path is None:
                continue
            url = self._texture_url(jar, path)
            if url is not None:
                return url
        return None

    def _resolve(self, key: str) -> dict[str, Any] | None:
        jar = self._open_jar()
        if jar is None:
            return None

        definition = self._read_json(jar, f"assets/minecraft/items/{key}.json")
        model_path = _asset_path(_find_model_ref(definition)) if definition else None

        if model_path is not None:
            chain = self._model_chain(jar, model_path)
            if chain:
                textures = _merged_textures(chain)
                faces = _full_cube_faces(chain)
                if faces is not None:
                    top = self._face_url(
                        jar, faces, ("up", "down", "north", "south", "east", "west"), textures
                    )
                    left = self._face_url(
                        jar, faces, ("north", "west", "east", "south", "up", "down"), textures
                    )
                    right = self._face_url(
                        jar, faces, ("east", "south", "west", "north", "up", "down"), textures
                    )
                    if top is not None and left is not None and right is not None:
                        return {"kind": "cube", "top": top, "left": left, "right": right}
                layers = self._layers(jar, textures)
                if layers:
                    return {"kind": "sprite", "layers": layers}
                url = self._representative(jar, textures)
                if url is not None:
                    return {"kind": "sprite", "layers": [url]}

        # No usable model: an item texture, then a block texture of the same
        # name. Old bodies and a few special models still get a real icon.
        for folder in ("item", "block"):
            url = self._texture_url(jar, f"{folder}/{key}")
            if url is not None:
                return {"kind": "sprite", "layers": [url]}
        return None

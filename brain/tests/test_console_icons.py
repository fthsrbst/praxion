from __future__ import annotations

import base64
import http.client
import json
import struct
import threading
import zipfile
import zlib
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import pytest

from praxion_brain.console import icons as icons_module
from praxion_brain.console.icons import ItemIcons, default_jar_path

CUBE = {
    "parent": "block/block",
    "elements": [{
        "from": [0, 0, 0],
        "to": [16, 16, 16],
        "faces": {
            "down": {"texture": "#down"},
            "up": {"texture": "#up"},
            "north": {"texture": "#north"},
            "south": {"texture": "#south"},
            "west": {"texture": "#west"},
            "east": {"texture": "#east"},
        },
    }],
}


def png(red: int) -> bytes:
    """A valid 1x1 RGB PNG; the resolver only needs a real PNG header."""
    def chunk(tag: bytes, payload: bytes) -> bytes:
        body = tag + payload
        return struct.pack(">I", len(payload)) + body + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)

    header = struct.pack(">IIBBBBB", 1, 1, 8, 2, 0, 0, 0)
    pixel = bytes((0, red, 0, 0))  # one scanline: filter byte + RGB
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", header)
        + chunk(b"IDAT", zlib.compress(pixel))
        + chunk(b"IEND", b"")
    )


TEXTURES = {
    "block/oak_log": png(1),
    "block/oak_log_top": png(2),
    "block/stone": png(3),
    "block/crafting_table_front": png(4),
    "block/crafting_table_side": png(5),
    "block/crafting_table_top": png(6),
    "item/apple": png(7),
    "item/stick": png(8),
}


def asset(member: str, value: object) -> tuple[str, bytes]:
    return f"assets/minecraft/{member}", json.dumps(value, indent=2).encode()


def build_jar(root: Path) -> Path:
    """A miniature 26.3 client: the real model shapes, tiny real PNGs."""
    path = root / "mini-26.3.jar"
    members: list[tuple[str, bytes]] = [
        ("assets/minecraft/items/oak_log.json",
         json.dumps({"model": {"type": "minecraft:model",
                               "model": "minecraft:block/oak_log"}}).encode()),
        asset("models/block/oak_log.json", {
            "parent": "minecraft:block/cube_column",
            "textures": {"end": "minecraft:block/oak_log_top", "side": "minecraft:block/oak_log"},
        }),
        asset("models/block/cube_column.json", {
            "parent": "block/cube",
            "textures": {
                "particle": "#side", "down": "#end", "up": "#end",
                "north": "#side", "east": "#side", "south": "#side", "west": "#side",
            },
        }),
        asset("models/block/cube.json", CUBE),
        ("assets/minecraft/items/stone.json",
         json.dumps({"model": {"type": "minecraft:model", "model": "block/stone"}}).encode()),
        asset("models/block/stone.json", {
            "parent": "minecraft:block/cube_all",
            "textures": {"all": "minecraft:block/stone"},
        }),
        asset("models/block/cube_all.json", {
            "parent": "block/cube",
            "textures": {
                "particle": "#all", "down": "#all", "up": "#all",
                "north": "#all", "east": "#all", "south": "#all", "west": "#all",
            },
        }),
        ("assets/minecraft/items/crafting_table.json",
         json.dumps({"model": {"type": "minecraft:model", "model": "block/crafting_table"}}).encode()),
        asset("models/block/crafting_table.json", {
            "parent": "minecraft:block/cube",
            "textures": {
                "down": "minecraft:block/oak_log",
                "up": "minecraft:block/crafting_table_top",
                "north": "minecraft:block/crafting_table_front",
                "south": "minecraft:block/crafting_table_side",
                "west": "minecraft:block/crafting_table_front",
                "east": "minecraft:block/crafting_table_side",
            },
        }),
        ("assets/minecraft/items/apple.json",
         json.dumps({"model": {"type": "minecraft:model", "model": "item/apple"}}).encode()),
        asset("models/item/apple.json", {
            "parent": "minecraft:item/generated",
            "textures": {"layer0": "minecraft:item/apple"},
        }),
        asset("models/item/generated.json", {"parent": "builtin/generated"}),
        ("assets/minecraft/items/layered.json",
         json.dumps({"model": {"type": "minecraft:model", "model": "item/layered"}}).encode()),
        asset("models/item/layered.json", {
            "parent": "minecraft:item/generated",
            "textures": {"layer0": "item/apple", "layer1": "item/stick"},
        }),
        # A conditional definition: the first usable branch is the icon.
        ("assets/minecraft/items/conditional.json", json.dumps({
            "model": {
                "type": "minecraft:condition",
                "property": "minecraft:using_item",
                "on_false": {"type": "minecraft:model", "model": "item/layered"},
                "on_true": {"type": "minecraft:model", "model": "item/apple"},
            },
        }).encode()),
        ("assets/minecraft/items/ghost.json",
         json.dumps({"model": {"type": "minecraft:model", "model": "block/ghost"}}).encode()),
        asset("models/block/ghost.json", {
            "parent": "block/cube_all",
            "textures": {"all": "minecraft:block/missing"},
        }),
        # An untrusted model identifier must never become a jar path.
        ("assets/minecraft/items/evil.json",
         json.dumps({"model": {"type": "minecraft:model",
                               "model": "../../../etc/passwd"}}).encode()),
        ("assets/minecraft/items/broken.json", b"{ not json"),
    ]
    for name, data in TEXTURES.items():
        members.append((f"assets/minecraft/textures/{name}.png", data))

    with zipfile.ZipFile(path, "w") as jar:
        for name, data in members:
            jar.writestr(name, data)
    return path


@pytest.fixture
def jar(tmp_path: Path) -> Path:
    return build_jar(tmp_path)


@pytest.fixture
def icons(jar: Path) -> ItemIcons:
    resolver = ItemIcons(jar)
    yield resolver
    resolver.close()


def payload(url: str) -> bytes:
    assert url.startswith("data:image/png;base64,"), url[:40]
    return base64.b64decode(url[len("data:image/png;base64,"):])


def test_a_cube_column_follows_parent_variables_and_faces(jar: Path) -> None:
    result = ItemIcons(jar).get("minecraft:oak_log")
    assert result is not None and result["kind"] == "cube"
    assert payload(result["top"]) == TEXTURES["block/oak_log_top"]
    assert payload(result["left"]) == TEXTURES["block/oak_log"]
    assert payload(result["right"]) == TEXTURES["block/oak_log"]


def test_cube_all_uses_one_texture_for_every_face(jar: Path) -> None:
    result = ItemIcons(jar).get("stone")
    assert result is not None and result["kind"] == "cube"
    assert {payload(result[face]) for face in ("top", "left", "right")} == {TEXTURES["block/stone"]}


def test_direct_faces_and_no_namespace_are_supported(jar: Path) -> None:
    result = ItemIcons(jar).get("crafting_table")
    assert result is not None and result["kind"] == "cube"
    assert payload(result["top"]) == TEXTURES["block/crafting_table_top"]
    assert payload(result["left"]) == TEXTURES["block/crafting_table_front"]
    assert payload(result["right"]) == TEXTURES["block/crafting_table_side"]


def test_generated_items_are_layered_sprites(jar: Path) -> None:
    apple = ItemIcons(jar).get("apple")
    assert apple == {"kind": "sprite", "layers": [
        "data:image/png;base64," + base64.b64encode(TEXTURES["item/apple"]).decode(),
    ]}
    layered = ItemIcons(jar).get("layered")
    assert layered is not None and layered["kind"] == "sprite"
    assert [payload(url) for url in layered["layers"]] == [TEXTURES["item/apple"], TEXTURES["item/stick"]]


def test_a_conditional_definition_uses_its_first_model(jar: Path) -> None:
    result = ItemIcons(jar).get("conditional")
    assert result is not None and result["kind"] == "sprite"
    assert [payload(url) for url in result["layers"]] == [TEXTURES["item/apple"], TEXTURES["item/stick"]]


def test_untrusted_ids_never_reach_the_jar(jar: Path) -> None:
    resolver = ItemIcons(jar)
    for item in (
        "",
        "minecraft:",
        "Stone",
        "stone/extra",
        "minecraft:stone/extra",
        "../stone",
        "minecraft:../stone",
        "stone.png",
        "other:stone",
        "stone\x00",
        "a" * 60,
        None,
        7,
    ):
        assert resolver.get(item) is None, item


def test_untrusted_model_identifier_is_ignored(jar: Path) -> None:
    assert ItemIcons(jar).get("evil") is None


def test_missing_texture_missing_model_and_broken_json_fall_back(jar: Path) -> None:
    resolver = ItemIcons(jar)
    assert resolver.get("ghost") is None
    assert resolver.get("broken") is None
    assert resolver.get("not_an_item") is None
    # One bad member must not poison the jar for later lookups.
    assert resolver.get("stone") is not None


def test_missing_jar_returns_none(tmp_path: Path) -> None:
    resolver = ItemIcons(tmp_path / "absent.jar")
    assert resolver.get("minecraft:stone") is None


def test_results_are_copies_of_the_cache(jar: Path) -> None:
    resolver = ItemIcons(jar)
    first = resolver.get("stone")
    assert first is not None
    first["top"] = "mutated"
    assert resolver.get("stone")["top"] != "mutated"


def test_the_jar_is_opened_once_for_repeated_gets(jar: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    opened: list[Path] = []
    real = zipfile.ZipFile

    def counting(file, *args, **kwargs):
        opened.append(Path(file))
        return real(file, *args, **kwargs)

    monkeypatch.setattr(icons_module.zipfile, "ZipFile", counting)
    resolver = ItemIcons(jar)
    for _ in range(4):
        assert resolver.get("stone") is not None
    assert opened == [jar]


def test_concurrent_gets_agree(jar: Path) -> None:
    resolver = ItemIcons(jar)
    expected = {"oak_log": resolver.get("oak_log"), "stone": resolver.get("stone"),
                "apple": resolver.get("apple")}
    with ThreadPoolExecutor(max_workers=8) as pool:
        results = list(pool.map(lambda name: resolver.get(name), list(expected) * 20))
    assert results == list(expected.values()) * 20
    threads = [threading.Thread(target=resolver.get, args=("stone",)) for _ in range(4)]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join()


def test_the_panel_contract_matches_the_route() -> None:
    """The static side is pinned where the server contract lives.

    The console has no JS test runner, so these source assertions cover only
    the seams that can silently drift: the route path, caching before the
    request (a redraw during a load must not fetch twice), the "never reuse a
    stale tile" check, and the stylesheet the integrator must load.
    """
    from praxion_brain.console.server import STATIC_DIR

    panel = (STATIC_DIR / "inventory.js").read_text(encoding="utf-8")
    assert '"/api/item-icon/"' in panel
    assert panel.index("icons.set(item, entry)") < panel.index("getJSON(ICON_PATH")
    assert 'node.dataset.item === item' in panel
    assert "await" not in panel  # rendering never waits for an icon
    assert "itemGlyph" in panel  # the monogram fallback is still used
    css = (STATIC_DIR / "inventory.css").read_text(encoding="utf-8")
    assert "image-rendering: pixelated" in css
    assert ".inv-icon .face.top" in css
    assert "transform-origin: 0 0" in css
    assert "matrix(.5, .25, -.5, .25, 0, 0)" in css
    assert "polygon(" not in css  # a projected face, not a clipped crop


def test_the_real_client_jar_answers_without_dumping_textures() -> None:
    jar = default_jar_path()
    if not jar.is_file():
        pytest.skip("machine-local 26.3 client jar is not installed")
    resolver = ItemIcons()
    oak = resolver.get("minecraft:oak_log")
    assert oak is not None and oak["kind"] == "cube"
    assert oak["top"].startswith("data:image/png;base64,")
    stone = resolver.get("minecraft:stone")
    assert stone is not None and stone["kind"] == "cube"
    assert resolver.get("minecraft:not_a_real_item") is None
    resolver.close()


class _ConsoleStub:
    """Only what ConsoleServer reads before an endpoint needs a live console."""

    def __init__(self, root: Path) -> None:
        from praxion_brain.evidence import Evidence
        from praxion_brain.ledger import Ledger

        self.ledger = Ledger(root / "ledger.jsonl")
        self.evidence = Evidence(root / "evidence")


def test_the_item_icon_route_serves_the_jar_and_closes_on_shutdown(
    jar: Path, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The HTTP seam: the fixture resolver answers the UI's exact path."""
    from praxion_brain.console.server import ConsoleServer

    resolver = ItemIcons(jar)
    real_close = resolver.close
    closed: list[bool] = []

    def tracked_close() -> None:
        closed.append(True)
        real_close()

    monkeypatch.setattr(resolver, "close", tracked_close)
    server = ConsoleServer(_ConsoleStub(tmp_path), 0, icons=resolver).start()

    def get(path: str) -> tuple[int, object]:
        connection = http.client.HTTPConnection("127.0.0.1", server.port, timeout=5)
        connection.request("GET", path)
        response = connection.getresponse()
        try:
            return response.status, json.loads(response.read())
        finally:
            connection.close()

    try:
        status, body = get("/api/item-icon/stone")
        assert status == 200
        assert body["kind"] == "cube"
        assert body["top"].startswith("data:image/png;base64,")

        status, body = get("/api/item-icon/not_an_item")
        assert status == 404 and "error" in body

        # An encoded traversal is still an untrusted id, never a jar path.
        status, body = get("/api/item-icon/..%2F..%2Fetc%2Fpasswd")
        assert status == 404 and "error" in body
    finally:
        server.shutdown()

    assert closed == [True]

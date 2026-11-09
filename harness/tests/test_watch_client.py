"""Unit tests for the watch client launcher: rules, classpath, args, uuid.

Nothing here starts Java or a server. The one test that reads the real client
under ``third_party/minecraft-client`` skips when it is not installed.
"""

from __future__ import annotations

import hashlib
import os
import re
from pathlib import Path

import pytest

from praxion_harness import watch_client as wc
from praxion_harness.paths import repo_root

#: The offline-mode UUID of ``Watcher``, from Java's
#: ``UUID.nameUUIDFromBytes("OfflinePlayer:Watcher")``; hex, no dashes.
WATCHER_UUID = "e0314eaaf0cf36b0ad18d19b22fa52c8"


def osx_platform(features=None) -> wc.Platform:
    return wc.Platform(
        os_name="osx",
        arch="arm64",
        os_version="15.0",
        features=dict(wc.DEFAULT_FEATURES if features is None else features),
    )


def windows_platform() -> wc.Platform:
    return wc.Platform(
        os_name="windows",
        arch="x86_64",
        os_version="10.0.19045",
        features=dict(wc.DEFAULT_FEATURES),
    )


# -- rule evaluation --------------------------------------------------------


def test_no_rules_allows():
    assert wc.rule_allows(None, osx_platform())
    assert wc.rule_allows([], osx_platform())


def test_osx_rule_allows_on_osx_and_drops_on_windows():
    rules = [{"action": "allow", "os": {"name": "osx"}}]
    assert wc.rule_allows(rules, osx_platform())
    assert not wc.rule_allows(rules, windows_platform())


def test_windows_only_rule_dropped_on_osx():
    rules = [{"action": "allow", "os": {"name": "windows"}}]
    assert not wc.rule_allows(rules, osx_platform())
    assert wc.rule_allows(rules, windows_platform())


def test_arch_rule():
    rules = [{"action": "allow", "os": {"name": "osx", "arch": "arm64"}}]
    assert wc.rule_allows(rules, osx_platform())
    assert not wc.rule_allows(
        rules, wc.Platform(os_name="osx", arch="x86_64", features=dict(wc.DEFAULT_FEATURES))
    )


def test_last_matching_rule_decides():
    rules = [
        {"action": "allow", "os": {"name": "osx"}},
        {"action": "disallow", "os": {"name": "osx"}},
    ]
    assert not wc.rule_allows(rules, osx_platform())
    reversed_rules = list(reversed(rules))
    assert wc.rule_allows(reversed_rules, osx_platform())


def test_features_follow_the_default_set():
    quick_play = [{"action": "allow", "features": {"is_quick_play_multiplayer": True}}]
    demo = [{"action": "allow", "features": {"is_demo_user": True}}]
    resolution = [{"action": "allow", "features": {"has_custom_resolution": True}}]
    assert wc.rule_allows(quick_play, osx_platform())
    assert not wc.rule_allows(demo, osx_platform())
    assert not wc.rule_allows(resolution, osx_platform())


def test_feature_rule_requires_an_explicit_true():
    """A rule asking for a false feature matches the default set."""
    rules = [{"action": "allow", "features": {"is_demo_user": False}}]
    assert wc.rule_allows(rules, osx_platform())


def test_windows_version_range_rules():
    newer = [{"action": "allow", "os": {"name": "windows", "versionRange": {"min": "10.0.17134"}}}]
    older = [{"action": "allow", "os": {"name": "windows", "versionRange": {"max": "10.0.17134"}}}]
    assert wc.rule_allows(newer, windows_platform())
    assert not wc.rule_allows(older, windows_platform())
    old = wc.Platform(
        os_name="windows", arch="x86_64", os_version="10.0.16299", features={}
    )
    assert wc.rule_allows(older, old)
    assert not wc.rule_allows(newer, old)


def test_evaluate_arguments_filters_by_rules():
    entries = [
        "--first",
        {"rules": [{"action": "allow", "os": {"name": "windows"}}], "value": "--windows-only"},
        {
            "rules": [{"action": "allow", "features": {"is_quick_play_multiplayer": True}}],
            "value": ["--quickPlayMultiplayer", "${quickPlayMultiplayer}"],
        },
        {
            "rules": [{"action": "allow", "features": {"has_quick_plays_support": True}}],
            "value": ["--quickPlayPath", "${quickPlayPath}"],
        },
    ]
    assert wc.evaluate_arguments(entries, osx_platform()) == [
        "--first",
        "--quickPlayMultiplayer",
        "${quickPlayMultiplayer}",
    ]


# -- substitution -----------------------------------------------------------


def test_substitute_replaces_every_placeholder():
    values = {"a": "/natives", "b": "praxion-watch", "sep": os.pathsep}
    result = wc.substitute(
        ["-Djava.library.path=${a}/java", "-Dminecraft.launcher.brand=${b}", "-p=${sep}"], values
    )
    assert result == [
        "-Djava.library.path=/natives/java",
        "-Dminecraft.launcher.brand=praxion-watch",
        f"-p={os.pathsep}",
    ]
    assert not any("${" in argument for argument in result)


def test_substitute_unknown_placeholder_is_an_error():
    with pytest.raises(wc.LaunchError, match="resolution_width"):
        wc.substitute(["--width", "${resolution_width}"], {"auth_player_name": "Watcher"})


# -- offline uuid -----------------------------------------------------------


def test_offline_uuid_matches_java_name_uuid():
    raw = bytearray(hashlib.md5(b"OfflinePlayer:Watcher").digest())
    raw[6] = (raw[6] & 0x0F) | 0x30  # version 3
    raw[8] = (raw[8] & 0x3F) | 0x80  # IETF variant
    assert bytes(raw).hex() == WATCHER_UUID

    value = wc.offline_uuid("Watcher")
    assert value == WATCHER_UUID
    assert re.fullmatch(r"[0-9a-f]{32}", value)
    assert value[12] == "3"
    assert value[16] in "89ab"


def test_offline_uuid_is_stable_and_name_dependent():
    assert wc.offline_uuid("Watcher") == wc.offline_uuid("Watcher")
    assert wc.offline_uuid("Watcher") != wc.offline_uuid("Praxion")


# -- classpath --------------------------------------------------------------


def _fake_client(tmp_path: Path, libraries: list[dict]) -> wc.ClientPaths:
    """A minimal client tree with the given libraries and an empty version jar."""
    paths = wc.ClientPaths(tmp_path / "client")
    for library in libraries:
        relative = (library.get("downloads") or {}).get("artifact", {}).get("path")
        if not relative:
            continue
        jar = paths.libraries / relative
        jar.parent.mkdir(parents=True, exist_ok=True)
        jar.write_bytes(b"")
    paths.version_jar.parent.mkdir(parents=True, exist_ok=True)
    paths.version_jar.write_bytes(b"")
    return paths


def _library(name: str, rules=None) -> dict:
    slug = name.replace(":", "-")
    library = {"name": name, "downloads": {"artifact": {"path": f"com/example/{slug}.jar"}}}
    if rules is not None:
        library["rules"] = rules
    return library


def test_classpath_keeps_allowed_libraries_and_ends_with_the_version_jar(tmp_path):
    version = {
        "libraries": [
            _library("com.example:always:1"),
            _library("com.example:osx:1", [{"action": "allow", "os": {"name": "osx"}}]),
            _library("com.example:windows:1", [{"action": "allow", "os": {"name": "windows"}}]),
        ]
    }
    paths = _fake_client(tmp_path, version["libraries"])

    classpath = wc.build_classpath(version, paths, osx_platform())

    assert [entry.name for entry in classpath] == [
        "com.example-always-1.jar",
        "com.example-osx-1.jar",
        f"{wc.VERSION_ID}.jar",
    ]
    assert classpath[-1] == paths.version_jar
    assert all(entry.is_file() for entry in classpath)


def test_classpath_skips_libraries_without_an_artifact(tmp_path):
    version = {
        "libraries": [
            {"name": "com.example:noartifact:1", "downloads": {}},
            _library("com.example:always:1"),
        ]
    }
    paths = _fake_client(tmp_path, version["libraries"])
    classpath = wc.build_classpath(version, paths, osx_platform())
    assert [entry.name for entry in classpath] == [
        "com.example-always-1.jar",
        f"{wc.VERSION_ID}.jar",
    ]


def test_missing_library_is_a_clear_error(tmp_path):
    version = {"libraries": [_library("com.example:always:1")]}
    paths = _fake_client(tmp_path, version["libraries"])
    (paths.libraries / "com/example/com.example-always-1.jar").unlink()
    with pytest.raises(wc.LaunchError, match="com.example:always:1"):
        wc.build_classpath(version, paths, osx_platform())


# -- the real client --------------------------------------------------------


def cmd_value(command: list[str], flag: str) -> str:
    """The argument right after ``flag``."""
    return command[command.index(flag) + 1]


def real_client_root() -> Path:
    return repo_root() / wc.CLIENT_ROOT


needs_client = pytest.mark.skipif(
    not (real_client_root() / "versions" / wc.VERSION_ID / f"{wc.VERSION_ID}.json").is_file(),
    reason="the Minecraft 26.3 client is not installed (third_party/minecraft-client)",
)


@needs_client
def test_dry_run_against_the_real_client(monkeypatch, capsys):
    # No java process: the java path and the version probe are stubbed.
    version = wc.load_version(wc.ClientPaths(real_client_root()))
    monkeypatch.setattr(wc, "resolve_java", lambda: Path("/nonexistent/bin/java"))
    monkeypatch.setattr(wc, "java_major_version", lambda java: wc.expected_java_major(version))

    assert wc.main(["--port", "25565", "--dry-run"]) == 0

    lines = capsys.readouterr().out.splitlines()
    assert lines[0] == "/nonexistent/bin/java"
    assert not any("${" in line for line in lines)
    assert "--quickPlayMultiplayer" in lines
    assert lines[lines.index("--quickPlayMultiplayer") + 1] == "127.0.0.1:25565"
    assert "--quickPlayPath" not in lines
    assert lines[lines.index("--username") + 1] == "Watcher"
    assert lines[lines.index("--uuid") + 1] == WATCHER_UUID
    assert "net.minecraft.client.main.Main" in lines

    classpath = lines[lines.index("-cp") + 1].split(os.pathsep)
    assert classpath[-1] == str(
        real_client_root() / "versions" / wc.VERSION_ID / f"{wc.VERSION_ID}.jar"
    )
    assert all(Path(entry).is_file() for entry in classpath)


@needs_client
def test_real_client_arguments_are_fully_substituted():
    paths = wc.ClientPaths(real_client_root())
    version = wc.load_version(paths)
    command = wc.build_command(
        java=Path("/nonexistent/bin/java"),
        version=version,
        paths=paths,
        name="Watcher",
        host="127.0.0.1",
        port=25565,
    )
    assert not any("${" in argument for argument in command)
    assert command[0] == "/nonexistent/bin/java"
    assert "-Dminecraft.launcher.brand=praxion-watch" in command
    assert f"-Djava.library.path={paths.natives}/java" in command
    assert "--quickPlayMultiplayer" in command
    assert cmd_value(command, "--quickPlayMultiplayer") == "127.0.0.1:25565"
    assert cmd_value(command, "--assetIndex") == version["assetIndex"]["id"]
    assert cmd_value(command, "--gameDir") == str(paths.game)


def cmd_value(command: list[str], flag: str) -> str:
    return command[command.index(flag) + 1]

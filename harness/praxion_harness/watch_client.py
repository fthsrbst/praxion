"""Launch the local Minecraft 26.3 client against a running harness server.

The autonomy runs print the address to join (``127.0.0.1:<port>``); this module
turns that address into a real client launch so the user can watch the bot live:

    python -m praxion_harness.watch_client --port 25565 --name Watcher

``--dry-run`` prints the full command, one argument per line, without starting
anything. Otherwise java is started detached (its output goes nowhere) and the
pid plus ``game-watch/logs/latest.log`` are printed; the command returns at once.

The client files (version json, jar, libraries, assets, natives, the game
directory) live under ``third_party/minecraft-client/``. They are machine-local
and ignored by git; nothing here downloads or writes them. Only the game
directory is written to, and only by Minecraft itself.

Argument and classpath construction follows the vanilla launcher: library and
argument ``rules`` are evaluated for this OS, arch and feature set, then every
``${...}`` placeholder is substituted. A placeholder left over is an error.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import re
import subprocess
import sys
import uuid
from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from .paths import PathResolutionError, repo_root, resolve_java

#: Version id of the client on disk; the json and the jar are named after it.
VERSION_ID = "26.3"
#: Where the machine-local client lives, relative to the repository root.
CLIENT_ROOT = Path("third_party") / "minecraft-client"
#: Launcher identity substituted into the JVM arguments.
LAUNCHER_NAME = "praxion-watch"
LAUNCHER_VERSION = "1"
#: Feature flags for rule evaluation: quick play into a server, nothing else.
DEFAULT_FEATURES: Mapping[str, bool] = {"is_quick_play_multiplayer": True}

_PLACEHOLDER = re.compile(r"\$\{([^}]*)\}")


class LaunchError(RuntimeError):
    """The client cannot be launched as asked (missing file, bad version json)."""


@dataclass(frozen=True)
class Platform:
    """What rule evaluation needs to know about the machine.

    ``os_version`` is only consulted by rules that carry an ``os.versionRange``
    (the Windows-only GC defaults in ``default-user-jvm``).
    """

    os_name: str
    arch: str
    os_version: str = ""
    features: Mapping[str, bool] = field(default_factory=lambda: dict(DEFAULT_FEATURES))

    @classmethod
    def current(cls, features: Mapping[str, bool] | None = None) -> "Platform":
        return cls(
            os_name=_current_os_name(),
            arch=_current_arch(),
            os_version=_current_os_version(),
            features=dict(DEFAULT_FEATURES if features is None else features),
        )


@dataclass(frozen=True)
class ClientPaths:
    """The directories of the machine-local client."""

    root: Path

    @property
    def libraries(self) -> Path:
        return self.root / "libraries"

    @property
    def natives(self) -> Path:
        return self.root / "natives"

    @property
    def assets(self) -> Path:
        return self.root / "assets"

    @property
    def game(self) -> Path:
        """The game directory: options, servers.dat, logs and worlds live here."""
        return self.root / "game-watch"

    @property
    def version_json(self) -> Path:
        return self.root / "versions" / VERSION_ID / f"{VERSION_ID}.json"

    @property
    def version_jar(self) -> Path:
        return self.root / "versions" / VERSION_ID / f"{VERSION_ID}.jar"

    @property
    def log(self) -> Path:
        return self.game / "logs" / "latest.log"


# -- platform detection -----------------------------------------------------


def _current_os_name() -> str:
    """The ``os.name`` value the vanilla launcher uses: osx, windows or linux."""
    system = platform.system()
    if system == "Darwin":
        return "osx"
    if system == "Windows":
        return "windows"
    if system == "Linux":
        return "linux"
    raise LaunchError(f"unsupported operating system for the client: {system!r}")


def _current_arch() -> str:
    """``os.arch`` in launcher terms: x86, x86_64 or arm64."""
    machine = platform.machine().lower()
    if machine in ("arm64", "aarch64"):
        return "arm64"
    if machine in ("x86_64", "amd64"):
        return "x86_64"
    if machine in ("x86", "i386", "i686"):
        return "x86"
    raise LaunchError(f"unsupported architecture for the client: {machine!r}")


def _current_os_version() -> str:
    if platform.system() == "Darwin":
        return platform.mac_ver()[0]
    if platform.system() == "Windows":
        return platform.version()
    return platform.release()


# -- rule evaluation --------------------------------------------------------


def rule_allows(rules: Sequence[Mapping[str, Any]] | None, platform_: Platform) -> bool:
    """Whether ``rules`` allow an item on ``platform_``, the way vanilla does.

    A rule list starts as "denied"; the last rule that matches decides with its
    ``action``. No rules at all means allowed. A rule matches when every one of
    its ``os`` name/arch/versionRange and ``features`` constraints matches.
    """
    if not rules:
        return True
    allowed = False
    for rule in rules:
        if not _rule_matches(rule, platform_):
            continue
        allowed = rule.get("action") == "allow"
    return allowed


def _rule_matches(rule: Mapping[str, Any], platform_: Platform) -> bool:
    os_rule = rule.get("os") or {}
    if "name" in os_rule and os_rule["name"] != platform_.os_name:
        return False
    if "arch" in os_rule and os_rule["arch"] != platform_.arch:
        return False
    if "versionRange" in os_rule and not _in_version_range(
        platform_.os_version, os_rule["versionRange"]
    ):
        return False
    for name, expected in (rule.get("features") or {}).items():
        if bool(platform_.features.get(name, False)) != bool(expected):
            return False
    return True


def _in_version_range(value: str, spec: Mapping[str, str]) -> bool:
    """Inclusive ``min``, exclusive ``max`` on dotted numeric versions."""
    current = _numeric_version(value)
    if not current:
        return False
    low = spec.get("min")
    high = spec.get("max")
    if low is not None and _compare_versions(current, _numeric_version(low)) < 0:
        return False
    if high is not None and _compare_versions(current, _numeric_version(high)) >= 0:
        return False
    return True


def _numeric_version(value: str) -> tuple[int, ...]:
    return tuple(int(part) for part in re.findall(r"\d+", value))


def _compare_versions(left: tuple[int, ...], right: tuple[int, ...]) -> int:
    width = max(len(left), len(right))
    padded_left = left + (0,) * (width - len(left))
    padded_right = right + (0,) * (width - len(right))
    return (padded_left > padded_right) - (padded_left < padded_right)


# -- the version json -------------------------------------------------------


def load_version(paths: ClientPaths) -> dict:
    """Read and sanity-check the version json of the client on disk."""
    path = paths.version_json
    if not path.is_file():
        raise LaunchError(f"client version json not found: {path}")
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise LaunchError(f"cannot read {path}: {error}") from error
    for key in ("id", "mainClass", "libraries", "arguments", "javaVersion", "assetIndex"):
        if key not in data:
            raise LaunchError(f"{path} has no {key!r}")
    return data


def expected_java_major(version: Mapping[str, Any]) -> int:
    java_version = version.get("javaVersion") or {}
    try:
        return int(java_version["majorVersion"])
    except (KeyError, TypeError, ValueError) as error:
        raise LaunchError(f"version json has no usable javaVersion.majorVersion: {error}") from error


def java_major_version(java: Path, *, run: Callable[..., Any] = subprocess.run) -> int:
    """The major version of the given java binary, parsed from ``java -version``."""
    try:
        result = run([str(java), "-version"], capture_output=True, text=True, timeout=60)
    except OSError as error:
        raise LaunchError(f"cannot run {java}: {error}") from error
    text = f"{result.stdout or ''}{result.stderr or ''}"
    match = re.search(r'version "([^"]+)"', text)
    if not match:
        raise LaunchError(f"cannot parse the output of {java} -version: {text.strip()!r}")
    number = re.match(r"(\d+)(?:\.(\d+))?", match.group(1))
    if not number:
        raise LaunchError(f"cannot parse the java version {match.group(1)!r}")
    if number.group(1) == "1":  # legacy 1.8.0_392
        return int(number.group(2) or 0)
    return int(number.group(1))


# -- classpath and arguments ------------------------------------------------


def build_classpath(version: Mapping[str, Any], paths: ClientPaths, platform_: Platform) -> list[Path]:
    """Every allowed library jar in json order, then the version jar."""
    entries: list[Path] = []
    for library in version["libraries"]:
        if not rule_allows(library.get("rules"), platform_):
            continue
        artifact = (library.get("downloads") or {}).get("artifact") or {}
        relative = artifact.get("path")
        if not relative:
            continue
        jar = paths.libraries / relative
        if not jar.is_file():
            raise LaunchError(f"missing client library {library.get('name', relative)}: {jar}")
        entries.append(jar)
    jar = paths.version_jar
    if not jar.is_file():
        raise LaunchError(f"missing client jar: {jar}")
    entries.append(jar)
    return entries


def _argument_values(value: Any) -> list[str]:
    """Flatten a json argument entry (string or nested list of strings)."""
    if isinstance(value, str):
        return [value]
    if isinstance(value, list):
        flat: list[str] = []
        for item in value:
            flat.extend(_argument_values(item))
        return flat
    raise LaunchError(f"unexpected argument entry in the version json: {value!r}")


def evaluate_arguments(
    entries: Sequence[Any], platform_: Platform, *, where: str = "arguments"
) -> list[str]:
    """The arguments whose rules allow them, flattened and in json order."""
    arguments: list[str] = []
    for entry in entries:
        if isinstance(entry, str):
            arguments.append(entry)
            continue
        if not isinstance(entry, Mapping):
            raise LaunchError(f"unexpected {where} entry in the version json: {entry!r}")
        if rule_allows(entry.get("rules"), platform_):
            arguments.extend(_argument_values(entry.get("value")))
    return arguments


def substitute(arguments: Sequence[str], values: Mapping[str, str]) -> list[str]:
    """Replace every ``${name}``; an unknown placeholder is an error."""
    return [_substitute_argument(argument, values) for argument in arguments]


def _substitute_argument(argument: str, values: Mapping[str, str]) -> str:
    def replace(match: re.Match[str]) -> str:
        name = match.group(1)
        if name not in values:
            raise LaunchError(f"unsubstituted placeholder ${{{name}}} in argument {argument!r}")
        return values[name]

    return _PLACEHOLDER.sub(replace, argument)


def offline_uuid(name: str) -> str:
    """The UUID the offline-mode server gives ``name``, as hex without dashes.

    Vanilla uses ``UUID.nameUUIDFromBytes("OfflinePlayer:" + name)``: an MD5
    name-based (version 3) UUID.
    """
    digest = bytearray(hashlib.md5(f"OfflinePlayer:{name}".encode("utf-8")).digest())
    digest[6] = (digest[6] & 0x0F) | 0x30
    digest[8] = (digest[8] & 0x3F) | 0x80
    return uuid.UUID(bytes=bytes(digest)).hex


def build_command(
    *,
    java: Path,
    version: Mapping[str, Any],
    paths: ClientPaths,
    name: str,
    host: str,
    port: int,
    platform_: Platform | None = None,
) -> list[str]:
    """The full java command line for the client, ready for ``subprocess``."""
    platform_ = platform_ or Platform.current()
    classpath = build_classpath(version, paths, platform_)
    classpath_text = os.pathsep.join(str(entry) for entry in classpath)
    address = f"{host}:{port}"

    jvm_values = {
        "natives_directory": str(paths.natives),
        "launcher_name": LAUNCHER_NAME,
        "launcher_version": LAUNCHER_VERSION,
        "classpath": classpath_text,
        "library_directory": str(paths.libraries),
        "classpath_separator": os.pathsep,
    }
    game_values = {
        "auth_player_name": name,
        "version_name": VERSION_ID,
        "game_directory": str(paths.game),
        "assets_root": str(paths.assets),
        "assets_index_name": str(version["assetIndex"]["id"]),
        "auth_uuid": offline_uuid(name),
        "auth_access_token": "0",
        "clientid": "0",
        "auth_xuid": "0",
        "version_type": "release",
        "quickPlayMultiplayer": address,
        "quickPlayPath": str(paths.game / "quickplay.json"),
    }

    arguments = version.get("arguments") or {}
    jvm_arguments = evaluate_arguments(arguments.get("jvm") or [], platform_, where="jvm arguments")
    if "default-user-jvm" in arguments:
        jvm_arguments.extend(
            evaluate_arguments(
                arguments["default-user-jvm"], platform_, where="default-user-jvm arguments"
            )
        )
    game_arguments = evaluate_arguments(arguments.get("game") or [], platform_, where="game arguments")

    return [
        str(java),
        *substitute(jvm_arguments, jvm_values),
        str(version["mainClass"]),
        *substitute(game_arguments, game_values),
    ]


# -- launching --------------------------------------------------------------


def client_command(
    *,
    name: str,
    host: str,
    port: int,
    paths: ClientPaths | None = None,
    version: dict | None = None,
    java: Path | None = None,
    platform_: Platform | None = None,
) -> tuple[list[str], ClientPaths]:
    """The full client command and the paths it was built for.

    Resolves and checks the machine-local client exactly as the CLI always did
    (version json, Java major version), then builds the argument list without
    starting anything. ``launch_client`` and the dry run share it.
    """
    paths = paths or ClientPaths(repo_root() / CLIENT_ROOT)
    version = version or load_version(paths)
    java = java or resolve_java()
    expected = expected_java_major(version)
    found = java_major_version(java)
    if found != expected:
        raise LaunchError(
            f"{java} is Java {found} but client {VERSION_ID} needs Java {expected}; "
            f"fix 'java' in {repo_root() / 'local' / 'paths.toml'}"
        )
    command = build_command(
        java=java,
        version=version,
        paths=paths,
        name=name,
        host=host,
        port=port,
        platform_=platform_,
    )
    return command, paths


def spawn_client(command: Sequence[str], paths: ClientPaths) -> subprocess.Popen:
    """Start one already prepared client command; the caller keeps the process.

    Split from :func:`launch_client` so a caller that must not hold its own
    lock during the slow preparation can revalidate its state after
    :func:`client_command` and only then spawn.
    """
    try:
        return subprocess.Popen(
            list(command),
            cwd=paths.game,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
    except OSError as error:
        raise LaunchError(f"cannot start the Minecraft client: {error}") from error


def launch_client(
    *,
    host: str = "127.0.0.1",
    port: int,
    name: str = "Watcher",
    paths: ClientPaths | None = None,
    version: dict | None = None,
    java: Path | None = None,
    platform_: Platform | None = None,
) -> subprocess.Popen:
    """Start the local client on ``host:port`` and return the running process.

    The owner keeps the returned process (it is the player's client: never kill
    it when a session stops). Its output is discarded and it runs detached from
    the caller's terminal, exactly like the CLI's launcher. No argument or
    placeholder differs from :func:`build_command`.
    """
    command, paths = client_command(
        name=name,
        host=host,
        port=port,
        paths=paths,
        version=version,
        java=java,
        platform_=platform_,
    )
    return spawn_client(command, paths)


# -- cli --------------------------------------------------------------------


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--port", type=int, required=True, help="the harness server port")
    parser.add_argument("--host", default="127.0.0.1", help="the harness server host")
    parser.add_argument("--name", default="Watcher", help="the player name to join as")
    parser.add_argument(
        "--dry-run", action="store_true", help="print the command and exit without starting java"
    )
    args = parser.parse_args(argv)

    paths = ClientPaths(repo_root() / CLIENT_ROOT)
    try:
        if args.dry_run:
            command, _ = client_command(
                name=args.name,
                host=args.host,
                port=args.port,
                paths=paths,
            )
            print("\n".join(command), flush=True)
            return 0
        process = launch_client(
            name=args.name,
            host=args.host,
            port=args.port,
            paths=paths,
        )
    except (LaunchError, PathResolutionError) as error:
        print(f"error: {error}", file=sys.stderr, flush=True)
        return 2

    print(f"client pid {process.pid}", flush=True)
    print(f"log {paths.log}", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())


__all__ = [
    "CLIENT_ROOT",
    "ClientPaths",
    "DEFAULT_FEATURES",
    "LaunchError",
    "Platform",
    "VERSION_ID",
    "build_classpath",
    "build_command",
    "client_command",
    "evaluate_arguments",
    "expected_java_major",
    "java_major_version",
    "launch_client",
    "load_version",
    "main",
    "offline_uuid",
    "rule_allows",
    "spawn_client",
    "substitute",
]

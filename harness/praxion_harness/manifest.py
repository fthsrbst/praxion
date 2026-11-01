"""Typed reader for the repository version manifest, `versions/manifest.toml`.

The manifest is the one place every pinned version, hash and URL lives; this module is the harness's
only reader of it. Every key is required and type-checked, so a missing key or a value of the wrong
type fails with an error that names the file and the key. Mirrors `body/crates/manifest`
(crate `praxion-manifest`).
"""

from __future__ import annotations

import tomllib
from dataclasses import dataclass
from pathlib import Path

# Manifest path, relative to the repository root.
MANIFEST_RELATIVE = Path("versions") / "manifest.toml"


class ManifestError(Exception):
    """The manifest cannot be read, is not TOML, or has a missing key or a value of the wrong type."""


@dataclass(frozen=True)
class Minecraft:
    """The pinned Minecraft version, its protocol number and the two jar hashes."""

    # Version id as Mojang names it, for example "26.3".
    version: str
    # Protocol number the forwarder accepts and nothing else.
    protocol: int
    # SHA-1 of the server jar.
    server_jar_sha1: str
    # Size of the server jar in bytes.
    server_jar_size: int
    # SHA-1 of the client jar the launcher installs.
    client_jar_sha1: str
    # Mojang's version manifest index, the first hop of the server jar download.
    version_manifest_url: str


@dataclass(frozen=True)
class Azalea:
    """The pinned Azalea source and the patches the spikes apply to it."""

    # Git URL the spike clone comes from.
    repo: str
    # Commit the clone is checked out at.
    commit: str
    # Every patch matching this glob is applied in sorted order; relative to the repository root.
    patches_glob: str


@dataclass(frozen=True)
class Rust:
    """The pinned Rust toolchain and host."""

    # rustup channel, for example "nightly-2026-08-20".
    toolchain: str
    # Host triple; Windows has no MSVC, hence the GNU target.
    windows_target: str
    # rustup-init download URL; {triple} and {suffix} are filled in by the caller.
    rustup_dist_url: str


@dataclass(frozen=True)
class Manifest:
    """Everything `versions/manifest.toml` pins."""

    minecraft: Minecraft
    azalea: Azalea
    rust: Rust


# section -> ((key, type), ...). Every key listed here is required and must have exactly this type.
SCHEMA = {
    "minecraft": (
        ("version", str),
        ("protocol", int),
        ("server_jar_sha1", str),
        ("server_jar_size", int),
        ("client_jar_sha1", str),
        ("version_manifest_url", str),
    ),
    "azalea": (
        ("repo", str),
        ("commit", str),
        ("patches_glob", str),
    ),
    "rust": (
        ("toolchain", str),
        ("windows_target", str),
        ("rustup_dist_url", str),
    ),
}


def type_name(value) -> str:
    """The TOML type name of a parsed value (`bool` is not an `int` here)."""
    return "bool" if isinstance(value, bool) else type(value).__name__


def checked_tables(data: dict, path: Path) -> dict[str, dict]:
    """The `[section]` tables of a parsed manifest, or ManifestError naming the first problem."""
    tables: dict[str, dict] = {}
    for section, fields in SCHEMA.items():
        table = data.get(section)
        if not isinstance(table, dict):
            raise ManifestError(f"{path}: missing [{section}] table")
        for key, expected in fields:
            if key not in table:
                raise ManifestError(f"{path}: missing key {section}.{key}")
            value = table[key]
            if isinstance(value, bool) or not isinstance(value, expected):
                raise ManifestError(
                    f"{path}: {section}.{key} must be {expected.__name__}, found {type_name(value)}"
                )
        tables[section] = table
    return tables


def default_path() -> Path:
    """The manifest of this checkout: walk up from this file until a directory holds one."""
    start = Path(__file__).resolve().parent
    for directory in (start, *start.parents):
        candidate = directory / MANIFEST_RELATIVE
        if candidate.is_file():
            return candidate
    raise ManifestError(f"no {MANIFEST_RELATIVE} in {start} or any parent directory")


def load(path: Path | str | None = None) -> Manifest:
    """Read and validate the manifest at `path`, or this checkout's manifest when path is None."""
    target = Path(path) if path is not None else default_path()
    try:
        text = target.read_text(encoding="utf-8")
    except OSError as exc:
        raise ManifestError(f"cannot read {target}: {exc}") from exc
    try:
        data = tomllib.loads(text)
    except tomllib.TOMLDecodeError as exc:
        raise ManifestError(f"invalid TOML in {target}: {exc}") from exc

    tables = checked_tables(data, target)
    minecraft = tables["minecraft"]
    azalea = tables["azalea"]
    rust = tables["rust"]
    return Manifest(
        minecraft=Minecraft(
            version=minecraft["version"],
            protocol=minecraft["protocol"],
            server_jar_sha1=minecraft["server_jar_sha1"],
            server_jar_size=minecraft["server_jar_size"],
            client_jar_sha1=minecraft["client_jar_sha1"],
            version_manifest_url=minecraft["version_manifest_url"],
        ),
        azalea=Azalea(
            repo=azalea["repo"],
            commit=azalea["commit"],
            patches_glob=azalea["patches_glob"],
        ),
        rust=Rust(
            toolchain=rust["toolchain"],
            windows_target=rust["windows_target"],
            rustup_dist_url=rust["rustup_dist_url"],
        ),
    )

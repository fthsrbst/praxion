#!/usr/bin/env python3
"""One-command, idempotent setup for the session 2.7 experiment spikes.

Run from anywhere:  python spikes/setup.py [--check] [--no-build] [--test] [--accept-eula] [--java PATH]

Every step checks first and skips with an "ok: ..." line when it is already done, so a second
run on a finished machine changes nothing. --check only reports and changes nothing.

Never does: write eula=true without --accept-eula, download the Minecraft client, start the game
or the server, or change system settings. See spikes/SETUP.md.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import re
import shutil
import subprocess
import sys
import urllib.request
from dataclasses import dataclass
from pathlib import Path

MIN_PYTHON = (3, 11)
if sys.version_info < MIN_PYTHON:
    sys.stderr.write(
        "setup.py needs Python %d.%d or newer (found %s). Install Python and rerun.\n"
        % (MIN_PYTHON[0], MIN_PYTHON[1], platform.python_version())
    )
    raise SystemExit(2)

import tomllib  # Python 3.11+

# --------------------------------------------------------------------------- errors


class SetupError(Exception):
    """A step cannot be completed; reported as a failed step, the run continues."""


# --------------------------------------------------------------------------- repository root

SPIKES = Path(__file__).resolve().parent
REPO = SPIKES.parent


def rel(path: Path) -> str:
    """Path relative to the repository root, or the absolute path when it lies outside."""
    try:
        return path.resolve().relative_to(REPO).as_posix()
    except ValueError:
        return str(path)


# --------------------------------------------------------------------------- version manifest
#
# Every pinned version, hash and URL lives in versions/manifest.toml: the one pin source, shared
# with body/ (praxion-manifest) and harness/ (praxion_harness). Nothing pinned is written again
# here, and each value's source is the comment above it in that file. MANIFEST_SCHEMA lists what
# this script reads and the type it must have, so a broken edit fails at startup, not mid-run.

MANIFEST_PATH = REPO / "versions" / "manifest.toml"

MANIFEST_SCHEMA = {
    "minecraft": (
        ("version", str),
        ("protocol", int),
        ("server_jar_sha1", str),
        ("server_jar_size", int),
        ("client_jar_sha1", str),
        ("version_manifest_url", str),
    ),
    "azalea": (("repo", str), ("commit", str), ("patches_glob", str)),
    "rust": (("toolchain", str), ("windows_target", str), ("rustup_dist_url", str)),
}


def type_name(value) -> str:
    return "bool" if isinstance(value, bool) else type(value).__name__


def load_manifest() -> dict:
    """versions/manifest.toml, parsed and checked against MANIFEST_SCHEMA."""
    try:
        data = tomllib.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
    except OSError as exc:
        raise SetupError(f"cannot read {rel(MANIFEST_PATH)}: {exc}") from exc
    except tomllib.TOMLDecodeError as exc:
        raise SetupError(f"invalid TOML in {rel(MANIFEST_PATH)}: {exc}") from exc

    for section, fields in MANIFEST_SCHEMA.items():
        table = data.get(section)
        if not isinstance(table, dict):
            raise SetupError(f"{rel(MANIFEST_PATH)} has no [{section}] table")
        for key, expected in fields:
            if key not in table:
                raise SetupError(f"{rel(MANIFEST_PATH)} is missing {section}.{key}")
            value = table[key]
            if isinstance(value, bool) or not isinstance(value, expected):
                raise SetupError(
                    f"{rel(MANIFEST_PATH)}: {section}.{key} must be {expected.__name__}, "
                    f"found {type_name(value)}"
                )
    return data


try:
    MANIFEST = load_manifest()
except SetupError as exc:  # nothing below can run without the pins
    sys.stderr.write(f"spikes/setup.py: {exc}\n")
    raise SystemExit(2)
MINECRAFT = MANIFEST["minecraft"]
AZALEA = MANIFEST["azalea"]
RUST_PIN = MANIFEST["rust"]

# --------------------------------------------------------------------------- paths and constants

CARGO_BIN = Path.home() / ".cargo" / "bin"
TOOLCHAIN_DIR = SPIKES / ".toolchain"
SPIKES_CARGO_CONFIG = SPIKES / ".cargo" / "config.toml"
BODY_CARGO_CONFIG = REPO / "body" / ".cargo" / "config.toml"
AZALEA_DIR = SPIKES / "azalea"
ROUND0_DIR = SPIKES / "round0"
SERVER_DIR = ROUND0_DIR / "server"
SERVER_JAR = ROUND0_DIR / f"server-{MINECRAFT['version']}.jar"

IS_WINDOWS = os.name == "nt"
IS_MACOS = sys.platform == "darwin"

OK, DONE, SKIP, WARN, MISSING, WRONG, FAIL = "ok", "done", "skip", "warn", "missing", "wrong", "fail"
FAIL_STATUSES = {MISSING, WRONG, FAIL}
STATUS_LABEL = {OK: "ok", DONE: "done", SKIP: "skip", WARN: "warn", MISSING: "missing", WRONG: "wrong", FAIL: "fail"}
# When several sub-results make one step, the worst one wins.
STATUS_ORDER = (FAIL, WRONG, MISSING, DONE, WARN, OK)

# The comment block below is the explanatory text of the generated `.cargo/config.toml` of a Rust
# workspace (`spikes/` or `body/`), kept verbatim apart from `{scope}`, `{linker}` and `{dlltool}`.
CARGO_CONFIG_TEMPLATE = """\
# Toolchain fix for every crate under {scope} (pinned host x86_64-pc-windows-gnu, self-contained mingw,
# no MinGW binutils installed). Two problems, two settings:
#
# 1. dlltool. Crates that link through `raw-dylib` (windows-sys 0.61, getrandom 0.4) need rustc to
#    generate import libraries with a dlltool; the sysroot copy needs an assembler that is not installed.
#    LLVM's dlltool is the same binary as llvm-ar (it picks its mode from its file name), so
#    `spikes/.toolchain/llvm-dlltool.exe` is a copy of `llvm-ar.exe` from the rustup `llvm-tools` component.
# 2. linker. binutils' ld cannot read import libraries made by LLVM: it links, then the EXE dies with
#    STATUS_ACCESS_VIOLATION before `main` (rustc says so in `back/archive.rs`). Linking with rust-lld in
#    its gnu flavour fixes it.
#
# Generated by spikes/setup.py (machine-specific, not committed); rerun it to recreate this file and
# the dlltool copy. rustc resolves both paths from each crate's build directory, so they are absolute.
[target.x86_64-pc-windows-gnu]
linker = "{linker}"
rustflags = [
    "-C", "dlltool={dlltool}",
    "-Z", "unstable-options",
    "-C", "linker-flavor=gnu-lld",
]
"""

EULA_URL = "https://aka.ms/MinecraftEULA"

# --------------------------------------------------------------------------- setup-owned values
#
# Values the setup itself owns. Nothing here is a version pin: every pin is in versions/manifest.toml.
PINS = {
    # server.properties keys this setup owns. Every other key is left to the server defaults.
    "server_properties": {
        "online-mode": "false",
        "server-ip": "127.0.0.1",
        "server-port": "25566",
        "level-seed": "praxion",
        "spawn-protection": "0",
        "enable-rcon": "true",
        "rcon.port": "25575",
        "rcon.password": "praxion-local",
        "white-list": "false",
        "enforce-whitelist": "false",
        "accepts-transfers": "true",
        "difficulty": "peaceful",
    },
}


# --------------------------------------------------------------------------- small helpers


def one_line(text: str) -> str:
    return " ".join(str(text).split())


def sha1_of(path: Path) -> str:
    digest = hashlib.sha1()
    with path.open("rb") as fh:
        for block in iter(lambda: fh.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def sha256_of(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as fh:
        for block in iter(lambda: fh.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def fmt_cmd(cmd) -> str:
    return " ".join(f'"{part}"' if " " in str(part) else str(part) for part in cmd)


def capture(cmd, cwd: Path | None = None) -> subprocess.CompletedProcess:
    return subprocess.run(
        [str(part) for part in cmd],
        cwd=str(cwd) if cwd else None,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
    )


def run_stream(cmd, cwd: Path | None = None) -> int:
    print(f"  $ {fmt_cmd(cmd)}" + (f"   ({rel(cwd)})" if cwd else ""), flush=True)
    return subprocess.run([str(part) for part in cmd], cwd=str(cwd) if cwd else None).returncode


def must_run(cmd, what: str, cwd: Path | None = None) -> None:
    code = run_stream(cmd, cwd)
    if code != 0:
        raise SetupError(f"{what} failed (exit {code}); see the output above")


def tool(name: str) -> Path | None:
    """Executable on PATH, else in ~/.cargo/bin (rustup's bin dir is often not on PATH)."""
    found = shutil.which(name)
    if found:
        return Path(found)
    for suffix in ("", ".exe"):
        candidate = CARGO_BIN / (name + suffix)
        if candidate.exists():
            return candidate
    return None


def ensure_cargo_bin_on_path() -> None:
    if not CARGO_BIN.is_dir():
        return
    parts = os.environ.get("PATH", "").split(os.pathsep)
    if str(CARGO_BIN) not in parts:
        os.environ["PATH"] = str(CARGO_BIN) + os.pathsep + os.environ.get("PATH", "")


def download(url: str, dest: Path, expected_size: int | None = None) -> Path:
    """Download url to a temporary file next to dest and return that temporary path."""
    dest.parent.mkdir(parents=True, exist_ok=True)
    tmp = dest.with_name(f"{dest.name}.{os.getpid()}.part")
    print(f"  downloading {url}", flush=True)
    request = urllib.request.Request(url, headers={"User-Agent": "praxion-spikes-setup/1"})
    try:
        with urllib.request.urlopen(request, timeout=60) as response, tmp.open("wb") as fh:
            total = 0
            printed = 0
            while True:
                chunk = response.read(1 << 20)
                if not chunk:
                    break
                fh.write(chunk)
                total += len(chunk)
                if expected_size:
                    percent = min(100, total * 100 // expected_size)
                    if percent >= printed + 20:
                        printed = percent - percent % 20
                        print(f"    {percent}% ({total} of {expected_size} bytes)", flush=True)
    except BaseException:
        tmp.unlink(missing_ok=True)
        raise
    print(f"    {total} bytes", flush=True)
    return tmp


def fetch_json(url: str) -> dict:
    print(f"  fetching {url}", flush=True)
    request = urllib.request.Request(url, headers={"User-Agent": "praxion-spikes-setup/1"})
    with urllib.request.urlopen(request, timeout=60) as response:
        return json.loads(response.read().decode("utf-8"))


# --------------------------------------------------------------------------- git helpers


def git_out(args, cwd: Path, allow_fail: bool = False) -> subprocess.CompletedProcess:
    result = capture(["git", *args], cwd)
    if result.returncode != 0 and not allow_fail:
        raise SetupError(f"git {' '.join(str(a) for a in args)} failed in {rel(cwd)}: {result.stderr.strip()}")
    return result


def worktree_changes(repo: Path) -> set[str]:
    """Paths with uncommitted (staged, unstaged or untracked) changes, relative and forward-slash."""
    data = git_out(["status", "--porcelain=v1", "-z"], repo).stdout
    fields = data.split("\0")
    paths: set[str] = set()
    index = 0
    while index < len(fields):
        entry = fields[index]
        index += 1
        if not entry:
            continue
        code = entry[:2]
        paths.add(entry[3:].replace("\\", "/"))
        if "R" in code or "C" in code:
            if index < len(fields) and fields[index]:
                paths.add(fields[index].replace("\\", "/"))
                index += 1
    return paths


def patch_paths(patch: Path, repo: Path) -> set[str]:
    """Files a patch touches, as printed by `git apply --numstat` run inside the clone."""
    result = git_out(["apply", "--numstat", str(patch)], repo)
    paths = set()
    for line in result.stdout.splitlines():
        parts = line.split("\t")
        if len(parts) >= 3 and parts[2]:
            paths.add(parts[2].replace("\\", "/"))
    return paths


# --------------------------------------------------------------------------- java helpers


def java_version(java: Path) -> tuple[int | None, str]:
    """(major version, first line of `java -version`) or (None, "") when it cannot run."""
    try:
        result = capture([java, "-version"])
    except OSError:
        return None, ""
    output = (result.stderr or "") + (result.stdout or "")
    first = next((line.strip() for line in output.splitlines() if line.strip()), "")
    match = re.search(r'version "(\d+)(?:\.(\d+))?', output)
    if not match:
        return None, first
    major = int(match.group(1))
    if major == 1 and match.group(2):
        major = int(match.group(2))  # pre-9 version strings ("1.8.0_402")
    return major, first


def minecraft_dir() -> Path:
    if IS_WINDOWS:
        appdata = os.environ.get("APPDATA")
        base = Path(appdata) if appdata else Path.home() / "AppData" / "Roaming"
        return base / ".minecraft"
    if IS_MACOS:
        return Path.home() / "Library" / "Application Support" / "minecraft"
    return Path.home() / ".minecraft"


def launcher_java() -> Path | None:
    runtime = minecraft_dir() / "runtime" / "java-runtime-epsilon"
    if IS_WINDOWS:
        java = runtime / "windows" / "java-runtime-epsilon" / "bin" / "java.exe"
        return java if java.is_file() else None
    if runtime.is_dir():
        for candidate in sorted(runtime.rglob("java")):
            if candidate.is_file() and candidate.parent.name == "bin":
                return candidate
    return None


# --------------------------------------------------------------------------- rust chain


def rust_toolchain_channel(path: Path) -> str:
    """The channel a rust-toolchain.toml declares."""
    try:
        data = tomllib.loads(path.read_text(encoding="utf-8"))
    except OSError as exc:
        raise SetupError(f"cannot read {rel(path)}: {exc}") from exc
    except tomllib.TOMLDecodeError as exc:
        raise SetupError(f"invalid TOML in {rel(path)}: {exc}") from exc
    channel = data.get("toolchain", {}).get("channel")
    if not isinstance(channel, str) or not channel:
        raise SetupError(f"{rel(path)} has no toolchain.channel")
    return channel


def pinned_toolchain() -> str:
    """The manifest's channel, after checking that both workspaces declare the same one."""
    channel = RUST_PIN["toolchain"]
    for path in (SPIKES / "rust-toolchain.toml", REPO / "body" / "rust-toolchain.toml"):
        if not path.is_file():
            continue
        declared = rust_toolchain_channel(path)
        if declared != channel:
            raise SetupError(
                f"{rel(path)} pins {declared} but {rel(MANIFEST_PATH)} pins {channel}; "
                f"set rust.toolchain in the manifest and both rust-toolchain.toml files to one channel"
            )
    return channel


def rustup_triple() -> str:
    if IS_WINDOWS:
        return RUST_PIN["windows_target"]
    machine = platform.machine().lower()
    return "aarch64-apple-darwin" if machine in ("arm64", "aarch64") else "x86_64-apple-darwin"


def toolchain_installed(rustup: Path, channel: str) -> bool:
    return capture([rustup, "run", channel, "rustc", "--version"]).returncode == 0


def component_installed(rustup: Path, channel: str, name: str) -> bool:
    result = capture([rustup, "component", "list", "--toolchain", channel, "--installed"])
    if result.returncode != 0:
        return False
    for line in result.stdout.splitlines():
        first = line.split()[0] if line.split() else ""
        if first == name or first.startswith(name + "-"):
            return True
    return False


def install_rustup() -> Path:
    suffix = ".exe" if IS_WINDOWS else ""
    url = RUST_PIN["rustup_dist_url"].format(triple=rustup_triple(), suffix=suffix)
    target = TOOLCHAIN_DIR / f"rustup-init{suffix}"
    created_dir = not TOOLCHAIN_DIR.exists()
    tmp = download(url, target)
    os.replace(tmp, target)
    cmd = [target, "-y", "--profile", "minimal", "--default-toolchain", "none"]
    if IS_WINDOWS:
        cmd += ["--default-host", RUST_PIN["windows_target"]]
    try:
        must_run(cmd, "rustup-init")
    finally:
        target.unlink(missing_ok=True)
        if created_dir:
            try:
                TOOLCHAIN_DIR.rmdir()  # only succeeds when nothing else lives there
            except OSError:
                pass
    ensure_cargo_bin_on_path()
    rustup = tool("rustup")
    if rustup is None:
        raise SetupError(f"rustup-init finished but no rustup found on PATH or in {rel(CARGO_BIN)}")
    return rustup


def pinned_sysroot(ctx: "Ctx") -> Path:
    if ctx.sysroot is not None:
        return ctx.sysroot
    if ctx.channel is None:
        raise SetupError("the pinned toolchain is not known yet (step 2)")
    rustc = tool("rustc")
    if rustc is None:
        raise SetupError("rustc not found after the Rust step")
    result = capture([rustc, f"+{ctx.channel}", "--print", "sysroot"])
    if result.returncode != 0:
        raise SetupError(f"rustc +{ctx.channel} --print sysroot failed: {result.stderr.strip()}")
    ctx.sysroot = Path(result.stdout.strip())
    return ctx.sysroot


def cargo_build(ctx: "Ctx", args, cwd: Path, what: str) -> None:
    cargo = tool("cargo")
    if cargo is None:
        raise SetupError("cargo not found; rerun without --no-build to install the toolchain")
    must_run([cargo, f"+{ctx.channel}", *args], what, cwd)


# --------------------------------------------------------------------------- properties files


def split_property(line: str) -> tuple[str, str] | None:
    """(key, value) for a java .properties line, None for blanks and comments."""
    stripped = line.lstrip()
    if not stripped or stripped[0] in "#!":
        return None
    for index, char in enumerate(stripped):
        if char in "=:" or char.isspace():
            key = stripped[:index]
            rest = stripped[index:].lstrip()
            if rest[:1] in ("=", ":"):
                rest = rest[1:].lstrip()
            return key, rest.rstrip()
    return stripped, ""


def render_properties(raw: str, desired: dict) -> str:
    """Keep every other line, force the desired keys (once each) and append the ones missing."""
    lines = raw.splitlines() if raw else []
    emitted: set[str] = set()
    out: list[str] = []
    for line in lines:
        pair = split_property(line)
        if pair is None:
            out.append(line)
            continue
        key = pair[0]
        if key in desired:
            if key in emitted:
                continue
            out.append(f"{key}={desired[key]}")
            emitted.add(key)
        else:
            out.append(line)
    for key, value in desired.items():
        if key not in emitted:
            out.append(f"{key}={value}")
    return "\n".join(out) + "\n"


def properties_report(raw: str, desired: dict) -> tuple[list[str], list[str]]:
    found: dict[str, str] = {}
    for line in raw.splitlines():
        pair = split_property(line)
        if pair and pair[0] in desired:
            found[pair[0]] = pair[1]
    missing = [key for key in desired if key not in found]
    wrong = [key for key in desired if key in found and found[key] != desired[key]]
    return missing, wrong


# --------------------------------------------------------------------------- step plumbing


@dataclass
class Options:
    check: bool = False
    no_build: bool = False
    test: bool = False
    accept_eula: bool = False
    java: str | None = None


@dataclass
class Ctx:
    opts: Options
    channel: str | None = None
    rustup: Path | None = None
    sysroot: Path | None = None
    toolchain_ready: bool = False


@dataclass
class Step:
    name: str
    status: str
    detail: str


def combine(statuses) -> str:
    for status in STATUS_ORDER:
        if status in statuses:
            return status
    return OK


# --------------------------------------------------------------------------- steps


def step_prerequisites(ctx: Ctx) -> Step:
    git = shutil.which("git")
    if git is None:
        raise SetupError(
            "git not found on PATH. Install Git for Windows (https://git-scm.com/download/win), "
            "reopen Git Bash and rerun."
        )
    version = capture(["git", "--version"]).stdout.strip() or "git"
    return Step(
        "prerequisites",
        OK,
        f"python {platform.python_version()}; {version} ({git})",
    )


def step_rust(ctx: Ctx) -> Step:
    channel = pinned_toolchain()
    ctx.channel = channel
    ensure_cargo_bin_on_path()
    changes: list[str] = []

    rustup = tool("rustup")
    if rustup is None:
        if ctx.opts.check:
            return Step(
                "rust toolchain",
                MISSING,
                f"rustup not found (PATH or {rel(CARGO_BIN)}); would download rustup-init and install {channel}",
            )
        rustup = install_rustup()
        changes.append("installed rustup")
    ctx.rustup = rustup

    if not toolchain_installed(rustup, channel):
        if ctx.opts.check:
            return Step(
                "rust toolchain",
                MISSING,
                f"toolchain {channel} not installed (would run: rustup toolchain install {channel} --profile minimal --component llvm-tools)",
            )
        must_run(
            [rustup, "toolchain", "install", channel, "--profile", "minimal", "--component", "llvm-tools"],
            f"rustup toolchain install {channel}",
        )
        changes.append(f"installed {channel}")
    elif not component_installed(rustup, channel, "llvm-tools"):
        if ctx.opts.check:
            return Step("rust toolchain", MISSING, f"toolchain {channel} lacks the llvm-tools component")
        must_run(
            [rustup, "component", "add", "llvm-tools", "--toolchain", channel],
            "rustup component add llvm-tools",
        )
        changes.append("added llvm-tools")

    if not toolchain_installed(rustup, channel):  # install finished but rustc still will not run
        raise SetupError(f"toolchain {channel} is installed but rustc does not run; run: rustup toolchain install {channel}")
    ctx.toolchain_ready = True
    detail = f"{rel(rustup)}; toolchain {channel} with llvm-tools"
    if changes:
        return Step("rust toolchain", DONE, detail + " (" + ", ".join(changes) + ")")
    return Step("rust toolchain", OK, detail)


def step_dlltool(ctx: Ctx) -> Step:
    if not IS_WINDOWS:
        return Step("dlltool shim", SKIP, "not needed on macOS")
    if not ctx.toolchain_ready:
        return Step("dlltool shim", MISSING, "pinned toolchain not installed (step 2)")
    bin_dir = pinned_sysroot(ctx) / "lib" / "rustlib" / RUST_PIN["windows_target"] / "bin"
    source = bin_dir / "llvm-ar.exe"
    target = TOOLCHAIN_DIR / "llvm-dlltool.exe"
    if not source.is_file():
        raise SetupError(f"{rel(source)} missing; run: rustup component add llvm-tools --toolchain {ctx.channel}")

    if target.is_file() and target.stat().st_size == source.stat().st_size and sha256_of(target) == sha256_of(source):
        return Step("dlltool shim", OK, f"{rel(target)} is a copy of llvm-ar.exe")

    if ctx.opts.check:
        status = WRONG if target.exists() else MISSING
        return Step("dlltool shim", status, f"would copy {rel(source)} to {rel(target)}")

    TOOLCHAIN_DIR.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source, target)
    if sha256_of(target) != sha256_of(source):
        raise SetupError(f"{rel(target)} does not match {rel(source)} after copying")
    return Step("dlltool shim", DONE, f"copied llvm-ar.exe to {rel(target)}")


def cargo_config_text(ctx: Ctx, scope: str) -> str:
    """The `.cargo/config.toml` body for the workspace at scope ("spikes/" or "body/")."""
    bin_dir = pinned_sysroot(ctx) / "lib" / "rustlib" / RUST_PIN["windows_target"] / "bin"
    return CARGO_CONFIG_TEMPLATE.format(
        scope=scope,
        linker=(bin_dir / "rust-lld.exe").as_posix(),
        dlltool=(TOOLCHAIN_DIR / "llvm-dlltool.exe").as_posix(),
    )


def ensure_cargo_config(ctx: Ctx, path: Path, scope: str) -> tuple[str, str]:
    """(status, detail) for the generated config at path; written unless --check."""
    text = cargo_config_text(ctx, scope)
    current = path.read_text(encoding="utf-8") if path.is_file() else None
    if current == text:
        return OK, f"{rel(path)} matches the pinned toolchain paths"
    if ctx.opts.check:
        status = WRONG if current is not None else MISSING
        return status, f"would write {rel(path)} with the pinned linker and dlltool"

    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8", newline="\n")
    return DONE, f"wrote {rel(path)}"


def step_cargo_config(ctx: Ctx) -> Step:
    if not IS_WINDOWS:
        return Step("cargo config", SKIP, "not needed on macOS")
    if not ctx.toolchain_ready:
        return Step("cargo config", MISSING, "pinned toolchain not installed (step 2)")

    results = [
        ensure_cargo_config(ctx, SPIKES_CARGO_CONFIG, "spikes/"),
        ensure_cargo_config(ctx, BODY_CARGO_CONFIG, "body/"),
    ]
    statuses = [status for status, _ in results]
    details = [detail for _, detail in results]
    return Step("cargo config", combine(statuses), "; ".join(details))


def step_azalea(ctx: Ctx) -> Step:
    commit = AZALEA["commit"]
    short = commit[:7]
    patches = sorted(REPO.glob(AZALEA["patches_glob"]))
    if not patches:
        raise SetupError(f"no patches match {AZALEA['patches_glob']} under {rel(REPO)}")

    if not (AZALEA_DIR / ".git").exists():
        if ctx.opts.check:
            return Step("azalea", MISSING, f"{rel(AZALEA_DIR)} not cloned; would clone {AZALEA['repo']}")
        if AZALEA_DIR.exists() and any(AZALEA_DIR.iterdir()):
            raise SetupError(
                f"{rel(AZALEA_DIR)} exists but is not a git clone; move it away and rerun, "
                f"or reclone from {AZALEA['repo']} at {short}"
            )
        must_run(["git", "clone", AZALEA["repo"], str(AZALEA_DIR)], "git clone azalea")

    # Refuse to touch a clone that carries changes which are not ours.
    ours: set[str] = set()
    for patch in patches:
        ours |= patch_paths(patch, AZALEA_DIR)
    foreign = sorted(worktree_changes(AZALEA_DIR) - ours)
    if foreign:
        shown = ", ".join(foreign[:8]) + (" ..." if len(foreign) > 8 else "")
        raise SetupError(
            f"{rel(AZALEA_DIR)} has uncommitted changes that are not in {AZALEA['patches_glob']}: {shown}. "
            "Commit, stash or remove them (or delete the clone and rerun); setup will not touch them."
        )

    statuses = [OK]
    notes: list[str] = []
    head = git_out(["rev-parse", "HEAD"], AZALEA_DIR).stdout.strip()
    if head != commit:
        if ctx.opts.check:
            statuses.append(WRONG)
            notes.append(f"HEAD is {head[:7]}, pinned is {short}")
        else:
            if git_out(["cat-file", "-e", f"{commit}^{{commit}}"], AZALEA_DIR, allow_fail=True).returncode != 0:
                must_run(["git", "fetch", "--quiet", "origin"], "git fetch azalea", AZALEA_DIR)
            must_run(["git", "-c", "advice.detachedHead=false", "checkout", commit], f"git checkout {short}", AZALEA_DIR)
            head = commit
            statuses.append(DONE)
            notes.append(f"checked out {short}")
    notes.insert(0, f"{rel(AZALEA_DIR)} at {head[:7]}")

    applied: list[str] = []
    pending: list[str] = []
    for patch in patches:
        if git_out(["apply", "--check", "-R", str(patch)], AZALEA_DIR, allow_fail=True).returncode == 0:
            applied.append(patch.name)
            continue
        if git_out(["apply", "--check", str(patch)], AZALEA_DIR, allow_fail=True).returncode != 0:
            raise SetupError(
                f"patch {patch.name} neither applies nor is applied in {rel(AZALEA_DIR)}; "
                "the clone is not in the expected state, so setup stops here"
            )
        if ctx.opts.check:
            pending.append(patch.name)
            statuses.append(WRONG)
            continue
        must_run(["git", "apply", str(patch)], f"git apply {patch.name}", AZALEA_DIR)
        applied.append(patch.name)
        statuses.append(DONE)
        notes.append(f"applied {patch.name}")

    notes.append(f"patches applied {len(applied)}/{len(patches)}")
    if pending:
        notes.append("not applied: " + ", ".join(pending))
    return Step("azalea", combine(statuses), "; ".join(notes))


def step_java(ctx: Ctx) -> Step:
    candidates: list[tuple[Path, str]] = []
    if ctx.opts.java:
        candidates.append((Path(ctx.opts.java).expanduser(), "--java"))
    else:
        launcher = launcher_java()
        if launcher is not None:
            candidates.append((launcher, "Minecraft launcher runtime"))
        on_path = shutil.which("java")
        if on_path:
            candidates.append((Path(on_path), "PATH"))

    tried: list[str] = []
    for path, origin in candidates:
        if not path.is_file():
            tried.append(f"{origin}: {path} is missing")
            continue
        major, first_line = java_version(path)
        if major == 25:
            return Step("java 25", OK, f"{path} ({origin}; {first_line})")
        tried.append(f"{origin}: {path} reports Java {major if major is not None else 'unknown'}")
        if ctx.opts.java:
            raise SetupError(f"--java {path} is not a Java 25 runtime ({first_line or 'did not run'}); Java 25 is required")

    advice = (
        "Java 25 not found. Install the official Minecraft launcher and run 26.3 once "
        "(it downloads java-runtime-epsilon), or pass --java <path to java.exe>."
    )
    if tried:
        advice += " Tried: " + "; ".join(tried)
    if ctx.opts.check:
        return Step("java 25", MISSING, advice)
    raise SetupError(advice)


def ensure_eula(ctx: Ctx) -> tuple[str, str]:
    path = SERVER_DIR / "eula.txt"
    raw = ""
    if path.is_file():
        raw = path.read_text(encoding="utf-8", errors="surrogateescape")
        if re.search(r"^\s*eula\s*=\s*true\s*$", raw, re.IGNORECASE | re.MULTILINE):
            return OK, "EULA accepted"

    if not ctx.opts.accept_eula:
        return WARN, f"EULA not accepted: rerun with --accept-eula after reading {EULA_URL}"
    if ctx.opts.check:
        return MISSING, f"EULA would be accepted ({rel(path)} not written in --check mode)"

    if raw:
        text = render_properties(raw, {"eula": "true"})
    else:
        text = (
            "#By changing the setting below to TRUE you are indicating your agreement to our EULA "
            f"({EULA_URL}).\n"
            "#written by spikes/setup.py --accept-eula\n"
            "eula=true\n"
        )
    path.write_text(text, encoding="utf-8", newline="\n")
    return DONE, f"accepted the EULA in {rel(path)} (--accept-eula)"


def download_server_jar() -> None:
    version = MINECRAFT["version"]
    manifest = fetch_json(MINECRAFT["version_manifest_url"])
    entry = next((item for item in manifest.get("versions", []) if item.get("id") == version), None)
    if entry is None:
        raise SetupError(f"Minecraft {version} is not in the version manifest {MINECRAFT['version_manifest_url']}")
    meta = fetch_json(entry["url"])
    url = (meta.get("downloads", {}).get("server") or {}).get("url")
    if not url:
        raise SetupError(f"the {version} version JSON has no downloads.server.url")

    tmp = download(url, SERVER_JAR, MINECRAFT["server_jar_size"])
    size = tmp.stat().st_size
    digest = sha1_of(tmp)
    if size != MINECRAFT["server_jar_size"] or digest != MINECRAFT["server_jar_sha1"]:
        tmp.unlink(missing_ok=True)
        raise SetupError(
            f"server jar verification failed: size {size} (pinned {MINECRAFT['server_jar_size']}), "
            f"sha1 {digest} (pinned {MINECRAFT['server_jar_sha1']}); the download was deleted"
        )
    os.replace(tmp, SERVER_JAR)


def step_server(ctx: Ctx) -> Step:
    version = MINECRAFT["version"]
    statuses: list[str] = []
    parts: list[str] = []

    # 1. Server jar, verified against the pins.
    if SERVER_JAR.is_file():
        size = SERVER_JAR.stat().st_size
        digest = sha1_of(SERVER_JAR)
        if size == MINECRAFT["server_jar_size"] and digest == MINECRAFT["server_jar_sha1"]:
            statuses.append(OK)
            parts.append(f"server-{version}.jar sha1 and size match")
        elif ctx.opts.check:
            statuses.append(WRONG)
            parts.append(f"server-{version}.jar is wrong (size {size}, sha1 {digest})")
        else:
            SERVER_JAR.unlink()
            download_server_jar()
            statuses.append(DONE)
            parts.append(f"re-downloaded server-{version}.jar")
    elif ctx.opts.check:
        statuses.append(MISSING)
        parts.append(f"server-{version}.jar missing")
    else:
        download_server_jar()
        statuses.append(DONE)
        parts.append(f"downloaded server-{version}.jar")

    # 2. Run directory and server.properties.
    if not SERVER_DIR.is_dir():
        if ctx.opts.check:
            statuses.append(MISSING)
            parts.append(f"{rel(SERVER_DIR)} missing")
        else:
            SERVER_DIR.mkdir(parents=True, exist_ok=True)
            statuses.append(DONE)
            parts.append(f"created {rel(SERVER_DIR)}")

    desired = PINS["server_properties"]
    props = SERVER_DIR / "server.properties"
    if props.is_file():
        raw = props.read_text(encoding="utf-8", errors="surrogateescape")
        rendered = render_properties(raw, desired)
        if rendered == raw:
            statuses.append(OK)
            parts.append(f"server.properties has all {len(desired)} keys")
        elif ctx.opts.check:
            missing, wrong = properties_report(raw, desired)
            problems = []
            if missing:
                problems.append("missing " + ", ".join(missing))
            if wrong:
                problems.append("wrong value for " + ", ".join(wrong))
            statuses.append(WRONG)
            parts.append("server.properties would change (" + "; ".join(problems) + ")")
        else:
            props.write_text(rendered, encoding="utf-8", newline="\n")
            statuses.append(DONE)
            parts.append(f"fixed {len(desired)} keys in server.properties")
    elif ctx.opts.check:
        statuses.append(MISSING)
        parts.append("server.properties missing")
    else:
        props.write_text(render_properties("", desired), encoding="utf-8", newline="\n")
        statuses.append(DONE)
        parts.append("wrote server.properties")

    # 3. EULA: never accepted on the user's behalf.
    eula_status, eula_detail = ensure_eula(ctx)
    statuses.append(eula_status)
    parts.append(eula_detail)

    return Step("server", combine(statuses), "; ".join(parts))


def step_client(ctx: Ctx) -> Step:
    version = MINECRAFT["version"]
    version_dir = minecraft_dir() / "versions" / version
    jar = version_dir / f"{version}.jar"
    meta = version_dir / f"{version}.json"

    problems: list[str] = []
    if not meta.is_file():
        problems.append(f"version JSON missing ({rel(meta)})")
    if not jar.is_file():
        problems.append(f"client jar missing ({rel(jar)})")
    elif sha1_of(jar) != MINECRAFT["client_jar_sha1"]:
        problems.append(f"client jar sha1 {sha1_of(jar)} does not match the pin")

    if problems:
        return Step(
            "client files",
            WARN,
            "the official launcher's 26.3 install is not verified: "
            + "; ".join(problems)
            + " (run the launcher once and install 26.3; setup never downloads the client)",
        )
    return Step(
        "client files",
        OK,
        f"26.3 version JSON and jar present, jar sha1 matches ({rel(jar)})",
    )


def step_build(ctx: Ctx) -> Step:
    crates: list[tuple[str, list[str]]] = [("proxy", ["proxy", "pingbench"]), ("decode", ["decode"])]
    if IS_WINDOWS:
        crates.append(("hands", ["hands"]))
    suffix = ".exe" if IS_WINDOWS else ""

    if ctx.opts.check:
        missing = [
            f"{crate}/{binary}{suffix}"
            for crate, binaries in crates
            for binary in binaries
            if not (SPIKES / crate / "target" / "release" / f"{binary}{suffix}").is_file()
        ]
        if missing:
            return Step("build", MISSING, "release binaries missing: " + ", ".join(missing))
        return Step("build", OK, "release binaries present (" + ", ".join(crate for crate, _ in crates) + ")")

    if ctx.opts.no_build:
        return Step("build", SKIP, "--no-build")

    if not ctx.toolchain_ready:
        return Step("build", MISSING, "pinned toolchain not installed (step 2)")

    for crate, _ in crates:
        cargo_build(ctx, ["build", "--release"], SPIKES / crate, f"cargo build in spikes/{crate}")
    detail = "built " + ", ".join(crate for crate, _ in crates)
    if ctx.opts.test:
        for crate, _ in crates:
            cargo_build(ctx, ["test", "--release"], SPIKES / crate, f"cargo test in spikes/{crate}")
        detail += " and ran tests"
    return Step("build", DONE, detail)


STEPS = [
    ("prerequisites", step_prerequisites),
    ("rust toolchain", step_rust),
    ("dlltool shim", step_dlltool),
    ("cargo config", step_cargo_config),
    ("azalea", step_azalea),
    ("java 25", step_java),
    ("server", step_server),
    ("client files", step_client),
    ("build", step_build),
]


# --------------------------------------------------------------------------- main


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="setup.py",
        description="One-command, idempotent setup for the Praxion session 2.7 spikes (spikes/).",
        epilog=(
            "Run it again at any time; finished steps print 'ok: ...' and are skipped. "
            "Details: spikes/SETUP.md"
        ),
    )
    parser.add_argument("--check", action="store_true", help="only report the state of every step; change nothing")
    parser.add_argument("--no-build", action="store_true", help="skip cargo build (and cargo test)")
    parser.add_argument("--test", action="store_true", help="also run cargo test --release in every crate")
    parser.add_argument(
        "--accept-eula",
        action="store_true",
        help=f"write eula=true in spikes/round0/server/eula.txt (read {EULA_URL} first)",
    )
    parser.add_argument("--java", metavar="PATH", help="path to a Java 25 java/java.exe")
    return parser


def print_summary(steps: list[Step]) -> None:
    rows = [(f"{index} {step.name}", step.status, one_line(step.detail)) for index, step in enumerate(steps, 1)]
    width = max([len("step")] + [len(row[0]) for row in rows])
    print()
    print("summary")
    print(f"{'step'.ljust(width)}  {'status':<7}  detail")
    print(f"{'-' * width}  {'-' * 7}  {'-' * 40}")
    for name, status, detail in rows:
        print(f"{name.ljust(width)}  {status:<7}  {detail}")


def main(argv=None) -> int:
    parser = build_parser()
    opts = parser.parse_args(argv)
    if opts.test and opts.no_build:
        parser.error("--test needs the build step; drop --no-build")

    ctx = Ctx(opts=opts)
    mode = "check only, nothing will change" if opts.check else "setup"
    print(f"Praxion 2.7 spike setup: {mode}")
    print(f"repo {REPO}  |  python {platform.python_version()}  |  {platform.system()}")
    print()

    steps: list[Step] = []
    for number, (name, function) in enumerate(STEPS, 1):
        try:
            step = function(ctx)
        except SetupError as exc:
            step = Step(name, FAIL, str(exc))
        print(f"{STATUS_LABEL[step.status]}: [{number}/{len(STEPS)}] {step.name}: {one_line(step.detail)}", flush=True)
        steps.append(step)

    print_summary(steps)

    failed = [step for step in steps if step.status in FAIL_STATUSES]
    warned = [step for step in steps if step.status == WARN]
    print()
    if failed:
        print(f"{len(failed)} step(s) need attention; see the table above.")
        if opts.check:
            print("Rerun without --check (and with the flags it names) to fix them.")
        return 1
    if warned:
        print(f"all steps ok or intentionally skipped; {len(warned)} warning(s) above need the user.")
    else:
        print("all steps ok or intentionally skipped; a rerun changes nothing.")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except KeyboardInterrupt:
        sys.stderr.write("\ninterrupted\n")
        raise SystemExit(130)

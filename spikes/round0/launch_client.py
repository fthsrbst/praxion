"""Launch the vanilla 26.3 client straight from the version JSON, without any launcher tweaker.

Reuses the files TLauncher downloaded under %APPDATA%\\.minecraft, but checks every library and the
client jar against the sha1 in the version JSON first. Offline username only (local offline server).

Usage: python launch_client.py [--join localhost:25565] [--username Praxion] [--dry-run]
"""
import argparse
import hashlib
import json
import os
import platform
import subprocess
import sys
from pathlib import Path

MC = Path(os.environ["APPDATA"]) / ".minecraft"
VERSION = "26.3"
CLIENT_JAR_SHA1 = "e877b6a07acd633fb3bb475002175cec036e7b87"  # docs/sessions/02-body-connection.md manifest
JAVA = MC / "runtime/java-runtime-epsilon/windows/java-runtime-epsilon/bin/java.exe"
NATIVES = Path(__file__).resolve().parent / "client-natives"


def sha1(path: Path) -> str:
    h = hashlib.sha1()
    with path.open("rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def rules_allow(rules, features) -> bool:
    if not rules:
        return True
    allowed = False
    for rule in rules:
        os_rule = rule.get("os", {})
        if os_rule.get("name") not in (None, "windows"):
            continue
        if os_rule.get("arch") not in (None, "x86_64" if platform.machine().endswith("64") else "x86"):
            continue
        if "versionRange" in os_rule:
            continue  # only guards pre-1803 Windows builds
        if any(features.get(k, False) != v for k, v in rule.get("features", {}).items()):
            continue
        allowed = rule["action"] == "allow"
    return allowed


def expand(args, features, subst):
    out = []
    for arg in args:
        if isinstance(arg, str):
            values = [arg]
        elif rules_allow(arg.get("rules"), features):
            values = arg["value"] if isinstance(arg["value"], list) else [arg["value"]]
        else:
            continue
        for value in values:
            for key, repl in subst.items():
                value = value.replace("${" + key + "}", repl)
            out.append(value)
    return out


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--join", default=None, help="host:port to quick-join, e.g. localhost:25565")
    ap.add_argument("--username", default="Praxion")
    ap.add_argument("--dry-run", action="store_true")
    a = ap.parse_args()

    meta = json.loads((MC / f"versions/{VERSION}/{VERSION}.json").read_text("utf-8"))
    client_jar = MC / f"versions/{VERSION}/{VERSION}.jar"
    if sha1(client_jar) != CLIENT_JAR_SHA1:
        sys.exit("client jar sha1 mismatch")

    classpath = []
    for lib in meta["libraries"]:
        if not rules_allow(lib.get("rules"), {}):
            continue
        art = lib["downloads"]["artifact"]
        path = MC / "libraries" / art["path"]
        if not path.exists() or sha1(path) != art["sha1"]:
            sys.exit(f"library missing or sha1 mismatch: {art['path']}")
        classpath.append(str(path))
    classpath.append(str(client_jar))

    NATIVES.mkdir(exist_ok=True)
    features = {"is_quick_play_multiplayer": a.join is not None}
    subst = {
        "natives_directory": str(NATIVES),
        "launcher_name": "praxion-spike",
        "launcher_version": "0",
        "classpath": os.pathsep.join(classpath),
        "auth_player_name": a.username,
        "version_name": VERSION,
        "game_directory": str(MC),
        "assets_root": str(MC / "assets"),
        "assets_index_name": meta["assetIndex"]["id"],
        "auth_uuid": "00000000000000000000000000000000",
        "auth_access_token": "0",
        "clientid": "0",
        "auth_xuid": "0",
        "version_type": "release",
        "quickPlayMultiplayer": a.join or "",
    }
    jvm = expand(meta["arguments"]["jvm"], features, subst)
    game = expand(meta["arguments"]["game"], features, subst)
    cmd = [str(JAVA), "-Xmx4G", *jvm, meta["mainClass"], *game]

    sdl_env = {k: v for k, v in os.environ.items() if k.upper().startswith("SDL_")}
    print(f"libraries verified: {len(classpath) - 1}; SDL_* env: {sdl_env or 'none'}")
    if a.dry_run:
        print(" ".join(c if " " not in c else f'"{c}"' for c in cmd if not c.startswith(str(MC / "libraries"))))
        return
    subprocess.run(cmd, cwd=MC, check=False)


if __name__ == "__main__":
    main()

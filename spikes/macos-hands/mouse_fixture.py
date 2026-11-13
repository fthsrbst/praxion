"""Live fixture for mouse_probe: which post path does the vanilla client accept?

Starts a disposable flat creative world, launches the local 26.3 client into it
as the only player, and for each post path (hid, session, pid) checks two
effects on the server, never on the client:

  click  a stone block under the crosshair becomes air
  look   the player's yaw changes after a horizontal camera move

Run from the repository root (the client window must stay frontmost; do not
touch the mouse or keyboard while it runs, about one minute):

    xcrun swiftc -warnings-as-errors spikes/macos-hands/mouse_probe.swift -o logs/mac-hands/mouse-probe
    PYTHONPATH=harness brain/.venv/bin/python spikes/macos-hands/mouse_fixture.py logs/mac-mouse-probe

Writes `result.txt` (one line per check) into the given directory.
"""

from __future__ import annotations

import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

from praxion_harness.server import TestServer
from praxion_harness.watch_client import client_command

REPO = Path(__file__).resolve().parents[2]
PROBE = REPO / "logs/mac-hands/mouse-probe"
NAME = "Fatih"
PATHS = ("hid", "session", "pid")
BLOCK = "8 -59 11"
#: Where a block placed against BLOCK's near (north) face lands.
PLACED = "8 -59 10"


def stop(proc: subprocess.Popen) -> None:
    if proc.poll() is None:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()


def rotation(rcon) -> tuple[float, float] | None:
    """The server's (yaw, pitch) for the player."""
    reply = rcon.command(f"data get entity {NAME} Rotation")
    match = re.search(r"\[(-?[\d.]+)f, (-?[\d.]+)f\]", reply)
    return (float(match.group(1)), float(match.group(2))) if match else None


def probe(pid: int, path: str, *action: str) -> tuple[bool, str]:
    """Runs the probe; returns whether it posted, and a one-line report."""
    try:
        done = subprocess.run(
            [str(PROBE), "--pid", str(pid), "--tap", path, "--activate", *action],
            capture_output=True, text=True, timeout=30,
        )
    except subprocess.TimeoutExpired:
        return False, "exit=timeout result=killed"
    lines = dict(line.split("=", 1) for line in done.stdout.splitlines() if "=" in line)
    posted = done.returncode == 0 and lines.get("result") == "posted"
    return posted, (f"exit={done.returncode} result={lines.get('result')} reason={lines.get('reason')} "
                    f"steps={lines.get('steps_posted')}")


def main(out_dir: Path) -> int:
    out_dir.mkdir(parents=True, exist_ok=True)
    game = out_dir / "client"
    game.mkdir(exist_ok=True)
    options = (REPO / "third_party/minecraft-client/game-watch/options.txt").read_text()
    # A focus blip must not open the pause menu in the middle of a check.
    patched = options.replace("pauseOnLostFocus:true", "pauseOnLostFocus:false")
    if "pauseOnLostFocus:false" not in patched:
        print("fixture=failed reason=options_have_no_pauseOnLostFocus_line")
        return 1
    (game / "options.txt").write_text(patched)
    lines: list[str] = []

    def log(text: str) -> None:
        print(text, flush=True)
        lines.append(text)

    with TestServer(out_dir / "attempt", 1213, keep=True, properties={
        "difficulty": "peaceful", "level-type": "minecraft:flat",
        "view-distance": 4, "simulation-distance": 4,
    }) as server:
        rcon = server.rcon
        cmd, _ = client_command(name=NAME, host="127.0.0.1", port=server.port)
        cmd[cmd.index("--gameDir") + 1] = str(game)
        client = None
        try:
            for attempt in range(2):
                with (out_dir / f"client-{attempt}.txt").open("w") as out:
                    client = subprocess.Popen(
                        cmd, cwd=game, stdin=subprocess.DEVNULL, stdout=out, stderr=subprocess.STDOUT)
                deadline = time.monotonic() + 90
                while time.monotonic() < deadline:
                    if rcon.command("list").partition(": ")[2].strip() == NAME:
                        break
                    time.sleep(0.25)
                else:
                    stop(client)
                    if attempt == 1:
                        log("fixture=failed reason=client_did_not_join")
                        return 1
                    continue
                break
            rcon.command(f"gamemode creative {NAME}")
            time.sleep(3)  # let the client finish loading the world and grab the mouse
            pid = client.pid

            def is_block(pos: str, block: str) -> bool:
                return "Test passed" in rcon.command(f"execute if block {pos} minecraft:{block}")

            def reset(block: str = "stone") -> bool:
                rcon.command(f"tp {NAME} 8.5 -60 8.5 0 0")
                rcon.command(f"setblock {PLACED} minecraft:air")
                rcon.command(f"setblock {BLOCK} minecraft:{block}")
                time.sleep(1.5)
                return is_block(BLOCK, block)

            def look(path: str, dx: int, dy: int) -> tuple[bool, str, tuple[float, float] | None]:
                posted, report = probe(pid, path, "--look", str(dx), str(dy))
                time.sleep(0.8)
                return posted, report, rotation(rcon)

            # 1. One click and one camera move per post path (regression of the first run).
            for path in PATHS:
                if not reset():
                    log(f"path={path} check=click verdict=invalid reason=stone_block_not_set")
                else:
                    posted, report = probe(pid, path, "--click", "left")
                    time.sleep(0.8)
                    verdict = f"block_broken={is_block(BLOCK, 'air')}" if posted else "verdict=invalid"
                    log(f"path={path} check=click {report} {verdict}")
                posted, report, after = look(path, 300, 0)
                verdict = f"camera_moved={abs(after[0]) > 1.0}" if posted and after else "verdict=invalid"
                log(f"path={path} check=look {report} rotation_after={after} {verdict}")

            # The remaining checks use the session path only.
            path = "session"

            # 2. A held button mines a survival block (dirt by hand: 0.75 s).
            rcon.command(f"gamemode survival {NAME}")
            if reset("dirt"):
                posted, report = probe(pid, path, "--click", "left", "--hold-ms", "1500")
                time.sleep(0.8)
                verdict = f"block_mined={is_block(BLOCK, 'air')}" if posted else "verdict=invalid"
                log(f"check=hold_mine {report} {verdict}")
                # A short click must not mine it: the hold, not the click, did the work.
                if reset("dirt"):
                    posted, report = probe(pid, path, "--click", "left")
                    time.sleep(0.8)
                    verdict = f"block_still_there={is_block(BLOCK, 'dirt')}" if posted else "verdict=invalid"
                    log(f"check=short_click_control {report} {verdict}")
            else:
                log("check=hold_mine verdict=invalid reason=dirt_block_not_set")

            # 3. A right click places the held block against the target's near face.
            rcon.command(f"gamemode creative {NAME}")
            rcon.command(f"item replace entity {NAME} hotbar.0 with minecraft:stone 8")
            if reset():
                posted, report = probe(pid, path, "--click", "right")
                time.sleep(0.8)
                verdict = f"block_placed={is_block(PLACED, 'stone')}" if posted else "verdict=invalid"
                log(f"check=right_click_place {report} {verdict}")
            else:
                log("check=right_click_place verdict=invalid reason=stone_block_not_set")

            # 4. Pitch.
            reset()
            posted, report, after = look(path, 0, 200)
            verdict = f"pitch_moved={abs(after[1]) > 1.0}" if posted and after else "verdict=invalid"
            log(f"check=pitch {report} rotation_after={after} {verdict}")

            # 5. Gain: the same move five times, then other sizes, each from yaw 0.
            for dx in (200, 200, 200, 200, 200, 50, 100, 400, 800, -200):
                reset()
                posted, report, after = look(path, dx, 0)
                if posted and after:
                    log(f"check=gain dx={dx} yaw={after[0]} deg_per_px={after[0] / dx:.5f}")
                else:
                    log(f"check=gain dx={dx} {report} verdict=invalid")
        finally:
            if client is not None:
                stop(client)
            (out_dir / "result.txt").write_text("\n".join(lines) + "\n")
    return 0


if __name__ == "__main__":
    if len(sys.argv) != 2:
        print(__doc__)
        raise SystemExit(2)
    if not PROBE.exists():
        print(f"build the probe first: {PROBE}")
        raise SystemExit(2)
    shutil.rmtree(Path(sys.argv[1]) / "attempt", ignore_errors=True)
    raise SystemExit(main(Path(sys.argv[1])))

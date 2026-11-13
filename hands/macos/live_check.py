"""Live check of `praxion-hands` (S3a): scripted frames against a disposable world.

Starts a flat test world and the local 26.3 client as the only player, starts
the helper on the client's process id, and plays short frame scripts through
its stdin at 20 frames per second. Every verdict is read from the server:

  walk        forward held for 30 frames moves the player north-south
  turn        ten frames of dx=20 turn the yaw by 30 degrees (gain 0.15)
  mine        the left button held for 30 frames mines survival dirt
  drag_look   the camera still turns while the left button is held
  place       one right-button frame places the held block
  slot        `slot 3` selects hotbar slot 3
  idle        after one forward frame and silence the helper prints IDLE and the player stops
  eof         closing stdin makes the helper print BYE and the player stops

Takeover by physical input needs a person and is not tested here.

Run from the repository root; do not touch the mouse or keyboard (about 90 s):

    xcrun swiftc -O -warnings-as-errors hands/macos/praxion_hands.swift -o hands/macos/build/praxion-hands
    PYTHONPATH=harness brain/.venv/bin/python hands/macos/live_check.py logs/mac-hands-live-check

Writes `result.txt` into the given directory. Exit 0 when every check passed.
"""

from __future__ import annotations

import queue
import re
import shutil
import subprocess
import sys
import threading
import time
from pathlib import Path

from praxion_harness.server import TestServer
from praxion_harness.watch_client import client_command

REPO = Path(__file__).resolve().parents[2]
HELPER = REPO / "hands/macos/build/praxion-hands"
NAME = "Fatih"
BLOCK = "8 -59 11"
PLACED = "8 -59 10"
FRAME_SECONDS = 0.05


class Helper:
    """The helper process, its stdin, and its replies read on a thread."""

    def __init__(self, pid: int, log_path: Path) -> None:
        self.proc = subprocess.Popen(
            [str(HELPER), "--serve", "--pid", str(pid), "--activate"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, bufsize=1)
        self.replies: queue.Queue[str] = queue.Queue()
        self.seen: list[str] = []
        self.log = log_path.open("w")
        self.tick = 0
        threading.Thread(target=self._read, daemon=True).start()

    def _read(self) -> None:
        assert self.proc.stdout is not None
        for line in self.proc.stdout:
            line = line.strip()
            self.log.write(f"< {line}\n")
            self.log.flush()
            self.replies.put(line)

    def wait_for(self, prefix: str, seconds: float) -> str | None:
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            try:
                line = self.replies.get(timeout=0.1)
            except queue.Empty:
                continue
            self.seen.append(line)
            if line.startswith(prefix):
                return line
        return None

    def send(self, line: str) -> None:
        assert self.proc.stdin is not None
        self.log.write(f"> {line}\n")
        self.proc.stdin.write(line + "\n")
        self.proc.stdin.flush()

    def frames(self, count: int, keys: int = 0, dx: int = 0, dy: int = 0, buttons: int = 0,
               slot: str = "-") -> None:
        """`count` identical frames at 20 per second, then nothing."""
        for _ in range(count):
            self.tick += 1
            self.send(f"F {self.tick} 0 {keys} {dx} {dy} {buttons} {slot}")
            time.sleep(FRAME_SECONDS)

    def release(self) -> None:
        self.frames(2)

    def drain(self) -> list[str]:
        lines = []
        while True:
            try:
                lines.append(self.replies.get_nowait())
            except queue.Empty:
                break
        self.seen.extend(lines)
        return lines

    def close(self) -> None:
        if self.proc.poll() is None:
            try:
                assert self.proc.stdin is not None
                self.proc.stdin.close()
            except OSError:
                pass
            try:
                self.proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.proc.kill()
        self.log.close()


def stop(proc: subprocess.Popen) -> None:
    if proc.poll() is None:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()


def main(out_dir: Path) -> int:
    out_dir.mkdir(parents=True, exist_ok=True)
    game = out_dir / "client"
    game.mkdir(exist_ok=True)
    options = (REPO / "third_party/minecraft-client/game-watch/options.txt").read_text()
    patched = options.replace("pauseOnLostFocus:true", "pauseOnLostFocus:false")
    if "pauseOnLostFocus:false" not in patched:
        print("fixture=failed reason=options_have_no_pauseOnLostFocus_line")
        return 1
    (game / "options.txt").write_text(patched)
    lines: list[str] = []
    failed = False

    def log(text: str) -> None:
        print(text, flush=True)
        lines.append(text)

    def check(name: str, ok: bool, detail: str) -> None:
        nonlocal failed
        failed = failed or not ok
        log(f"check={name} ok={ok} {detail}")

    client = None
    helper = None
    try:
        with TestServer(out_dir / "attempt", 1215, keep=True, properties={
            "difficulty": "peaceful", "level-type": "minecraft:flat",
            "view-distance": 4, "simulation-distance": 4,
        }) as server:
            rcon = server.rcon

            def pos() -> tuple[float, float, float]:
                reply = rcon.command(f"data get entity {NAME} Pos")
                x, y, z = re.findall(r"(-?[\d.]+)d", reply)[:3]
                return float(x), float(y), float(z)

            def rotation() -> tuple[float, float]:
                reply = rcon.command(f"data get entity {NAME} Rotation")
                yaw, pitch = re.findall(r"(-?[\d.]+)f", reply)[:2]
                return float(yaw), float(pitch)

            def is_block(where: str, block: str) -> bool:
                return "Test passed" in rcon.command(f"execute if block {where} minecraft:{block}")

            def reset(block: str = "stone") -> None:
                rcon.command(f"tp {NAME} 8.5 -60 8.5 0 0")
                rcon.command(f"setblock {PLACED} minecraft:air")
                rcon.command(f"setblock {BLOCK} minecraft:{block}")
                time.sleep(1.5)

            cmd, _ = client_command(name=NAME, host="127.0.0.1", port=server.port)
            cmd[cmd.index("--gameDir") + 1] = str(game)
            for attempt in range(2):
                client = subprocess.Popen(
                    cmd, cwd=game, stdin=subprocess.DEVNULL,
                    stdout=(out_dir / f"client-{attempt}.txt").open("w"), stderr=subprocess.STDOUT)
                deadline = time.monotonic() + 90
                while time.monotonic() < deadline:
                    if rcon.command("list").partition(": ")[2].strip() == NAME:
                        break
                    time.sleep(0.25)
                else:
                    stop(client)
                    client = None
                    continue
                break
            if client is None:
                log("fixture=failed reason=client_did_not_join")
                return 1
            rcon.command(f"gamemode creative {NAME}")
            time.sleep(3)

            helper = Helper(client.pid, out_dir / "helper.txt")
            ready = helper.wait_for("READY", 25)
            if ready is None:
                log(f"fixture=failed reason=helper_not_ready seen={helper.seen}")
                return 1
            helper.send("ARM")

            # walk: the block in front is cleared so the player can move south (+z).
            reset("air")
            before = pos()
            helper.frames(30, keys=1)
            helper.release()
            time.sleep(1.0)
            after = pos()
            check("walk", after[2] - before[2] > 3.0, f"z_before={before[2]:.2f} z_after={after[2]:.2f}")
            settled = pos()
            time.sleep(0.5)
            check("walk_stops", abs(pos()[2] - settled[2]) < 0.01, f"z={settled[2]:.2f}")

            # turn
            reset("air")
            helper.frames(10, dx=20)
            time.sleep(0.8)
            yaw = rotation()[0]
            check("turn", abs(yaw - 30.0) < 0.5, f"yaw={yaw}")

            # mine, then the camera under a held button
            rcon.command(f"gamemode survival {NAME}")
            reset("dirt")
            helper.frames(30, buttons=1)
            helper.release()
            time.sleep(0.8)
            check("mine", is_block(BLOCK, "air"), "dirt mined by a 1.5 s hold")
            reset("air")
            helper.frames(10, dx=10, buttons=1)
            helper.release()
            time.sleep(0.8)
            yaw = rotation()[0]
            check("drag_look", abs(yaw - 15.0) < 0.5, f"yaw={yaw}")

            # place
            rcon.command(f"gamemode creative {NAME}")
            rcon.command(f"item replace entity {NAME} hotbar.0 with minecraft:stone 8")
            reset("stone")
            helper.frames(1, slot="1")
            helper.frames(1, buttons=2)
            helper.release()
            time.sleep(0.8)
            check("place", is_block(PLACED, "stone"), "stone placed against the target")

            # slot
            helper.frames(1, slot="3")
            time.sleep(0.8)
            slot = rcon.command(f"data get entity {NAME} SelectedItemSlot")
            check("slot", slot.strip().endswith(": 2"), f"server={slot!r}")

            refused = [line for line in helper.drain() if line.startswith(("REFUSED", "TAKEOVER"))]
            check("no_refusals", not refused, f"unexpected={refused[:5]}")

            # idle: one forward frame, then silence.
            reset("air")
            helper.frames(1, keys=1)
            idle = helper.wait_for("IDLE", 2.0)
            time.sleep(0.6)
            first = pos()
            time.sleep(0.6)
            check("idle", idle is not None and abs(pos()[2] - first[2]) < 0.01,
                  f"reply={idle} z={first[2]:.2f}")

            # eof: forward for a few frames, then the pipe closes.
            reset("air")
            helper.frames(6, keys=1)
            helper.proc.stdin.close()
            bye = helper.wait_for("BYE", 3.0)
            time.sleep(0.6)
            first = pos()
            time.sleep(0.6)
            check("eof", bye is not None and abs(pos()[2] - first[2]) < 0.01, f"reply={bye} z={first[2]:.2f}")
    finally:
        if helper is not None:
            helper.close()
        if client is not None:
            stop(client)
        lines.append(f"fixture={'failed' if failed else 'passed'}")
        (out_dir / "result.txt").write_text("\n".join(lines) + "\n")
    print(lines[-1], flush=True)
    return 1 if failed else 0


if __name__ == "__main__":
    if len(sys.argv) != 2:
        print(__doc__)
        raise SystemExit(2)
    if not HELPER.exists():
        print(f"build the helper first: {HELPER}")
        raise SystemExit(2)
    shutil.rmtree(Path(sys.argv[1]) / "attempt", ignore_errors=True)
    raise SystemExit(main(Path(sys.argv[1])))

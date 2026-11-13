"""Live check of S2 shadow mode: does the body follow the operator's own client?

Starts a disposable flat world, the product forwarder, `praxion-body --real-client`
on the forwarder's tee (before the client joins), and the local 26.3 client as the
only player. Then it compares what the body's port snapshot says with what the
server says:

  inventory   3 stone given by the server appear in the snapshot
  position    the snapshot position equals the server's block position, before
              and after a 1.5 s forward walk posted by the keyboard spike
  tick rate   snapshot ticks advance at about 20 per second of wall time
  validity    `world_valid` stays true for the whole observation
  silence     the server never sees a second player

Run from the repository root; the client window must stay frontmost and the
operator must not touch the mouse or keyboard (about 90 seconds):

    xcrun swiftc -warnings-as-errors spikes/macos-hands/main.swift -o logs/mac-hands/macos-hands
    xcrun swiftc -warnings-as-errors spikes/macos-hands/mouse_probe.swift -o logs/mac-hands/mouse-probe
    PYTHONPATH=harness:brain brain/.venv/bin/python spikes/macos-hands/shadow_fixture.py logs/mac-shadow

Writes `result.txt` into the given directory. Exit 0 when every check passed.
"""

from __future__ import annotations

import json
import shutil
import signal
import socket
import subprocess
import sys
import time
from pathlib import Path

from praxion_brain.port import PortClient, Snapshot
from praxion_harness.referee import Referee
from praxion_harness.server import TestServer
from praxion_harness.watch_client import client_command

REPO = Path(__file__).resolve().parents[2]
RELEASE = REPO / "body/target/release"
HANDS = REPO / "logs/mac-hands/macos-hands"
PROBE = REPO / "logs/mac-hands/mouse-probe"
NAME = "Fatih"
OBSERVE_SECONDS = 20.0


def free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def stop(proc: subprocess.Popen, sig: int = signal.SIGTERM) -> None:
    if proc.poll() is None:
        proc.send_signal(sig)
        try:
            proc.wait(timeout=8)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()


def wait_listening(port: int, seconds: float) -> bool:
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        try:
            socket.create_connection(("127.0.0.1", port), timeout=0.5).close()
            return True
        except OSError:
            time.sleep(0.2)
    return False


def snapshot(client: PortClient, seconds: float = 10.0) -> Snapshot | None:
    """The newest snapshot after a resync.

    The body also sends a snapshot on every wake, so the queue can hold older
    ones: drain it and keep the snapshot with the highest tick.
    """
    client.resync()
    newest: Snapshot | None = None
    deadline = time.monotonic() + seconds
    quiet_until = None
    while time.monotonic() < deadline:
        message = client.poll(timeout=0.2)
        if isinstance(message, Snapshot):
            if newest is None or message.tick >= newest.tick:
                newest = message
            quiet_until = time.monotonic() + 0.4
        elif message is None and quiet_until is not None and time.monotonic() >= quiet_until:
            break
    return newest


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

    procs: list[subprocess.Popen] = []
    try:
        with TestServer(out_dir / "attempt", 1214, keep=True, properties={
            "difficulty": "peaceful", "level-type": "minecraft:flat",
            "view-distance": 4, "simulation-distance": 4,
        }) as server:
            rcon = server.rcon
            game_port, tee_port, body_port = free_port(), free_port(), free_port()
            forwarder = subprocess.Popen(
                [str(RELEASE / "praxion-forwarder"), "--listen", f"127.0.0.1:{game_port}",
                 "--upstream", f"127.0.0.1:{server.port}", "--tee-listen", f"127.0.0.1:{tee_port}"],
                stdout=(out_dir / "forwarder.txt").open("w"), stderr=subprocess.STDOUT)
            procs.append(forwarder)
            if not wait_listening(tee_port, 10):
                log("fixture=failed reason=forwarder_did_not_start")
                return 1
            body_dir = out_dir / "body"
            shutil.rmtree(body_dir, ignore_errors=True)
            body = subprocess.Popen(
                [str(RELEASE / "praxion-body"), "--real-client", "--tee", f"127.0.0.1:{tee_port}",
                 "--name", NAME, "--port", f"127.0.0.1:{body_port}", "--run-dir", str(body_dir),
                 "--max-ticks", "100000"],
                stdout=(out_dir / "body.txt").open("w"), stderr=subprocess.STDOUT)
            procs.append(body)
            time.sleep(1.0)
            if body.poll() is not None:
                log(f"fixture=failed reason=body_exited code={body.returncode}")
                return 1

            cmd, _ = client_command(name=NAME, host="127.0.0.1", port=game_port)
            cmd[cmd.index("--gameDir") + 1] = str(game)
            client = None
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
            procs.append(client)

            referee = Referee(rcon, player=NAME)
            rcon.command(f"gamemode creative {NAME}")
            rcon.command(f"tp {NAME} 8.5 -60 8.5 0 0")
            rcon.command(f"give {NAME} minecraft:stone 3")
            time.sleep(3)

            if not wait_listening(body_port, 20):
                log("fixture=failed reason=body_port_not_listening")
                return 1
            port = PortClient(body_port, handshake_timeout=15.0, connect_timeout=15.0)
            port.connect()

            def floor_pos() -> list[int]:
                return [int(value // 1) for value in referee.player_pos()]

            first = snapshot(port)
            if first is None:
                log("fixture=failed reason=no_snapshot")
                return 1
            started = time.monotonic()
            check("inventory", first.facts.inventory.get("stone") == 3, f"snapshot={first.facts.inventory}")
            check("position_before", first.facts.pos == floor_pos(),
                  f"snapshot={first.facts.pos} server={floor_pos()}")

            # Bring the client forward, then one bounded forward hold from the keyboard spike.
            subprocess.run([str(PROBE), "--pid", str(client.pid), "--tap", "session", "--activate",
                            "--look", "0", "0"], capture_output=True, text=True, timeout=30)
            walk = subprocess.run([str(HANDS), "--run", "--pid", str(client.pid), "--keycode", "13"],
                                  capture_output=True, text=True, timeout=40)
            walk_lines = dict(line.split("=", 1) for line in walk.stdout.splitlines() if "=" in line)
            time.sleep(1.5)
            second = snapshot(port)
            moved = second is not None and second.facts.pos != first.facts.pos
            check("walk_posted", walk.returncode == 0, f"exit={walk.returncode} result={walk_lines.get('result')}")
            check("position_after", second is not None and second.facts.pos == floor_pos() and moved,
                  f"snapshot={second.facts.pos if second else None} server={floor_pos()} moved={moved}")

            valid = True
            while time.monotonic() - started < OBSERVE_SECONDS:
                time.sleep(2)
                current = snapshot(port)
                valid = valid and current is not None and current.facts.world_valid
            last = snapshot(port)
            elapsed = time.monotonic() - started
            if last is not None:
                rate = (last.tick - first.tick) / elapsed
                check("tick_rate", 18.0 <= rate <= 21.0, f"ticks={last.tick - first.tick} seconds={elapsed:.1f} rate={rate:.2f}")
            else:
                check("tick_rate", False, "no_snapshot")
            check("world_valid", valid, f"observed_seconds={elapsed:.1f}")
            players = rcon.command("list")
            check("single_player", players.partition(": ")[2].strip() == NAME, f"server_list={players!r}")

            stop(body)
            summary_path = body_dir / "summary.json"
            if summary_path.exists():
                log(f"body_summary={json.dumps(json.loads(summary_path.read_text()))[:600]}")
            else:
                log("body_summary=missing")
    finally:
        for proc in reversed(procs):
            stop(proc)
        lines.append(f"fixture={'failed' if failed else 'passed'}")
        (out_dir / "result.txt").write_text("\n".join(lines) + "\n")
    print(lines[-1], flush=True)
    return 1 if failed else 0


if __name__ == "__main__":
    if len(sys.argv) != 2:
        print(__doc__)
        raise SystemExit(2)
    for tool in (HANDS, PROBE):
        if not tool.exists():
            print(f"build the tools first: {tool}")
            raise SystemExit(2)
    shutil.rmtree(Path(sys.argv[1]) / "attempt", ignore_errors=True)
    raise SystemExit(main(Path(sys.argv[1])))

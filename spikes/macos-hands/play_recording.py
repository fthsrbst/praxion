"""Record Praxion playing the operator's own vanilla client (experiment, not product code).

Starts a survival world, the product forwarder, the local 26.3 client as the only
player (named like the operator), `praxion-body --real-client --hands` on the
forwarder's tee (attached before the client logs in, so the world view is whole),
and the brain on the body's port with its web console. Then it records the main
display with `screencapture -v` while the brain plays, sending operator tasks
through the console API.

Hands mode posts keyboard and mouse input to the client (S3 of
`docs/proposals/2026-11-12-macos-real-client-body.md`). It does not open menus
yet (S4), so tasks that only walk and mine are the ones it can finish; the
default task list is wood. Touching the mouse or keyboard takes control back.

Run from the repository root, then keep your hands off for the whole run:

    PYTHONPATH=harness:brain brain/.venv/bin/python spikes/macos-hands/play_recording.py \
        logs/demo1112/play-rec --seed 131469 --minutes 6 --task "8 odun topla"

Writes `screen.mov`, `result.txt` and every process log into the given directory.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import signal
import socket
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

from praxion_harness.referee import Referee
from praxion_harness.server import TestServer
from praxion_harness.watch_client import client_command

REPO = Path(__file__).resolve().parents[2]
#: A copy of the release binaries can be used while another build runs.
RELEASE = Path(os.environ.get("PRAXION_RELEASE", REPO / "body/target/release"))
HELPER = REPO / "hands/macos/build/praxion-hands"
BRAIN_PY = REPO / "brain/.venv/bin/python"
CONSOLE_PORT = 8765
NAME = "Fatih"


def free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def stop(proc: subprocess.Popen, sig: int = signal.SIGTERM) -> None:
    if proc.poll() is None:
        proc.send_signal(sig)
        try:
            proc.wait(timeout=10)
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


def console_task(text: str) -> str:
    request = urllib.request.Request(
        f"http://127.0.0.1:{CONSOLE_PORT}/api/control",
        data=json.dumps({"action": "task", "text": text}).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(request, timeout=10) as response:
        return response.read().decode()


def main(args: argparse.Namespace) -> int:
    out = Path(args.out_dir)
    out.mkdir(parents=True, exist_ok=True)
    lines: list[str] = []

    def log(text: str) -> None:
        print(text, flush=True)
        lines.append(text)

    game = out / "client"
    game.mkdir(exist_ok=True)
    options = (REPO / "third_party/minecraft-client/game-watch/options.txt").read_text()
    patched = options.replace("pauseOnLostFocus:true", "pauseOnLostFocus:false")
    (game / "options.txt").write_text(patched)

    procs: list[subprocess.Popen] = []
    recorder = None
    try:
        with TestServer(out / "attempt", args.seed, keep=True, properties={
            "difficulty": "easy", "view-distance": 8, "simulation-distance": 6,
        }) as server:
            rcon = server.rcon
            rcon.command("gamerule doDaylightCycle false")
            rcon.command("time set 1000")
            game_port, tee_port, body_port = free_port(), free_port(), free_port()
            forwarder = subprocess.Popen(
                [str(RELEASE / "praxion-forwarder"), "--listen", f"127.0.0.1:{game_port}",
                 "--upstream", f"127.0.0.1:{server.port}", "--tee-listen", f"127.0.0.1:{tee_port}"],
                stdout=(out / "forwarder.txt").open("w"), stderr=subprocess.STDOUT)
            procs.append(forwarder)
            if not wait_listening(tee_port, 10):
                log("result=failed reason=forwarder_did_not_start")
                return 1

            cmd, _ = client_command(name=NAME, host="127.0.0.1", port=game_port)
            cmd[cmd.index("--gameDir") + 1] = str(game)
            client = subprocess.Popen(cmd, cwd=game, stdin=subprocess.DEVNULL,
                                      stdout=(out / "client.txt").open("w"), stderr=subprocess.STDOUT)
            procs.append(client)
            # The body must be on the tee before the client logs in: a late
            # subscriber gets a gap and a world it cannot trust.
            body_dir = out / "body"
            shutil.rmtree(body_dir, ignore_errors=True)
            body = subprocess.Popen(
                [str(RELEASE / "praxion-body"), "--real-client", "--tee", f"127.0.0.1:{tee_port}",
                 "--name", NAME, "--port", f"127.0.0.1:{body_port}", "--run-dir", str(body_dir),
                 "--hands", str(HELPER), "--client-pid", str(client.pid),
                 "--client-options", str(game / "options.txt"), "--hands-activate",
                 "--max-ticks", "1000000", "--skills", os.environ.get("PRAXION_SKILLS", str(REPO / "body/skills"))],
                stdout=(out / "body.txt").open("w"), stderr=subprocess.STDOUT)
            procs.insert(1, body)

            deadline = time.monotonic() + 120
            while time.monotonic() < deadline:
                if rcon.command("list").partition(": ")[2].strip() == NAME:
                    break
                if client.poll() is not None or body.poll() is not None:
                    break
                time.sleep(0.5)
            else:
                log("result=failed reason=client_did_not_join")
                return 1
            if body.poll() is not None:
                log(f"result=failed reason=body_exited code={body.returncode}")
                return 1
            log(f"joined server_port={server.port} game_port={game_port}")
            rcon.command(f"gamemode survival {NAME}")
            time.sleep(4)

            if not wait_listening(body_port, 30):
                log("result=failed reason=body_port_not_listening")
                return 1
            world = out.name
            brain = subprocess.Popen(
                [str(BRAIN_PY), "-m", "praxion_brain", "run", "--port", str(body_port), "--world", world],
                cwd=REPO / "brain", stdout=(out / "brain.txt").open("w"), stderr=subprocess.STDOUT)
            procs.append(brain)
            if not wait_listening(CONSOLE_PORT, 40):
                log("result=failed reason=console_not_listening")
                return 1

            seconds = int(args.minutes * 60)
            recorder = subprocess.Popen(
                ["screencapture", "-v", "-x", "-V", str(seconds), str(out / "screen.mov")],
                stdout=(out / "recorder.txt").open("w"), stderr=subprocess.STDOUT)
            time.sleep(3)
            for task in args.task:
                log(f"task {task!r} -> {console_task(task)}")
                time.sleep(2)
            referee = Referee(rcon, player=NAME)
            started = time.monotonic()
            while time.monotonic() - started < seconds:
                time.sleep(15)
                if body.poll() is not None or brain.poll() is not None:
                    log(f"stopped early body={body.poll()} brain={brain.poll()}")
                    break
                try:
                    log(f"t={int(time.monotonic() - started)} pos={referee.player_pos()} "
                        f"inv={referee.player_inventory(NAME)}")
                except Exception as error:  # noqa: BLE001 - one RCON miss must not end the recording
                    log(f"referee skipped: {error}")
            log(f"final inventory={referee.player_inventory(NAME)} deaths={referee.death_count(NAME)}")
    finally:
        if recorder is not None and recorder.poll() is None:
            recorder.send_signal(signal.SIGINT)
            try:
                recorder.wait(timeout=30)
            except subprocess.TimeoutExpired:
                recorder.kill()
        for proc in reversed(procs):
            stop(proc)
        (out / "result.txt").write_text("\n".join(lines) + "\n")
    return 0


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("out_dir")
    parser.add_argument("--seed", type=int, default=131469)
    parser.add_argument("--minutes", type=float, default=6.0)
    parser.add_argument("--task", action="append", default=[])
    parsed = parser.parse_args()
    if not HELPER.exists():
        print(f"build the helper first: {HELPER}")
        raise SystemExit(2)
    shutil.rmtree(Path(parsed.out_dir) / "attempt", ignore_errors=True)
    raise SystemExit(main(parsed))

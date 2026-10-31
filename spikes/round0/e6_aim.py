"""E6: aim within 1 degree in 150 ms, 100 random targets. Server-side rotation is the referee (RCON).

Each trial: read the current yaw/pitch, pick a random target (yaw delta in [-120, 120], pitch in
[-60, 60]), convert to mouse counts with the 26.3 formula (0.15 deg per count at sensitivity 0.5), send
them as 10 relative moves 10 ms apart (`hands mouse-move` per step size), wait 250 ms, read the rotation
again. Error = angular distance between target and result. Writes one JSON line per trial.
"""
import json
import random
import subprocess
import sys
import time

sys.path.insert(0, ".")
import rcon  # noqa: E402  (local helper)

HANDS = "../hands/target/release/hands.exe"
DEG_PER_COUNT = 0.15
STEPS = 10
LOG = "../../logs/e6-aim.jsonl"


def rotation() -> tuple[float, float]:
    import socket
    with socket.create_connection((rcon.HOST, rcon.PORT), timeout=5) as s:
        s.sendall(rcon.packet(1, 3, rcon.PASSWORD))
        rcon.read_packet(s)
        s.sendall(rcon.packet(2, 2, "data get entity Praxion Rotation"))
        text = rcon.read_packet(s)[2]
    yaw, pitch = text.split("[", 1)[1].rstrip("]").split(",")
    return float(yaw.strip().rstrip("f")), float(pitch.strip().rstrip("f"))


def wrap(a: float) -> float:
    return (a + 180.0) % 360.0 - 180.0


def move(dx: int, dy: int) -> int:
    """Send dx, dy counts spread over STEPS events. Returns the hands exit code."""
    xs = [dx // STEPS + (1 if i < dx % STEPS else 0) for i in range(STEPS)]
    ys = [dy // STEPS + (1 if i < dy % STEPS else 0) for i in range(STEPS)]
    # hands sends one fixed (dx, dy) per event, so send each distinct step pair as its own run.
    code = 0
    runs: list[tuple[int, int, int]] = []
    for x, y in zip(xs, ys):
        if runs and runs[-1][0] == x and runs[-1][1] == y:
            runs[-1] = (x, y, runs[-1][2] + 1)
        else:
            runs.append((x, y, 1))
    for x, y, n in runs:
        code = subprocess.run([HANDS, "mouse-move", "--counts", str(x), str(y), "--repeat", str(n),
                               "--interval-ms", "10", "--delay-secs", "0", "--max-secs", "10",
                               "--log", "../../logs/e6-hands.jsonl"]).returncode
        if code != 0:
            return code
    return code


def main() -> None:
    random.seed(7)
    move(1, 0)  # prime: the first relative event after a grab is dropped (observed in E5)
    time.sleep(0.3)
    hits = 0
    with open(LOG, "a", encoding="utf-8") as out:
        for trial in range(100):
            yaw0, pitch0 = rotation()
            dyaw = random.uniform(-120, 120)
            tpitch = random.uniform(-60, 60)
            dx = round(dyaw / DEG_PER_COUNT)
            dy = round((tpitch - pitch0) / DEG_PER_COUNT)
            target_yaw = wrap(yaw0 + dx * DEG_PER_COUNT)
            target_pitch = pitch0 + dy * DEG_PER_COUNT
            t0 = time.perf_counter()
            code = move(dx, dy)
            elapsed_ms = (time.perf_counter() - t0) * 1000
            if code != 0:
                print(f"hands exit {code}, stopping")
                break
            time.sleep(0.25)
            yaw1, pitch1 = rotation()
            err = max(abs(wrap(yaw1 - target_yaw)), abs(pitch1 - target_pitch))
            hits += err <= 1.0
            out.write(json.dumps({"trial": trial, "dx": dx, "dy": dy, "target": [target_yaw, target_pitch],
                                  "result": [yaw1, pitch1], "err_deg": err,
                                  "inject_wall_ms": elapsed_ms}) + "\n")
    print(f"E6: {hits}/100 within 1 degree")


if __name__ == "__main__":
    main()

"""Build the Praxion promo: timeline from the voice-over lengths, frames, three cuts.

    python3 build.py timeline            # write timeline.json
    python3 build.py render tr|en [--no-captions]   # frames -> out/<name>.video.mp4
    python3 build.py mux                 # the three final files

Cuts: praxion-tr-sessiz.mp4 (Turkish captions, no speech), praxion-tr.mp4 (Turkish
speech), praxion-en.mp4 (English speech). Everything stays inside video/promo/.
"""
from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent
FPS = 30
#: Seconds a scene lasts beyond its longer voice-over line, and its minimum.
PAD = {"s1": 1.6, "s2": 3.6, "s3": 3.6, "s4": 3.4, "s5": 3.0, "s6": 3.4, "s7": 4.2, "s8": 3.2}
MINIMUM = 5.0
VO_OFFSET = 0.45


def duration(path: Path) -> float:
    out = subprocess.run(
        ["ffprobe", "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", str(path)],
        capture_output=True, text=True, check=True,
    ).stdout.strip()
    return float(out)


def timeline() -> dict:
    script = json.loads((ROOT / "script.json").read_text(encoding="utf-8"))
    scenes: dict[str, list[float]] = {}
    captions: dict[str, list[dict]] = {"tr": [], "en": []}
    vo: dict[str, list[dict]] = {"tr": [], "en": []}
    cursor = 0.0
    for scene in script["scenes"]:
        sid = scene["id"]
        lengths = {lang: duration(ROOT / "vo" / f"{sid}_{lang}.mp3") for lang in ("tr", "en")}
        length = max(MINIMUM, max(lengths.values()) + PAD[sid])
        scenes[sid] = [round(cursor, 3), round(cursor + length, 3)]
        for lang in ("tr", "en"):
            start = cursor + VO_OFFSET
            vo[lang].append({"file": f"vo/{sid}_{lang}.mp3", "start": round(start, 3)})
            captions[lang].append(
                {"start": round(start, 3), "end": round(start + lengths[lang] + 0.5, 3), "text": scene[lang]}
            )
        cursor += length
    clips = {}
    for name in ("a", "b", "c"):
        frames = sorted((ROOT / "footage" / name).glob("*.jpg")) if (ROOT / "footage" / name).is_dir() else []
        clips[name] = {"frames": len(frames), "fps": FPS}
    data = {"fps": FPS, "duration": round(cursor, 3), "scenes": scenes, "captions": captions, "vo": vo, "clips": clips}
    (ROOT / "timeline.json").write_text(json.dumps(data, ensure_ascii=False, indent=1), encoding="utf-8")
    return data


def render(lang: str, captions: bool) -> Path:
    data = timeline()
    name = f"{lang}{'' if captions else '-nocap'}"
    frames = ROOT / "out" / f"frames-{name}"
    frames.mkdir(parents=True, exist_ok=True)
    subprocess.run(
        ["node", str(ROOT / "render.mjs"), lang, "1" if captions else "0", str(frames), str(data["duration"]), str(FPS)],
        check=True, cwd=ROOT,
    )
    video = ROOT / "out" / f"{name}.video.mp4"
    subprocess.run(
        ["ffmpeg", "-y", "-v", "error", "-framerate", str(FPS), "-i", str(frames / "%05d.jpg"),
         "-c:v", "libx264", "-pix_fmt", "yuv420p", "-crf", "17", "-preset", "medium", str(video)],
        check=True,
    )
    return video


def mux() -> None:
    data = json.loads((ROOT / "timeline.json").read_text(encoding="utf-8"))
    music = ROOT / "music.mp3"
    for lang, speech, out in (("tr", False, "praxion-tr-sessiz.mp4"), ("tr", True, "praxion-tr.mp4"), ("en", True, "praxion-en.mp4")):
        video = ROOT / "out" / f"{lang}.video.mp4"
        inputs: list[str] = ["-i", str(video)]
        filters: list[str] = []
        labels: list[str] = []
        index = 1
        if music.exists():
            inputs += ["-stream_loop", "-1", "-i", str(music)]
            level = 0.22 if speech else 0.6
            filters.append(f"[{index}:a]volume={level},atrim=0:{data['duration']},afade=t=out:st={data['duration'] - 2}:d=2[m]")
            labels.append("[m]")
            index += 1
        if speech:
            for line in data["vo"][lang]:
                inputs += ["-i", str(ROOT / line["file"])]
                delay = int(line["start"] * 1000)
                filters.append(f"[{index}:a]adelay={delay}|{delay}[v{index}]")
                labels.append(f"[v{index}]")
                index += 1
        command = ["ffmpeg", "-y", "-v", "error", *inputs]
        if labels:
            filters.append(f"{''.join(labels)}amix=inputs={len(labels)}:normalize=0:duration=longest[a]")
            command += ["-filter_complex", ";".join(filters), "-map", "0:v", "-map", "[a]", "-c:a", "aac", "-b:a", "192k"]
        else:
            command += ["-an"]
        command += ["-c:v", "copy", "-t", str(data["duration"]), str(ROOT / out)]
        subprocess.run(command, check=True)
        print(out, duration(ROOT / out))


if __name__ == "__main__":
    action = sys.argv[1] if len(sys.argv) > 1 else "timeline"
    if action == "timeline":
        print(json.dumps(timeline()["scenes"]))
    elif action == "render":
        print(render(sys.argv[2], "--no-captions" not in sys.argv))
    elif action == "mux":
        mux()
    else:
        raise SystemExit(__doc__)

#!/usr/bin/env python3
"""Drive the chat spike: one harness server, two headless clients, one talking.

    python3 spikes/chat/run_chat_spike.py [--seconds 20]

Answers the two load-bearing questions of session 9's talk design
(`docs/sessions/09-commands.md`, "2026-11-09 chat spike (macOS)"):

1. does the 26.3 offline server accept an unsigned `ServerboundChat` from a
   headless client and broadcast it?
2. which clientbound packet carries a player's message to another client, and
   which field identifies the sender?

The Rust binary does the talking and the decoding; this script owns the server,
the assertions over what came back, and the cleanup. It writes nothing to disk:
the run directory is deleted on the way out (stdout is the record). It reuses
`TestServer` (`harness/praxion_harness/server.py`) so the server properties are
the ones every other harness run uses.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import subprocess
import sys
import uuid
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
sys.path.insert(0, str(REPO / "harness"))

from praxion_harness.server import TestServer  # noqa: E402

BINARY = HERE / "target" / "debug" / "chat-spike"
RUN_DIR = HERE / "run"
SEED = 911

TALKER = "A"
LISTENER = "B"
TALKER_LINE = "chat spike: unsigned line from A"
LISTENER_LINE = "chat spike: line from B (the user)"
COMMAND_TEXT = "waves at the spike"

#: Server properties this spike pins on top of `TestServer`'s defaults
#: (`online-mode=false` is already the default). Offline mode with no secure
#: profile is the pair the design's risk 1 names.
PROPERTIES = {
    "online-mode": "false",
    "enforce-secure-profile": "false",
    "max-players": "4",
    "view-distance": "4",
    "simulation-distance": "4",
}

CHAT_PACKETS = {"player_chat", "disguised_chat", "system_chat"}


def offline_uuid(name: str) -> str:
    """The uuid an offline-mode server assigns: v3 of "OfflinePlayer:<name>".

    Java's `UUID.nameUUIDFromBytes`, i.e. an MD5 digest with the version and
    variant bits forced (`ServerPlayer` derives it from the login name when
    `online-mode=false`).
    """
    digest = bytearray(hashlib.md5(b"OfflinePlayer:" + name.encode()).digest())
    digest[6] = (digest[6] & 0x0F) | 0x30
    digest[8] = (digest[8] & 0x3F) | 0x80
    return str(uuid.UUID(bytes=bytes(digest)))


def run_client(addr: str, seconds: float) -> list[dict]:
    """Run the binary and return its JSON lines, echoing them as they arrive."""
    proc = subprocess.Popen(
        [str(BINARY), "--addr", addr, "--seconds", str(seconds)],
        cwd=str(HERE),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    events: list[dict] = []
    assert proc.stdout is not None
    for line in proc.stdout:
        line = line.rstrip("\n")
        print(f"  {line}", flush=True)
        try:
            events.append(json.loads(line))
        except json.JSONDecodeError:
            print(f"  (unparsed) {line}", flush=True)
    assert proc.stderr is not None
    errors = proc.stderr.read()
    code = proc.wait(timeout=120)
    if code != 0:
        print(f"  client exit {code}; stderr:\n{errors}", flush=True)
    return events


def by_client(events: list[dict], client: str, packet: str) -> list[dict]:
    return [
        event for event in events if event.get("client") == client and event.get("packet") == packet
    ]


def text_of(event: dict) -> str:
    """The message text of a chat packet: `body.content` or `message`."""
    return event.get("body_content") or event.get("text") or ""


def chat_line(events: list[dict], client: str, needle: str) -> dict | None:
    """The first chat packet of any kind that carried `needle`, for one client."""
    for event in events:
        if event.get("client") != client or event.get("packet") not in CHAT_PACKETS:
            continue
        if needle in text_of(event):
            return event
    return None


def describe(event: dict | None) -> str:
    if event is None:
        return "MISSING"
    fields = {k: v for k, v in event.items() if k not in {"t_ns", "client", "dir"}}
    return json.dumps(fields, sort_keys=True)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--seconds",
        type=float,
        default=20.0,
        help="how long the clients run; the first client's login waits for the world",
    )
    args = parser.parse_args(argv)

    if not BINARY.is_file():
        print(
            f"missing {BINARY}; build it first:\n"
            f"  cd {HERE.relative_to(REPO)} && "
            "CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 cargo build",
            file=sys.stderr,
        )
        return 2

    failures: list[str] = []
    try:
        with TestServer(RUN_DIR, SEED, properties=PROPERTIES, keep=False) as server:
            addr = f"127.0.0.1:{server.port}"
            print(f"server on {addr} (seed {SEED}), run dir {server.run_dir}", flush=True)
            print(
                f"client: {BINARY.relative_to(REPO)} --addr {addr} --seconds {args.seconds}",
                flush=True,
            )
            events = run_client(addr, args.seconds)
            log = (
                server.log_path.read_text(encoding="utf-8", errors="replace")
                if server.log_path.exists()
                else ""
            )
    finally:
        # `TestServer.stop` already removes the run dir; this is the belt for the
        # braces case where a run was interrupted and left logs behind.
        if RUN_DIR.exists():
            shutil.rmtree(RUN_DIR, ignore_errors=True)

    # ---------------------------------------------------------------- measurement
    play_logins = by_client(events, TALKER, "login") + by_client(events, LISTENER, "login")
    if len(play_logins) < 2:
        failures.append("both clients must reach play state (a `login` packet each)")
    flags = sorted(
        {(event.get("client"), event.get("online_mode"), event.get("enforces_secure_chat")) for event in play_logins},
        key=str,
    )
    login_uuids = {
        event["client"]: event.get("game_profile_uuid")
        for event in events
        if event.get("packet") == "login_finished"
    }

    server_log_chat = [
        line
        for line in log.splitlines()
        if f"<{TALKER}>" in line or f"<{LISTENER}>" in line or f"* {TALKER} " in line
    ]

    talker_to_listener = chat_line(events, LISTENER, TALKER_LINE)
    listener_to_talker = chat_line(events, TALKER, LISTENER_LINE)
    # A sender's own line comes back on its own connection (vanilla does that so
    # the sending client can show its own message).
    talker_own = chat_line(events, TALKER, TALKER_LINE)
    command_broadcast = chat_line(events, LISTENER, COMMAND_TEXT)

    print()
    print("RESULT")
    print(f"  login flags (client, online_mode, enforces_secure_chat): {flags}")
    print(f"  offline uuid of {TALKER}: {offline_uuid(TALKER)} (login said {login_uuids.get(TALKER)})")
    print(f"  offline uuid of {LISTENER}: {offline_uuid(LISTENER)} (login said {login_uuids.get(LISTENER)})")
    print(f"  server log lines mentioning {TALKER} or {LISTENER}:")
    for line in server_log_chat:
        print(f"    {line.strip()}")
    for label, found in (
        (f"{TALKER}'s line, seen by {LISTENER}", talker_to_listener),
        (f"{TALKER}'s line, echoed to {TALKER}", talker_own),
        (f"{LISTENER}'s line, seen by {TALKER}", listener_to_talker),
        (f"/me broadcast, seen by {LISTENER}", command_broadcast),
    ):
        print(f"  {label}: {describe(found)}")

    # ---------------------------------------------------------------- assertions
    # Measured on 2026-11-09 (docs/sessions/09-commands.md, "chat spike"): the
    # offline server accepts `ServerboundChat { signature: None }`, logs it as
    # `[Not Secure] <name> text`, and broadcasts it as `ClientboundPlayerChat`
    # with the sender's offline uuid and `signature = None`. Only commands
    # (`/me`) come back as `ClientboundDisguisedChat`, which has no uuid. These
    # assertions fail if a server or protocol change invalidates that.
    if not any(f"<{TALKER}> {TALKER_LINE}" in line for line in server_log_chat):
        failures.append(
            "the server log must contain the talker's line, i.e. the server accepted the "
            "unsigned ServerboundChat (an ignored chat leaves no log line)"
        )
    if talker_own is not None and talker_own["packet"] != "player_chat":
        failures.append(f"own chat came back as {talker_own['packet']}, expected player_chat")
    if talker_to_listener is not None:
        if talker_to_listener["packet"] != "player_chat":
            failures.append(
                f"expected player_chat for offline player chat, got {talker_to_listener['packet']}"
            )
        if talker_to_listener.get("sender") != offline_uuid(TALKER):
            failures.append(
                f"expected sender == offline uuid of {TALKER} ({offline_uuid(TALKER)}), "
                f"got {talker_to_listener.get('sender')}"
            )
        if talker_to_listener.get("signature"):
            failures.append("the message was unsigned on the wire, so signature must be absent")
        if talker_to_listener.get("body_content") != TALKER_LINE:
            failures.append(
                f"body.content must carry the text, got {talker_to_listener.get('body_content')!r}"
            )
    else:
        failures.append(f"the listener never received {TALKER}'s line")
    if listener_to_talker is None:
        failures.append(f"the talker never received {LISTENER}'s line")
    elif listener_to_talker.get("sender") != offline_uuid(LISTENER):
        failures.append(
            f"expected sender == offline uuid of {LISTENER} ({offline_uuid(LISTENER)}), "
            f"got {listener_to_talker.get('sender')}"
        )
    if command_broadcast is not None:
        if command_broadcast["packet"] != "disguised_chat":
            failures.append(
                f"expected disguised_chat for /me, got {command_broadcast['packet']}"
            )
        if "sender" in command_broadcast:
            failures.append("disguised_chat must not carry a sender uuid")
        if command_broadcast.get("chat_type_name") != TALKER:
            failures.append(
                f"expected chat_type_name == {TALKER!r}, got {command_broadcast.get('chat_type_name')!r}"
            )
    else:
        failures.append(f"the listener never received the /me broadcast")

    if failures:
        print()
        for failure in failures:
            print(f"FAIL: {failure}")
        return 1
    print()
    print(
        "OK: unsigned chat accepted; player chat arrives as player_chat with the sender uuid; "
        "/me arrives as disguised_chat with only the display name."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

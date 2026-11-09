# spikes/ — session 2.7 experiment code

Throwaway experiment code. Not product code. Plan and thresholds: `docs/sessions/02-body-connection.md` §2.7.
Results go to that file, not here.

Later one-question spikes that follow the same rules live here too; their result goes to the
session doc named next to them in the table.

## Layout

| Dir | What | Language |
|---|---|---|
| `round0/` | Server run dir and data reports (world ignored); the server jar lives in `third_party/minecraft/` | — |
| `proxy/` | E1: byte-forwarding TCP proxy with a tee to a capture file; `pingbench` | Rust, std only |
| `decode/` | E2, E3, E4: offline decoder of capture files with Azalea crates (path deps into `third_party/azalea/`) | Rust |
| `hands/` | E5–E13: Windows input injection, hooks, timer jitter | Rust, `windows-sys` |
| `chat/` | Session 9: two headless clients on one offline server, one unsigned chat line each direction; result in `docs/sessions/09-commands.md` | Rust + Python driver |
| `captures/` | Capture files (ignored) | — |

`chat/` is host-independent (no game window, no input injection), so it runs on macOS too:

```sh
cd spikes/chat
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 cargo build
python3 run_chat_spike.py     # starts the harness server itself and deletes its run directory
```

The pinned third-party inputs are outside this folder: the Azalea clone at commit `8041a14` in
`third_party/azalea/` and the server jar in `third_party/minecraft/` (both ignored, machine-local).
The patches are committed in `versions/patches/`.

Toolchain: `nightly-2026-08-20`, host `x86_64-pc-windows-gnu` (no MSVC on this PC). Every pin
(toolchain, Minecraft version, jar hashes, Azalea commit) is in `versions/manifest.toml`.

Setup (idempotent, one command): `python setup.py`; `python setup.py --check` only reports.
Details: `../SETUP.md`.

## Shared contracts

### Clock
Every timestamp in every spike is **nanoseconds since the UNIX epoch** from
`std::time::SystemTime::now()` (on Windows this is `GetSystemTimePreciseAsFileTime`, ~100 ns
resolution). This lets the proxy capture and the input-injector logs be joined on one clock.

### Ports
Server listens on `127.0.0.1:25566` (offline mode). Proxy listens on `127.0.0.1:25565` and forwards
to the server. The client connects to `localhost` (default port 25565).

### Capture file `*.prxcap` (written by `proxy`, read by `decode`)
All integers little-endian.
```
header : 8 bytes  b"PRXCAP1\n"
record : u64 ts_unix_ns   time the proxy's read() returned
         u32 conn_id      0, 1, 2 ... per accepted client connection
         u8  dir          0 = serverbound (client->server), 1 = clientbound (server->client),
                          2 = connection opened (len 0), 3 = connection closed (len 0)
         u32 len
         [len] bytes      the raw TCP chunk exactly as read; NOT re-framed, NOT decoded
```
The proxy never parses bytes (decision D1). Framing, decompression and state tracking happen
only in `decode`.

### JSON lines logs
Injector and hook logs are JSON lines, one object per event, each with `"t": <ts_unix_ns>`.

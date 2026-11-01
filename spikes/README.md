# spikes/ — session 2.7 experiment code

Throwaway experiment code. Not product code. Plan and thresholds: `docs/sessions/02-body-connection.md` §2.7.
Results go to that file, not here.

## Layout

| Dir | What | Language |
|---|---|---|
| `round0/` | Server jar, server run dir, data reports (jars and worlds ignored) | — |
| `azalea/` | Azalea clone at the pinned commit `8041a14` (ignored; re-clone to reproduce) | Rust |
| `proxy/` | E1: byte-forwarding TCP proxy with a tee to a capture file; `pingbench` | Rust, std only |
| `decode/` | E2, E3, E4: offline decoder of capture files with Azalea crates | Rust |
| `hands/` | E5–E13: Windows input injection, hooks, timer jitter | Rust, `windows-sys` |
| `captures/` | Capture files (ignored) | — |

Toolchain: `nightly-2026-08-20`, host `x86_64-pc-windows-gnu` (no MSVC on this PC). Every pin
(toolchain, Minecraft version, jar hashes, Azalea commit) is in `versions/manifest.toml`.

Setup (idempotent, one command): `python spikes/setup.py`; `python spikes/setup.py --check` only
reports. Details: `SETUP.md`.

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

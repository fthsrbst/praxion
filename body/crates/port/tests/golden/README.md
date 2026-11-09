# Golden frames

Each `.bin` file is one complete wire frame: a 4-byte big-endian payload
length followed by the UTF-8 JSON object. Both port test suites read these
files, so the Rust server and the Python client are pinned to the same bytes.

| File | Written by | Read by |
|---|---|---|
| `rust_*.bin` | the Rust test `body_frames_match_the_golden_pins` | `brain/tests/test_port.py::test_rust_body_frames_parse` |
| `brain_*.bin` | a Python run of `encode_frame` (see below) | the Rust test `python_brain_frames_replay_exactly` |

## Regenerating the Rust frames

The frames are a deterministic serialisation of the wire types in
`body/crates/port/src/wire.rs`:

```sh
cd body
CARGO_TARGET_DIR=target/w-port PRAXION_UPDATE_GOLDEN=1 cargo test -p praxion-port --release
```

The test writes every `rust_*.bin` when `PRAXION_UPDATE_GOLDEN` is set and
always compares the files it reads back. Review the diff before committing:
a changed frame is a wire change that the Python client must follow.

## Regenerating the Python frames

Run this from `brain/`, with `GOLDEN_DIR` pointing at this directory:

```python
from pathlib import Path
from praxion_brain.port import (
    ClientHello, Command, Ping, Pop, ResyncRequest, Say, encode_frame,
)

GOLDEN_DIR = Path("../body/crates/port/tests/golden")
frames = {
    "brain_hello.bin": ClientHello(),
    "brain_command.bin": Command(
        id="c-0007",
        skill="get_furnace",
        args=["minecraft:furnace"],
        tier="command",
        expires_at_tick=4621,
        based_on_generation=7,
        condition="count(furnace) >= 1",
        budget_ticks=6000,
    ),
    "brain_pop.bin": Pop(id="c-0008", intent_id=3),
    "brain_say.bin": Say(id="s-0007", text="bitti: 8 taş", priority="urgent"),
    "brain_resync_request.bin": ResyncRequest(),
    "brain_ping.bin": Ping(),
}
for name, message in frames.items():
    (GOLDEN_DIR / name).write_bytes(encode_frame(message))
```

`test_python_brain_frames_are_pinned` asserts the same bytes, so a drift in
field order, defaults or number formatting fails on the Python side first.

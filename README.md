# Praxion

**A Minecraft agent that learns by playing, and writes its own skills along the way.**

Praxion is an autonomous Minecraft agent. It can join your singleplayer world as a companion
or play your own character, take direct commands, and play on its own. Instead of relying on a large model for every
move, it turns what it learns into reusable, verified skills: code it writes, tests and keeps
in a growing library. The more it plays, the less it needs to think from scratch.

> *Praxis*: learning through practice.

## Status

Demo-0 is in progress. The body is a Rust workspace under [`body/`](body/): forwarder, decoder,
world model, perception, reflexes, arbiter, rule-list interpreter and primitives, driven by a
headless packet client. The test harness is Python under [`harness/`](harness/), and `setup.py`
prepares the pinned Minecraft, Azalea and Rust inputs. Research is in
[`docs/research/`](docs/research/); agreed decisions are in [`docs/decisions.md`](docs/decisions.md);
the architecture is in [`docs/architecture.md`](docs/architecture.md).

## License

Apache-2.0 for all code (see `LICENSE`). There are no commercial or closed parts.

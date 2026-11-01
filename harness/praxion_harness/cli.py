"""Command-line interface: ``python -m praxion_harness ...``."""

from __future__ import annotations

import argparse
import sys
import time

from .cleanup import clean_runs
from .paths import repo_root
from .rcon import RconClient
from .referee import Referee
from .server import TestServer


def _cmd_serve(args) -> int:
    run_id = f"cli-{int(time.time())}"
    run_dir = repo_root() / "runs" / run_id
    srv = TestServer(run_dir, args.seed, keep=args.keep)
    with srv:
        print(
            f"run_dir={srv.run_dir} port={srv.port} rcon_port={srv.rcon_port} "
            f"log={srv.log_path}",
            flush=True,
        )
        try:
            while True:
                time.sleep(1)
        except KeyboardInterrupt:
            pass
    if srv.keep:
        print(f"kept run directory: {srv.run_dir}", flush=True)
    return 0


def _query(client: RconClient, fact: list[str]) -> str:
    """Resolve a named fact to a server query, or return a raw command's reply."""
    if not fact:
        raise ValueError("no fact given")
    name, *rest = fact
    ref = Referee(client)
    if name == "raw":
        if not rest:
            raise ValueError("raw needs a command")
        return client.command(" ".join(rest))
    if name in {"time", "daytime"}:
        return str(ref.time_of_day())
    if name == "day":
        return str(ref.day_count())
    if name == "gametime":
        return str(ref.game_time())
    if name == "is-night":
        return str(ref.is_night())
    if name == "block":
        if len(rest) != 3:
            raise ValueError("block needs: <x> <y> <z>")
        return ref.block_at(*rest)
    if name == "is-block":
        if len(rest) != 4:
            raise ValueError("is-block needs: <x> <y> <z> <block>")
        return str(ref.block_is(*rest))
    if name == "gamerule":
        if len(rest) != 1:
            raise ValueError("gamerule needs: <name>")
        return client.command(f"gamerule {rest[0]}")
    if name == "player":
        if len(rest) != 2:
            raise ValueError("player needs: <name> health|food|pos|inventory|dimension")
        player, what = rest
        methods = {
            "health": ref.player_health,
            "food": ref.player_food,
            "pos": ref.player_pos,
            "inventory": ref.player_inventory,
            "dimension": ref.player_dimension,
        }
        if what not in methods:
            raise ValueError(f"unknown player fact: {what}")
        return str(methods[what](player))
    raise ValueError(f"unknown fact: {name}")


def _cmd_query(args) -> int:
    fact = list(args.fact or [])
    client = RconClient(host="127.0.0.1", port=args.rcon_port, timeout=10.0)
    try:
        client.connect()
        print(_query(client, fact))
    except ValueError as exc:
        print(f"error: {exc}", file=sys.stderr)
        print(
            "facts: time|daytime, day, gametime, is-night, block <x> <y> <z>, "
            "is-block <x> <y> <z> <block>, gamerule <name>, "
            "player <name> health|food|pos|inventory|dimension, raw <command>",
            file=sys.stderr,
        )
        return 2
    finally:
        client.close()
    return 0


def _cmd_clean_runs(args) -> int:
    deleted, skipped = clean_runs(
        older_than_hours=args.older_than_hours, dry_run=args.dry_run
    )
    verb = "would delete" if args.dry_run else "deleted"
    for path in deleted:
        print(f"{verb}: {path}")
    for path in skipped:
        print(f"skipped (in use or not removable): {path}")
    print(f"{len(deleted)} {verb}, {len(skipped)} skipped")
    return 0


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(prog="praxion_harness")
    sub = parser.add_subparsers(dest="command", required=True)

    p_serve = sub.add_parser("serve", help="start a test server and wait for Ctrl+C")
    p_serve.add_argument("--seed", required=True)
    p_serve.add_argument("--keep", action="store_true",
                         help="keep the run directory after stopping")
    p_serve.set_defaults(func=_cmd_serve)

    p_query = sub.add_parser("query", help="query a named fact or run a raw rcon command")
    p_query.add_argument("--port", type=int, default=None,
                         help="game port (accepted for compatibility; RCON uses --rcon-port)")
    p_query.add_argument("--rcon-port", dest="rcon_port", type=int, required=True)
    p_query.add_argument("fact", nargs="*")
    p_query.set_defaults(func=_cmd_query)

    p_clean = sub.add_parser("clean-runs", help="delete stale harness run directories")
    p_clean.add_argument("--older-than-hours", type=float, default=None,
                         help="only directories older than this many hours")
    p_clean.add_argument("--dry-run", action="store_true",
                         help="print what would be deleted")
    p_clean.set_defaults(func=_cmd_clean_runs)

    args = parser.parse_args(argv)
    return int(args.func(args))


if __name__ == "__main__":
    sys.exit(main())

"""Command line entry points for the brain package."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

from . import config
from .admission import admit
from .console.hub import Console
from .console.server import DEFAULT_PORT, ConsoleServer
from .evidence import Evidence
from .goal import Goal
from .governor import Governor
from .ledger import BudgetExceeded, Ledger
from .loop import Loop
from .memory import Memory
from .port import PortClient
from .router import route
from .skills import check as run_check
from .skills import load_catalog


def _seeds(raw: str) -> tuple[int, ...]:
    try:
        return tuple(int(part.strip()) for part in raw.split(",") if part.strip())
    except ValueError as error:
        raise argparse.ArgumentTypeError(f"seeds must be comma-separated integers: {raw!r}") from error


def _print_json(value: object) -> None:
    print(json.dumps(value, indent=2, default=str))


def _cmd_route(args: argparse.Namespace) -> int:
    goal = Goal(text=args.goal, item=args.item, count=args.count)
    catalog = load_catalog()
    evidence = Evidence(config.local_brain_dir())
    ledger = Ledger()
    result = route(goal, catalog, ledger=ledger, evidence=evidence)
    _print_json(result.model_dump())
    return 0


def _cmd_learn(args: argparse.Namespace) -> int:
    goal = Goal(text=args.goal, item=args.item, count=args.count)
    catalog = load_catalog()
    evidence = Evidence(config.local_brain_dir())
    ledger = Ledger()

    routed = route(goal, catalog, ledger=ledger, evidence=evidence)
    _print_json({"route": routed.model_dump()})
    if routed.kind == "play":
        print(f"play: skills.{routed.skill}")
        return 0
    if routed.kind == "ask":
        print("ask: the writer decides (treated as write in the slice)")

    report = admit(
        goal,
        args.model,
        seeds=args.seeds,
        held_out=args.held_out,
        max_repairs=args.max_repairs,
        max_ticks=args.max_ticks,
        evidence=evidence,
        ledger=ledger,
        catalog=catalog,
    )
    _print_json(report.model_dump())
    print(f"status: {report.status}")
    if report.active_path:
        print(f"active: {report.active_path}")
    if report.report_path:
        print(f"report: {report.report_path}")
    return 0 if report.status == "admitted" else 1


def _cmd_check(args: argparse.Namespace) -> int:
    report = run_check(Path(args.directory))
    _print_json(report.model_dump())
    return 0 if report.ok else 1


def _cmd_spent(args: argparse.Namespace) -> int:
    ledger = Ledger()
    totals = ledger.totals()
    providers = sorted(set(totals) | {"openai", "typesafe"})
    for provider in providers:
        amount = totals.get(provider, 0.0)
        try:
            budget = ledger.budget(provider)
        except ValueError:
            print(f"{provider}: ${amount:.6f}")
        else:
            print(f"{provider}: ${amount:.6f} / ${budget:.2f}")
    print(f"total: ${sum(totals.values()):.6f}")
    return 0


def _cmd_run(args: argparse.Namespace) -> int:
    ledger = Ledger()
    evidence = Evidence(config.local_brain_dir())
    world_id = args.world or "default"
    memory = Memory(world_id, evidence=evidence)
    catalog = load_catalog()
    governor = Governor(ledger, evidence=evidence)
    port = PortClient(args.port, host=args.host)
    console = Console(evidence=evidence, ledger=ledger, memory=memory, world_id=world_id)
    server = None
    if args.console_port != 0:
        server = ConsoleServer(console, args.console_port).start()
        print(f"console: {server.url}", flush=True)
    loop = Loop(
        port,
        world_id=world_id,
        memory=memory,
        catalog=catalog,
        ledger=ledger,
        evidence=evidence,
        governor=governor,
        console=console,
    )
    print(
        f"loop: world {world_id}, port {args.host}:{args.port}, "
        f"hard stop ${governor.hard_stop_usd:.2f}"
    )
    loop.start()
    try:
        loop.run()
    except KeyboardInterrupt:
        print("loop: interrupted")
    finally:
        loop.close()
        if server is not None:
            server.shutdown()
    return 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="praxion-brain", description="Praxion brain")
    commands = parser.add_subparsers(dest="command", required=True)

    route_parser = commands.add_parser("route", help="ask Jev whether a known skill reaches the goal")
    route_parser.add_argument("goal", help="goal text, for example \"have a furnace\"")
    route_parser.add_argument("--item", required=True, help="the item the goal must produce")
    route_parser.add_argument("--count", type=int, default=1)
    route_parser.set_defaults(func=_cmd_route)

    learn_parser = commands.add_parser("learn", help="route, then write and admit a skill")
    learn_parser.add_argument("goal", help="goal text, for example \"have a furnace\"")
    learn_parser.add_argument("--item", required=True, help="the item the goal must produce")
    learn_parser.add_argument("--count", type=int, default=1)
    learn_parser.add_argument("--model", default="gpt-6-luna")
    learn_parser.add_argument("--seeds", type=_seeds, default=(101, 202, 303))
    learn_parser.add_argument("--held-out", type=int, default=404)
    learn_parser.add_argument("--max-repairs", type=int, default=3)
    learn_parser.add_argument("--max-ticks", type=int, default=12_000)
    learn_parser.set_defaults(func=_cmd_learn)

    check_parser = commands.add_parser("check", help="run the body's check-skills on a directory")
    check_parser.add_argument("directory")
    check_parser.set_defaults(func=_cmd_check)

    spent_parser = commands.add_parser("spent", help="print the ledger totals")
    spent_parser.set_defaults(func=_cmd_spent)

    run_parser = commands.add_parser(
        "run", help="run the autonomous loop against a body port"
    )
    run_parser.add_argument("--port", type=int, required=True, help="the body port")
    run_parser.add_argument(
        "--world",
        default=None,
        help="world id under local/worlds/ (default: \"default\")",
    )
    run_parser.add_argument("--host", default="127.0.0.1")
    run_parser.add_argument(
        "--console-port",
        type=int,
        default=DEFAULT_PORT,
        help="web console port on 127.0.0.1 (default: 8765; 0 disables it)",
    )
    run_parser.set_defaults(func=_cmd_run)

    return parser


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    try:
        return int(args.func(args))
    except BudgetExceeded as error:
        print(f"budget: {error}", file=sys.stderr)
        return 2
    except Exception as error:  # the CLI reports context instead of a traceback
        print(f"error: {error}", file=sys.stderr)
        return 2


__all__ = ["build_parser", "main"]

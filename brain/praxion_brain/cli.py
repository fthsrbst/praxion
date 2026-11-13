"""Command line entry points for the brain package."""

from __future__ import annotations

import argparse
import json
import signal
import sys
import threading
from pathlib import Path

from . import config, needs
from .admission import admit
from .console.run import SessionManager, build_runtime
from .console.server import DEFAULT_PORT, ConsoleServer
from .evidence import Evidence
from .goal import Goal
from .ledger import BudgetExceeded, Ledger
from .memory import Memory
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


def _cmd_wiki(args: argparse.Namespace) -> int:
    from .wiki import commands as wiki_commands

    return wiki_commands.run(args.action, args.query, limit=args.limit)


def _cmd_run(args: argparse.Namespace) -> int:
    world_id = args.world or "default"
    ledger = Ledger()
    evidence = Evidence(config.local_brain_dir())
    memory = Memory(world_id, evidence=evidence)
    catalog = load_catalog()
    # The recipe export keeps the proposer from stocking what it already holds.
    # Absent: today's behaviour. From another jar: a startup error (regenerate
    # it with `praxion-body export-gamedata`).
    gamedata = needs.load()
    runtime = build_runtime(
        port=args.port,
        host=args.host,
        world_id=world_id,
        evidence=evidence,
        ledger=ledger,
        memory=memory,
        catalog=catalog,
        gamedata=gamedata,
    )
    print(
        "gamedata: "
        + (f"jar {gamedata.jar_sha256[:12]}" if gamedata else "no export (need checks off)"),
        flush=True,
    )
    server = None
    if args.console_port != 0:
        server = ConsoleServer(runtime.console, args.console_port).start()
        print(f"console: {server.url}", flush=True)
    print(
        f"loop: world {world_id}, port {args.host}:{args.port}, "
        f"hard stop ${runtime.governor.hard_stop_usd:.2f}"
    )
    runtime.loop.start()
    try:
        runtime.loop.run()
    except KeyboardInterrupt:
        print("loop: interrupted")
    finally:
        runtime.loop.close()
        if server is not None:
            server.shutdown()
    return 0


def _cmd_console(args: argparse.Namespace) -> int:
    """Serve the console; Start server and Start bot own the local stack."""
    manager = SessionManager(
        seed=args.seed,
        operator=args.operator,
        peaceful=args.peaceful,
    )
    server = ConsoleServer(
        port=args.console_port,
        manager=manager,
        ledger=manager.ledger,
    ).start()
    manager.attach_server(server)
    print(
        f"console: {server.url}\n"
        f"run control: Start server opens seed {args.seed} "
        f"({'peaceful' if args.peaceful else 'survival'}, operator {args.operator}); "
        f"Start bot joins it; Join Minecraft works as soon as the server is ready",
        flush=True,
    )
    done = threading.Event()

    def stop(*_signal: object) -> None:
        done.set()

    previous_int = signal.signal(signal.SIGINT, stop)
    previous_term = signal.signal(signal.SIGTERM, stop)
    try:
        # The HTTP threads do all the work; this thread is only the lifetime.
        while not done.wait(0.5):
            pass
    finally:
        # The handlers stay installed until teardown ends, so a signal that
        # arrives while the session stops cannot surface as an exception. The
        # HTTP server is closed even if the session cleanup raises.
        try:
            manager.shutdown()
        finally:
            try:
                server.shutdown()
            finally:
                signal.signal(signal.SIGINT, previous_int)
                signal.signal(signal.SIGTERM, previous_term)
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

    wiki_parser = commands.add_parser(
        "wiki", help="local Minecraft Wiki RAG: fetch, build, search, eval"
    )
    wiki_parser.add_argument(
        "action", choices=["fetch", "build", "search", "eval"], help="what to do"
    )
    wiki_parser.add_argument(
        "query", nargs="?", default="", help="the question for `wiki search`"
    )
    wiki_parser.add_argument(
        "--limit", type=int, default=None, help="fetch only the first N titles"
    )
    wiki_parser.set_defaults(func=_cmd_wiki)

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

    console_parser = commands.add_parser(
        "console",
        help="serve the web console that starts and stops the local server and bot",
    )
    console_parser.add_argument(
        "--console-port",
        type=int,
        default=DEFAULT_PORT,
        help="web console port on 127.0.0.1 (default: 8765; 0 picks an ephemeral port)",
    )
    console_parser.add_argument("--seed", type=int, default=303, help="the server seed")
    console_parser.add_argument(
        "--operator",
        default="Fatih",
        help="the player the bot obeys and Join starts the client as",
    )
    console_parser.add_argument(
        "--peaceful",
        action="store_true",
        help="the old frozen daytime with no mobs (for debugging)",
    )
    console_parser.set_defaults(func=_cmd_console)

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

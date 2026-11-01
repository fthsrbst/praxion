import pytest

from praxion_harness.logtail import parse_line, LogTail, PlayerJoined, PlayerLeft, PlayerDied, ChatMessage, ServerDone
from praxion_harness.paths import PathResolutionError, resolve_server_jar


def _jar():
    try:
        return resolve_server_jar()
    except PathResolutionError:
        pytest.skip("server jar not available")


def test_parse_join_leave_chat_done():
    assert isinstance(parse_line("[13:05:59] [Server thread/INFO]: Steve joined the game"), PlayerJoined)
    assert parse_line("[x] [Server thread/INFO]: Steve joined the game").name == "Steve"
    assert isinstance(parse_line("[x] [Server thread/INFO]: Steve left the game"), PlayerLeft)
    ev = parse_line("[x] [Server thread/INFO]: <Steve> hello world")
    assert isinstance(ev, ChatMessage) and ev.sender == "Steve" and ev.message == "hello world"
    assert isinstance(parse_line("[x] [Server thread/INFO]: Done (0.666s)! For help, type \"help\""), ServerDone)


def test_parse_death_real_lines():
    jar = _jar()
    lines = [
        "[x] [Server thread/INFO]: Steve was slain by Zombie",
        "[x] [Server thread/INFO]: Steve fell from a high place",
        "[x] [Server thread/INFO]: Steve burned to death",
    ]
    for ln in lines:
        ev = parse_line(ln, jar_path=jar)
        assert isinstance(ev, PlayerDied), ln
        assert ev.name == "Steve"


def test_logtail_poll(tmp_path):
    log = tmp_path / "server.log"
    log.write_text("[x] [Server thread/INFO]: Alice joined the game\n", encoding="utf-8")
    lt = LogTail(log)
    evs = lt.poll()
    assert len(evs) == 1 and isinstance(evs[0], PlayerJoined)
    # Second poll returns nothing new.
    assert lt.poll() == []


def test_death_templates_count():
    from praxion_harness.logtail import load_death_templates
    ts = load_death_templates(_jar())
    assert len(ts) > 50

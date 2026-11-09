from __future__ import annotations

import http.client
import json
import re
import threading
from datetime import datetime, timezone
from pathlib import Path

import pytest

from fake_body import FakeBody
from praxion_brain.console.hub import Console
from praxion_brain.console.server import ConsoleServer
from praxion_brain.evidence import Evidence
from praxion_brain.ledger import Ledger
from praxion_brain.loop import LoopState
from praxion_brain.memory import GoalRecord, Memory
from praxion_brain.router import Route
from test_loop import (
    admission_options,
    catalog,
    evidence_entries,
    furnace_check_fn,
    furnace_write_fn,
    gated_practice,
    make_loop,
    open_record,
    proposal,
    pump,
    wait_accepted,
)

SKILL = 'skill get_wood()\n  doc "Collect logs."\n  rules:\n    count(#logs) >= 1 -> done\n    true -> do mine(#logs)\n'


def build_console(tmp_path: Path) -> Console:
    """A console over the same stores ``make_loop`` uses for ``tmp_path``."""
    core = tmp_path / "core"
    core.mkdir(exist_ok=True)
    (core / "get_wood.skill").write_text(SKILL, encoding="utf-8")
    active = tmp_path / "active"
    active.mkdir(exist_ok=True)
    return Console(
        evidence=Evidence(tmp_path / "evidence"),
        ledger=Ledger(tmp_path / "ledger.jsonl"),
        memory=Memory("w", root=tmp_path / "worlds"),
        world_id="w",
        core_dir=str(core),
        active_dir=str(active),
    )


@pytest.fixture
def console(tmp_path):
    return build_console(tmp_path)


@pytest.fixture
def server(console):
    running = ConsoleServer(console, 0).start()
    yield running
    running.shutdown()


def call(server, method, path, body=None, headers=None):
    connection = http.client.HTTPConnection("127.0.0.1", server.port, timeout=5)
    payload = json.dumps(body) if body is not None else None
    connection.request(method, path, body=payload, headers=headers or {})
    response = connection.getresponse()
    raw = response.read()
    connection.close()
    return response.status, json.loads(raw) if raw else None


# --------------------------------------------------------------------------- #
# Endpoints
# --------------------------------------------------------------------------- #
def test_state_merges_published_state_with_budget_and_rungs(server, console) -> None:
    console.publish({"world_id": "w", "state": "playing", "paused": False})
    console.ledger.record("openai", "gpt-6-luna", 1000, 100, "propose_goal")
    status, state = call(server, "GET", "/api/state")
    assert status == 200
    assert state["state"] == "playing"
    assert state["budget"]["openai"]["spent"] > 0
    assert state["budget"]["openai"]["limit"] > 0
    assert [r["id"] for r in state["rungs"]][0] == "wood"


def test_evidence_pages_by_since_kind_and_limit(server, console) -> None:
    for index in range(6):
        console.evidence.write("loop_transition" if index % 2 else "command_sent", i=index)
    _, newest = call(server, "GET", "/api/evidence?limit=2")
    assert [line["i"] for line in newest["lines"]] == [4, 5]
    assert newest["last"] == 6
    _, after = call(server, "GET", "/api/evidence?since=4")
    assert [line["n"] for line in after["lines"]] == [5, 6]
    _, only = call(server, "GET", "/api/evidence?kinds=command_sent")
    assert [line["i"] for line in only["lines"]] == [0, 2, 4]


def test_evidence_list_shortens_long_strings(server, console) -> None:
    console.evidence.write("proposal_request", system="x" * 5000)
    _, found = call(server, "GET", "/api/evidence")
    assert len(found["lines"][0]["system"]) < 600


def test_exchange_pairs_request_and_response_from_any_line(server, console) -> None:
    console.evidence.write(
        "router_request", provider="typesafe", model="jev-latest", state={"goal": "g"}, questions={"q": 1}
    )
    console.evidence.write(
        "router_response",
        provider="typesafe",
        model="jev-latest",
        scores={"get_wood": 0.9, "get_planks": 0.1},
        input_tokens=800,
        output_tokens=100,
    )
    console.evidence.write("router_decision", decision="play", skill="get_wood", scores={})
    console.evidence.write("loop_route", decision="play", skill="get_wood", scores={})
    for line in (1, 2, 3, 4):
        status, found = call(server, "GET", f"/api/exchange/{line}")
        assert status == 200, line
        assert found["kind"] == "router"
        assert found["request"]["questions"] == {"q": 1}
        assert found["response"]["scores"]["get_wood"] == 0.9
        assert found["decision"]["skill"] == "get_wood"
        assert found["input_tokens"] == 800
        assert found["cost"] > 0
    assert call(server, "GET", "/api/exchange/99")[0] == 404
    assert call(server, "GET", "/api/exchange/abc")[0] == 400


def test_goals_endpoint_lists_this_worlds_records(server, console) -> None:
    open_record(console.memory, goal_id="g-0001", command_id="c-0001", condition="has(furnace)")
    _, found = call(server, "GET", "/api/goals")
    assert [goal["goal_id"] for goal in found] == ["g-0001"]


def test_skills_split_core_learned_and_candidates(server, console, tmp_path) -> None:
    (tmp_path / "active" / "get_furnace.skill").write_text(
        SKILL.replace("get_wood", "get_furnace"), encoding="utf-8"
    )
    # Admission writes candidates under the evidence directory (local/brain/attempts).
    candidate = tmp_path / "evidence" / "attempts" / "cand.skill"
    candidate.parent.mkdir(parents=True)
    candidate.write_text(SKILL.replace("get_wood", "get_coal"), encoding="utf-8")

    def report(name, status, stamp):
        path = tmp_path / "evidence" / f"admission-{name}-{stamp}.json"
        path.parent.mkdir(exist_ok=True)
        path.write_text(
            json.dumps(
                {
                    "skill_name": name,
                    "status": status,
                    "goal": {"text": "g", "item": "x", "count": 1},
                    "repairs_used": 1,
                    "attempts": [
                        {
                            "attempt": 0,
                            "model": "m",
                            "candidate_path": str(candidate),
                            "static_ok": True,
                            "failure_kind": None,
                            "practices": [
                                {"seed": 101, "passed": status == "admitted", "outcome": "done", "ticks": 50, "digest": "d"}
                            ],
                        }
                    ],
                }
            ),
            encoding="utf-8",
        )

    report("get_furnace", "admitted", "20261108T100000Z")
    report("get_coal", "rejected", "20261108T110000Z")
    _, found = call(server, "GET", "/api/skills")
    assert [s["name"] for s in found["core"]] == ["get_wood"]
    assert [s["name"] for s in found["learned"]] == ["get_furnace"]
    assert found["learned"][0]["report"]["attempts"][0]["practices"][0]["seed"] == 101
    assert [c["skill"] for c in found["candidates"]] == ["get_coal"]
    assert "get_coal" in found["candidates"][0]["attempts"][0]["source"]


def test_metrics_series(server, console) -> None:
    console.ledger.record("openai", "gpt-6-luna", 1000, 100, "propose_goal")
    memory = console.memory
    for goal_id, status, ticks in (("g-0001", "done", 100), ("g-0002", "done", 300), ("g-0003", "failed", 50)):
        memory.append_goal(
            GoalRecord(goal_id=goal_id, text="t", condition="has(x)", status=status, skill="get_wood", ticks=ticks)
        )
    console.evidence.write("goal_finished", goal_id="g-0001", rungs_met=2)
    _, found = call(server, "GET", "/api/metrics")
    assert found["cost_per_hour"][-1]["calls"] == 1
    assert found["goals_by_skill"]["get_wood"] == {"done": 2, "failed": 1}
    assert found["ticks"]["median"] == 200
    assert found["rungs"][0]["rungs"] == 2
    assert found["totals"] == {"goals": 3, "done": 2, "failed": 1}


def test_static_index_is_served_and_traversal_refused(server) -> None:
    connection = http.client.HTTPConnection("127.0.0.1", server.port, timeout=5)
    connection.request("GET", "/")
    response = connection.getresponse()
    assert response.status == 200
    assert b"Praxion" in response.read()
    connection.request("GET", "/..%2f..%2fcli.py")
    refused = connection.getresponse()
    refused.read()
    assert refused.status == 404


def test_every_response_forbids_framing(server, console) -> None:
    console.evidence.write("loop_started", world_id="w")

    def framed(path, headers=None, read=True):
        connection = http.client.HTTPConnection("127.0.0.1", server.port, timeout=5)
        connection.request("GET", path, headers=headers or {})
        response = connection.getresponse()
        if read:
            response.read()
        connection.close()
        return response

    for path in ("/", "/api/state", "/api/nope"):
        response = framed(path)
        assert response.getheader("X-Frame-Options") == "DENY"
        csp = response.getheader("Content-Security-Policy")
        assert "frame-ancestors 'none'" in csp
        assert "default-src 'self'" in csp
        assert "https://fonts.googleapis.com" in csp  # the UI's font stylesheet
        assert "https://fonts.gstatic.com" in csp  # its font files
    stream = framed("/api/stream", read=False)
    assert stream.getheader("X-Frame-Options") == "DENY"
    assert "frame-ancestors 'none'" in stream.getheader("Content-Security-Policy")


def test_internal_error_does_not_echo_a_local_path(server, monkeypatch) -> None:
    from praxion_brain.console import server as console_server

    def boom(_console):
        raise RuntimeError(r"cannot read C:\Users\fatih\praxion\local\brain\brain.jsonl")

    monkeypatch.setattr(console_server.data, "state", boom)
    status, found = call(server, "GET", "/api/state")
    assert status == 500
    assert "RuntimeError" in found["error"]
    assert "C:" not in found["error"] and "local" not in found["error"]


def test_state_and_metrics_parse_each_new_goal_line_only_once(server, console) -> None:
    open_record(console.memory, goal_id="g-0001", command_id="c-0001", condition="has(furnace)")
    assert call(server, "GET", "/api/state")[0] == 200
    parsed = console.memory.goals_parsed
    assert parsed == 1

    assert call(server, "GET", "/api/state")[0] == 200
    assert call(server, "GET", "/api/metrics")[0] == 200
    assert console.memory.goals_parsed == parsed, "a second read re-parsed the history"

    open_record(console.memory, goal_id="g-0002", command_id="c-0002", condition="has(coal)")
    assert call(server, "GET", "/api/state")[0] == 200
    assert console.memory.goals_parsed == parsed + 1


def test_evidence_index_reuses_one_read_handle(server, console) -> None:
    for index in range(3):
        console.evidence.write("command_sent", i=index)
    log = server.log
    assert [entry["i"] for entry in log.tail(limit=3)] == [0, 1, 2]
    handle = log._handle
    assert handle is not None and not handle.closed
    assert [entry["n"] for entry in log.tail(limit=3)] == [1, 2, 3]
    assert [entry["n"] for entry in log.tail(since=1)] == [2, 3]
    assert [log.entry(number)["i"] for number in (1, 2, 3)] == [0, 1, 2]
    assert log._handle is handle, "entry() reopened the file"


# --------------------------------------------------------------------------- #
# Control
# --------------------------------------------------------------------------- #
def test_control_actions_reach_the_queue(server, console) -> None:
    assert call(server, "POST", "/api/control", {"action": "pause"})[0] == 202
    assert call(server, "POST", "/api/control", {"action": "stop_goal"})[0] == 202
    status, _ = call(
        server, "POST", "/api/control", {"action": "new_goal", "text": "get coal", "condition": "has(coal)"}
    )
    assert status == 202
    assert console.control.drain() == [
        {"action": "pause"},
        {"action": "stop_goal"},
        {"action": "new_goal", "text": "get coal", "condition": "has(coal)"},
    ]
    assert console.control.drain() == []


@pytest.mark.parametrize(
    "body",
    [
        {"action": "explode"},
        {"action": "new_goal"},
        {"action": "new_goal", "text": "x", "condition": "has(("},
        {"action": "new_goal", "text": "y" * 500},
        ["pause"],
    ],
)
def test_control_refuses_bad_requests(server, console, body) -> None:
    status, found = call(server, "POST", "/api/control", body)
    assert status == 400
    assert "error" in found
    assert console.control.drain() == []


def test_foreign_host_and_origin_are_refused(server, console) -> None:
    assert call(server, "GET", "/api/state", headers={"Host": "evil.example"})[0] == 403
    status, _ = call(
        server, "POST", "/api/control", {"action": "pause"}, headers={"Origin": "http://evil.example"}
    )
    assert status == 403
    assert console.control.drain() == []


def test_a_text_only_goal_is_refused_with_a_usable_example(server, console) -> None:
    from praxion_brain.console.server import GOAL_EXAMPLES

    status, found = call(
        server, "POST", "/api/control", {"action": "new_goal", "text": "get a furnace"}
    )
    assert status == 400
    for example in GOAL_EXAMPLES:
        assert example in found["error"]
    assert "condition" in found["error"]
    assert console.control.drain() == []


def test_the_dock_asks_for_a_condition_with_valid_examples() -> None:
    from praxion_brain.conditions import canonical_text
    from praxion_brain.console.server import STATIC_DIR

    index = (STATIC_DIR / "index.html").read_text(encoding="utf-8")
    field = re.search(r'<input id="goal-cond"[^\n]*', index)
    assert field is not None
    assert "required" in field.group(0)
    examples = re.findall(r"has\([a-z_]+\)|count\([a-z_]+\) >= \d+", field.group(0))
    assert examples, "the condition field shows no example"
    for example in examples:
        canonical_text(example)  # every example the dock shows parses
    assert "has(furnace)" in examples and "count(cobblestone) >= 16" in examples


def test_a_candidate_source_outside_the_evidence_directory_is_not_read(
    server, console, tmp_path
) -> None:
    outside = tmp_path / "outside.skill"
    outside.write_text(SKILL, encoding="utf-8")
    report = tmp_path / "evidence" / "admission-get_coal-20261108T120000Z.json"
    report.parent.mkdir(exist_ok=True)
    report.write_text(
        json.dumps(
            {
                "skill_name": "get_coal",
                "status": "rejected",
                "attempts": [
                    {"attempt": 0, "candidate_path": str(outside), "practices": []}
                ],
            }
        ),
        encoding="utf-8",
    )
    _, found = call(server, "GET", "/api/skills")
    assert found["candidates"][0]["attempts"][0]["source"] is None


def test_the_handler_has_a_socket_timeout() -> None:
    from praxion_brain.console.server import _Handler

    assert _Handler.timeout is not None
    assert 0 < _Handler.timeout <= 60


# --------------------------------------------------------------------------- #
# Stream
# --------------------------------------------------------------------------- #
def test_stream_emits_state_then_new_evidence_lines(server, console) -> None:
    console.evidence.write("loop_started", world_id="w")
    connection = http.client.HTTPConnection("127.0.0.1", server.port, timeout=5)
    connection.request("GET", "/api/stream")
    response = connection.getresponse()
    assert response.getheader("Content-Type") == "text/event-stream"

    def next_event():
        name = None
        while True:
            line = response.readline().decode().strip()
            if line.startswith("event:"):
                name = line.split(":", 1)[1].strip()
            elif line.startswith("data:"):
                return name, json.loads(line.split(":", 1)[1])

    assert next_event()[0] == "state"
    console.evidence.write("command_sent", skill="get_wood")
    name, payload = next_event()
    assert (name, payload["kind"], payload["skill"]) == ("evidence", "command_sent", "get_wood")
    assert payload["n"] == 2  # the line that existed before connecting is not replayed
    connection.close()


def test_stream_resumes_from_last_event_id(server, console) -> None:
    for index in range(3):
        console.evidence.write("command_sent", i=index)  # lines 1..3
    connection = http.client.HTTPConnection("127.0.0.1", server.port, timeout=5)
    connection.request("GET", "/api/stream", headers={"Last-Event-ID": "2"})
    response = connection.getresponse()
    assert response.status == 200

    def next_event():
        name = None
        while True:
            line = response.readline().decode().strip()
            if line.startswith("event:"):
                name = line.split(":", 1)[1].strip()
            elif line.startswith("data:"):
                return name, json.loads(line.split(":", 1)[1])

    name, payload = next_event()
    assert (name, payload["i"], payload["n"]) == ("evidence", 2, 3)
    connection.close()


# --------------------------------------------------------------------------- #
# The loop applies control
# --------------------------------------------------------------------------- #
def console_loop(body, tmp_path, **kwargs):
    console = build_console(tmp_path)
    loop = make_loop(
        body,
        tmp_path,
        propose_fn=kwargs.pop("propose_fn", lambda *a, **k: proposal("has(coal)", "get coal", "coal")),
        route_fn=kwargs.pop(
            "route_fn", lambda goal, *a, **k: Route(kind="play", skill=f"get_{goal.item}")
        ),
        skills=catalog("get_furnace", "get_coal"),
        console=console,
        **kwargs,
    )
    return console, loop


def test_user_goal_goes_before_the_proposer_and_is_recorded_as_the_users(tmp_path) -> None:
    proposed = []

    def propose_fn(*args, **kwargs):
        proposed.append(1)
        return proposal("has(coal)", "get coal", "coal")

    with FakeBody(skills={"get_furnace", "get_coal"}, facts={"inventory": {}}) as body:
        console, loop = console_loop(body, tmp_path, propose_fn=propose_fn)
        try:
            console.control.put({"action": "new_goal", "text": "get a furnace", "condition": "has(furnace)"})
            loop.start()
            pump(loop, lambda: len(body.commands) >= 1, what="first command")
            assert body.commands[0].skill == "get_furnace"
            assert proposed == []
            record = loop.memory.goals()[0]
            assert (record.origin, record.text) == ("user", "get a furnace")
            applied = evidence_entries(loop, "console_control")
            assert [(e["action"], e["result"]) for e in applied] == [("new_goal", "queued")]
            assert evidence_entries(loop, "user_goal_result")[0]["validation"]["ok"] is True
        finally:
            loop.close()


def test_invalid_user_goal_is_dropped_and_the_proposer_runs(tmp_path) -> None:
    with FakeBody(skills={"get_furnace", "get_coal"}, facts={"inventory": {}}) as body:
        console, loop = console_loop(body, tmp_path)
        try:
            console.control.put({"action": "new_goal", "text": "nonsense", "condition": "nonsense(("})
            loop.start()
            pump(loop, lambda: len(body.commands) >= 1, what="first command")
            assert body.commands[0].skill == "get_coal"
            result = evidence_entries(loop, "user_goal_result")[0]["validation"]
            assert result["ok"] is False
        finally:
            loop.close()


def test_pause_blocks_new_picks_until_resume(tmp_path) -> None:
    with FakeBody(skills={"get_furnace", "get_coal"}, facts={"inventory": {}}) as body:
        console, loop = console_loop(body, tmp_path)
        try:
            console.control.put({"action": "pause"})
            loop.start()
            for _ in range(40):
                loop.step(0.02)
            assert body.commands == []
            assert loop.state.value == "idle"
            assert console.state["paused"] is True

            console.control.put({"action": "resume"})
            pump(loop, lambda: len(body.commands) >= 1, what="command after resume")
            pump(loop, lambda: console.state["paused"] is False, what="published resume")
            assert [e["action"] for e in evidence_entries(loop, "console_control")] == ["pause", "resume"]
        finally:
            loop.close()


def test_pause_lets_the_running_goal_finish_but_picks_nothing_after(tmp_path) -> None:
    with FakeBody(skills={"get_furnace", "get_coal"}, facts={"inventory": {}}) as body:
        console, loop = console_loop(body, tmp_path)
        try:
            loop.start()
            pump(loop, lambda: len(body.commands) >= 1, what="first command")
            first = body.commands[0]
            wait_accepted(body, first.id)
            console.control.put({"action": "pause"})
            body.settle(first.id, outcome="done", condition="true")
            pump(loop, lambda: loop.state.value == "idle" and loop._open_goal is None, what="goal settled")
            for _ in range(30):
                loop.step(0.02)
            assert len(body.commands) == 1
            assert loop.memory.goals()[0].status == "done"
        finally:
            loop.close()


def test_stop_goal_records_the_interruption_only_after_the_cancelled_receipt(
    tmp_path,
) -> None:
    with FakeBody(skills={"get_furnace", "get_coal"}, facts={"inventory": {}}) as body:
        console, loop = console_loop(body, tmp_path)
        try:
            loop.start()
            pump(loop, lambda: len(body.commands) >= 1, what="first command")
            first = body.commands[0]
            wait_accepted(body, first.id)
            pump(loop, lambda: loop._open_goal is not None and loop._open_goal.intent_id is not None, what="intent id")
            intent_id = loop._open_goal.intent_id

            console.control.put({"action": "stop_goal"})
            pump(loop, lambda: body.pops == [intent_id], what="pop")
            pump(loop, lambda: bool(evidence_entries(loop, "console_control")), what="stop evidence")

            # The pop is a request: until the body confirms the cancel, the goal
            # stays open and no interruption is on record.
            assert loop.memory.goals()[0].status == "open"
            assert loop._open_goal is not None
            assert evidence_entries(loop, "goal_finished") == []
            entry = evidence_entries(loop, "console_control")[0]
            assert (entry["action"], entry["result"]) == ("stop_goal", "pop_sent")

            # The body's `settled outcome=cancelled` is the confirmation.
            body.settle(first.id, outcome="cancelled")
            pump(loop, lambda: loop.memory.goals()[0].status == "interrupted", what="interrupted record")
            record = loop.memory.goals()[0]
            assert record.failure_kind == "stopped_by_user"
            assert loop._open_goal is None
            assert not loop.memory.is_blocked("has(coal)")
            finished = evidence_entries(loop, "goal_finished")[-1]
            assert (finished["status"], finished["failure_kind"]) == (
                "interrupted",
                "stopped_by_user",
            )
            assert finished["source"] == "console"
        finally:
            loop.close()


def test_stop_goal_without_a_goal_is_reported_not_raised(tmp_path) -> None:
    with FakeBody(skills={"get_coal"}, facts={"inventory": {}}) as body:
        console, loop = console_loop(body, tmp_path)
        try:
            console.control.put({"action": "pause"})
            loop.start()
            console.control.put({"action": "stop_goal"})
            for _ in range(10):
                loop.step(0.02)
            assert evidence_entries(loop, "console_control")[-1]["result"] == "no_goal"
            assert body.pops == []
        finally:
            loop.close()


def test_stop_goal_while_picking_cancels_the_pick(tmp_path) -> None:
    with FakeBody(skills={"get_furnace", "get_coal"}, facts={"inventory": {}}) as body:
        console, loop = console_loop(body, tmp_path)
        try:
            loop.start()
            pump(loop, lambda: loop.state is LoopState.PICKING, what="picking")
            console.control.put({"action": "stop_goal"})
            for _ in range(10):
                loop.step(0.02)
            assert evidence_entries(loop, "console_control")[-1]["result"] == "pick_cancelled"
            assert loop.state is LoopState.IDLE
            assert body.commands == []
        finally:
            loop.close()


def test_stop_goal_while_a_skill_is_being_written_reports_the_job(tmp_path) -> None:
    started, release = threading.Event(), threading.Event()

    with FakeBody(skills={"get_furnace", "get_coal"}, facts={"inventory": {}}) as body:
        console, loop = console_loop(
            body,
            tmp_path,
            propose_fn=lambda *a, **k: proposal("has(furnace)", "get a furnace", "furnace"),
            route_fn=lambda goal, *a, **k: Route(kind="write"),
            admission_options=admission_options(
                tmp_path,
                write_fn=furnace_write_fn,
                check_fn=furnace_check_fn,
                practice_fn=gated_practice(started, release),
            ),
            catalog_loader=lambda: catalog("get_furnace", "get_coal"),
        )
        try:
            loop.start()
            pump(loop, lambda: loop._job is not None, what="admission job")
            assert started.wait(4.0), "the admission job never started"
            job = loop._job
            console.control.put({"action": "stop_goal"})
            for _ in range(10):
                loop.step(0.02)
            entry = evidence_entries(loop, "console_control")[-1]
            assert (entry["result"], entry["writing"]) == ("job_running", "get_furnace")
            assert entry["stop_requested"] is True
            assert job.stop_requested is True
            assert loop._pending is None
            assert body.commands == [], "a stop during writing still sent a command"

            # The job ends on its own and is reported, but the stopped goal is
            # never played, even though the skill is admitted and stays active.
            release.set()
            pump(
                loop,
                lambda: any(
                    record.failure_kind == "stopped_by_user"
                    for record in loop.memory.goals()
                ),
                what="stopped writing goal",
            )
            record = loop.memory.goals()[0]
            assert (record.status, record.failure_kind) == (
                "interrupted",
                "stopped_by_user",
            )
            stopped = evidence_entries(loop, "write_stopped")[0]
            assert stopped["admission_status"] == "admitted"
            assert (tmp_path / "active" / "get_furnace.skill").is_file()
            assert loop._written is None
            assert body.commands == []
        finally:
            release.set()
            loop.close()


def test_close_publishes_a_final_state_so_the_console_clears_learning(
    tmp_path,
) -> None:
    started, release = threading.Event(), threading.Event()

    with FakeBody(skills={"get_furnace", "get_coal"}, facts={"inventory": {}}) as body:
        console, loop = console_loop(
            body,
            tmp_path,
            propose_fn=lambda *a, **k: proposal("has(furnace)", "get a furnace", "furnace"),
            route_fn=lambda goal, *a, **k: Route(kind="write"),
            admission_options=admission_options(
                tmp_path,
                write_fn=furnace_write_fn,
                check_fn=furnace_check_fn,
                practice_fn=gated_practice(started, release),
            ),
        )
        try:
            loop.start()
            pump(loop, lambda: loop._job is not None, what="admission job")
            assert started.wait(4.0), "the admission job never started"
            pump(
                loop,
                lambda: (console.state.get("learning") or {}).get("skill")
                == "get_furnace",
                what="published learning badge",
            )

            loop.close()

            assert console.state["learning"] is None
        finally:
            release.set()
            loop.close()


def test_a_skill_being_written_is_in_the_published_state(tmp_path) -> None:
    started, release = threading.Event(), threading.Event()

    with FakeBody(skills={"get_furnace", "get_coal"}, facts={"inventory": {}}) as body:
        console, loop = console_loop(
            body,
            tmp_path,
            propose_fn=lambda *a, **k: proposal("has(furnace)", "get a furnace", "furnace"),
            route_fn=lambda goal, *a, **k: Route(kind="write"),
            admission_options=admission_options(
                tmp_path,
                write_fn=furnace_write_fn,
                check_fn=furnace_check_fn,
                practice_fn=gated_practice(started, release),
            ),
        )
        try:
            loop.start()
            pump(loop, lambda: loop._job is not None, what="admission job")
            assert started.wait(4.0), "the admission job never started"
            pump(
                loop,
                lambda: (console.state.get("learning") or {}).get("seed") == 101,
                what="published learning seed",
            )
            learning = console.state["learning"]
            assert learning["skill"] == "get_furnace"
            assert learning["attempt"] == 0
            assert learning["started_at"] > 0
            assert learning["goal_id"]
            json.dumps(learning)  # the server must be able to serialise it

            release.set()
            pump(loop, lambda: console.state.get("learning") is None, what="cleared learning")
        finally:
            release.set()
            loop.close()


def test_the_same_user_goal_is_validated_against_the_last_one(tmp_path) -> None:
    with FakeBody(skills={"get_furnace", "get_coal"}, facts={"inventory": {}}) as body:
        console, loop = console_loop(body, tmp_path)
        try:
            goal = {"action": "new_goal", "text": "get a furnace", "condition": "has(furnace)"}
            console.control.put(goal)
            console.control.put(goal)
            loop.start()
            pump(loop, lambda: len(body.commands) >= 1, what="first command")
            first = body.commands[0]
            assert first.skill == "get_furnace"
            wait_accepted(body, first.id)
            body.settle(first.id, outcome="done", condition="true")
            pump(loop, lambda: len(body.commands) >= 2, what="next command")
            assert [command.skill for command in body.commands] == ["get_furnace", "get_coal"]
            results = [e["validation"] for e in evidence_entries(loop, "user_goal_result")]
            assert [(r["ok"], r["reason"]) for r in results] == [
                (True, None),
                (False, "same_as_last"),
            ]
        finally:
            loop.close()


def test_a_stop_cancels_a_goal_still_waiting_in_the_queue(tmp_path) -> None:
    with FakeBody(skills={"get_furnace", "get_coal"}, facts={"inventory": {}}) as body:
        console, loop = console_loop(body, tmp_path)
        try:
            console.control.put({"action": "pause"})
            console.control.put({"action": "new_goal", "text": "get coal", "condition": "has(coal)"})
            console.control.put({"action": "stop_goal"})
            loop.start()
            for _ in range(10):
                loop.step(0.02)
            results = [e["result"] for e in evidence_entries(loop, "console_control")]
            assert results == ["ok", "queued", "pick_cancelled"]
            assert loop._user_goals == []
            assert body.commands == []
        finally:
            loop.close()


def test_a_goal_without_a_condition_is_not_queued(tmp_path) -> None:
    with FakeBody(skills={"get_coal"}, facts={"inventory": {}}) as body:
        console, loop = console_loop(body, tmp_path)
        try:
            console.control.put({"action": "pause"})
            console.control.put({"action": "new_goal", "text": "get a furnace"})
            loop.start()
            for _ in range(10):
                loop.step(0.02)
            assert evidence_entries(loop, "console_control")[-1]["result"] == "no_condition"
            assert loop._user_goals == []
            assert body.commands == []
        finally:
            loop.close()


def test_loop_publishes_a_state_the_console_can_read(tmp_path) -> None:
    with FakeBody(skills={"get_furnace", "get_coal"}, facts={"inventory": {}}) as body:
        console, loop = console_loop(body, tmp_path)
        try:
            loop.start()
            pump(loop, lambda: len(body.commands) >= 1, what="first command")
            pump(loop, lambda: console.state.get("goal") is not None, what="published goal")
            state = console.state
            assert state["state"] == "playing"
            assert state["goal"]["condition"] == "has(coal)"
            assert state["route"]["skill"] == "get_coal"
            assert state["ladder"]["total"] == 12
            assert state["facts"]["inventory"] == {}
            json.dumps(state)  # the server must be able to serialise it
        finally:
            loop.close()


def test_the_task_action_takes_free_text_without_a_condition(server, console) -> None:
    status, body = call(server, "POST", "/api/control", {"action": "task", "text": "8 taş\ngetir"})
    assert status == 202 and body["queued"] == "task"
    assert console.control.drain() == [{"action": "task", "text": "8 taş getir"}]
    for bad in ({"action": "task"}, {"action": "task", "text": "   "}, {"action": "task", "text": "x" * 201}):
        status, _ = call(server, "POST", "/api/control", bad)
        assert status == 400

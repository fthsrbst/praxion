"""The console's HTTP server: JSON endpoints, one SSE stream and the static UI.

Standard library only, bound to 127.0.0.1. A request never reaches into the
loop: reads come from :mod:`.data`, and the one write, ``POST /api/control``,
only puts a request on the :class:`~.hub.ControlQueue` for the loop to apply.
The Host and Origin headers must name this server, so a web page elsewhere
cannot drive or read it (DNS rebinding, cross-site POST); every response also
forbids framing, so no page can clickjack its buttons.
"""

from __future__ import annotations

import json
import threading
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any, Optional
from urllib.parse import parse_qs, unquote, urlparse

from ..conditions import ConditionError, canonical_text
from . import data
from .hub import Console
from .log import EvidenceLog

STATIC_DIR = Path(__file__).resolve().parent / "static"
DEFAULT_PORT = 8765
ACTIONS = ("pause", "resume", "stop_goal", "new_goal", "task")
MAX_BODY_BYTES = 4096
MAX_TEXT_CHARS = 200
EVIDENCE_PAGE = 200
EVIDENCE_PAGE_MAX = 1000
STREAM_POLL_SECONDS = 0.25
STATE_EVERY_SECONDS = 1.0
#: A half-open keep-alive connection must not hold a handler thread forever.
SOCKET_TIMEOUT_SECONDS = 30
#: Every response: no frame may embed this page (clickjacking), and the page
#: may only load its own files plus the two Google Fonts hosts the UI uses.
FRAME_HEADERS = (
    ("X-Frame-Options", "DENY"),
    (
        "Content-Security-Policy",
        "default-src 'self'; script-src 'self'; "
        "style-src 'self' 'unsafe-inline' https://fonts.googleapis.com; "
        "font-src 'self' https://fonts.gstatic.com; "
        "img-src 'self' data:; connect-src 'self'; "
        "object-src 'none'; base-uri 'none'; form-action 'self'; "
        "frame-ancestors 'none'",
    ),
)
#: Example conditions the docs and the dock show; text alone is not verifiable.
GOAL_EXAMPLES = ("has(furnace)", "count(cobblestone) >= 16")
CONTENT_TYPES = {
    ".html": "text/html; charset=utf-8",
    ".css": "text/css; charset=utf-8",
    ".js": "text/javascript; charset=utf-8",
    ".svg": "image/svg+xml",
    ".json": "application/json",
}


def check_control(body: Any) -> dict[str, Any]:
    """The cleaned control request, or ``ValueError`` naming what is wrong."""
    if not isinstance(body, dict):
        raise ValueError("the body must be a JSON object")
    action = body.get("action")
    if action not in ACTIONS:
        raise ValueError(f"action must be one of {', '.join(ACTIONS)}")
    request: dict[str, Any] = {"action": action}
    if action == "task":
        # Free text, mapped to a goal by the loop's command path (the same one
        # chat uses); no condition needed. Newlines fold to spaces.
        text = " ".join(str(body.get("text") or "").split())
        if not text:
            raise ValueError("a task needs text, e.g. \"8 taş getir\" or \"get cobblestone 8\"")
        if len(text) > MAX_TEXT_CHARS:
            raise ValueError(f"text is limited to {MAX_TEXT_CHARS} characters")
        request["text"] = text
    elif action == "new_goal":
        text = str(body.get("text") or "").strip()
        condition = str(body.get("condition") or "").strip()
        if not condition:
            raise ValueError(
                "a goal needs a condition, e.g. "
                + " or ".join(GOAL_EXAMPLES)
                + "; text alone cannot be verified"
            )
        if len(text) > MAX_TEXT_CHARS or len(condition) > MAX_TEXT_CHARS:
            raise ValueError(f"text and condition are limited to {MAX_TEXT_CHARS} characters")
        try:
            canonical_text(condition)
        except ConditionError as error:
            raise ValueError(f"the condition does not parse: {error}") from error
        request["text"], request["condition"] = text, condition
    elif action == "task":
        text = str(body.get("text") or "").strip()
        if not text:
            raise ValueError("a task needs some text")
        if len(text) > MAX_TEXT_CHARS:
            raise ValueError(f"text is limited to {MAX_TEXT_CHARS} characters")
        request["text"] = text
    return request


class ConsoleServer:
    """Owns the HTTP server and its daemon thread."""

    def __init__(self, console: Console, port: int = DEFAULT_PORT, static_dir: Path = STATIC_DIR) -> None:
        self.console = console
        self.log = EvidenceLog(console.evidence.path)
        self.static_dir = Path(static_dir)
        self.stopping = threading.Event()
        self._httpd = ThreadingHTTPServer(("127.0.0.1", port), _Handler)
        self._httpd.daemon_threads = True
        self._httpd.app = self  # type: ignore[attr-defined]
        self._thread: Optional[threading.Thread] = None

    @property
    def port(self) -> int:
        return int(self._httpd.server_address[1])

    @property
    def url(self) -> str:
        return f"http://127.0.0.1:{self.port}/"

    def start(self) -> "ConsoleServer":
        self._thread = threading.Thread(
            target=self._httpd.serve_forever, name="console-http", daemon=True
        )
        self._thread.start()
        return self

    def shutdown(self) -> None:
        self.stopping.set()
        self._httpd.shutdown()
        self._httpd.server_close()
        self.log.close()


class _Handler(BaseHTTPRequestHandler):
    server_version = "PraxionConsole"
    protocol_version = "HTTP/1.1"
    timeout = SOCKET_TIMEOUT_SECONDS

    @property
    def app(self) -> ConsoleServer:
        return self.server.app  # type: ignore[attr-defined]

    def log_message(self, format: str, *args: Any) -> None:  # noqa: A002 - stdlib name
        pass

    # -- plumbing ------------------------------------------------------- #
    def _headers(self, content_type: str, length: Optional[int]) -> None:
        """The headers every response carries: type, length and framing bans."""
        self.send_header("Content-Type", content_type)
        if length is not None:
            self.send_header("Content-Length", str(length))
        self.send_header("Cache-Control", "no-store")
        for name, value in FRAME_HEADERS:
            self.send_header(name, value)

    def _send(self, status: int, body: bytes, content_type: str) -> None:
        self.send_response(status)
        self._headers(content_type, len(body))
        self.end_headers()
        self.wfile.write(body)

    def _json(self, value: Any, status: int = 200) -> None:
        body = json.dumps(value, ensure_ascii=False, default=str).encode("utf-8")
        self._send(status, body, "application/json; charset=utf-8")

    def _error(self, status: int, message: str) -> None:
        self.close_connection = True  # an unread request body must not poison the next one
        self._json({"error": message}, status)

    def _host_ok(self) -> bool:
        allowed = {f"127.0.0.1:{self.app.port}", f"localhost:{self.app.port}"}
        if self.headers.get("Host") not in allowed:
            return False
        origin = self.headers.get("Origin")
        return origin is None or origin in {f"http://{host}" for host in allowed}

    # -- routes --------------------------------------------------------- #
    def do_GET(self) -> None:  # noqa: N802 - stdlib name
        if not self._host_ok():
            self._error(HTTPStatus.FORBIDDEN, "bad host or origin")
            return
        url = urlparse(self.path)
        query = parse_qs(url.query)
        path = unquote(url.path)
        try:
            if path.startswith("/api/"):
                self._api(path[len("/api/"):], query)
            else:
                self._static(path)
        except (BrokenPipeError, ConnectionResetError):
            pass
        except Exception as error:  # noqa: BLE001 - a bad request must not kill the thread
            # The type only: a message can carry absolute local paths.
            self._error(
                HTTPStatus.INTERNAL_SERVER_ERROR,
                f"internal error ({type(error).__name__})",
            )

    def do_POST(self) -> None:  # noqa: N802 - stdlib name
        if not self._host_ok():
            self._error(HTTPStatus.FORBIDDEN, "bad host or origin")
            return
        if urlparse(self.path).path != "/api/control":
            self._error(HTTPStatus.NOT_FOUND, "not found")
            return
        try:
            length = int(self.headers.get("Content-Length") or 0)
            if length > MAX_BODY_BYTES:
                self._error(HTTPStatus.REQUEST_ENTITY_TOO_LARGE, "body too large")
                return
            request = check_control(json.loads(self.rfile.read(length) or b"null"))
        except ValueError as error:
            self._error(HTTPStatus.BAD_REQUEST, str(error))
            return
        if request["action"] == "task" and not self.app.console.accepts_tasks:
            # Never accept a request that would be silently dropped; the talk
            # design owns free text -> verifiable goal (decision 2026-11-09).
            self._error(
                HTTPStatus.SERVICE_UNAVAILABLE,
                "free-text tasks need a running loop; give a goal with a condition",
            )
            return
        self.app.console.control.put(request)
        self._json({"ok": True, "queued": request["action"]}, HTTPStatus.ACCEPTED)

    def _api(self, name: str, query: dict[str, list[str]]) -> None:
        console, log = self.app.console, self.app.log
        if name == "state":
            self._json(data.state(console))
        elif name == "evidence":
            self._json(self._evidence(query))
        elif name.startswith("exchange/"):
            try:
                number = int(name[len("exchange/"):])
            except ValueError:
                self._error(HTTPStatus.BAD_REQUEST, "the line must be a number")
                return
            found = data.exchange(log, number)
            if found is None:
                self._error(HTTPStatus.NOT_FOUND, "no exchange at that line")
            else:
                self._json(found)
        elif name == "goals":
            self._json(data.goals(console))
        elif name == "skills":
            self._json(data.skills(console))
        elif name == "metrics":
            self._json(data.metrics(console, log))
        elif name == "stream":
            self._stream(query)
        else:
            self._error(HTTPStatus.NOT_FOUND, "not found")

    def _evidence(self, query: dict[str, list[str]]) -> dict[str, Any]:
        log = self.app.log
        total = log.refresh()
        since = _int(query, "since")
        limit = min(max(1, _int(query, "limit") or EVIDENCE_PAGE), EVIDENCE_PAGE_MAX)
        kinds = _kinds(query)
        lines = [
            data.slim(entry)
            for entry in log.tail(since=since, kinds=kinds, limit=limit)
        ]
        return {"lines": lines, "last": total}

    def _stream(self, query: dict[str, list[str]]) -> None:
        """Server-Sent Events: each new evidence line, and the state every second.

        A reconnect resumes from the ``Last-Event-ID`` header the browser sends,
        which is ahead of the ``?since`` of the original request.
        """
        log = self.app.log
        since = _int_value(self.headers.get("Last-Event-ID"))
        if since is None:
            since = _int(query, "since")
        if since is None:
            since = log.refresh()
        kinds = _kinds(query)
        self.send_response(HTTPStatus.OK)
        self._headers("text/event-stream", None)
        self.send_header("Connection", "close")
        self.end_headers()
        self.close_connection = True
        waited = STATE_EVERY_SECONDS
        self.app.console.watch()
        try:
            while not self.app.stopping.is_set():
                for entry in log.tail(
                    since=since, kinds=kinds, limit=EVIDENCE_PAGE_MAX
                ):
                    since = entry["n"]
                    self._event("evidence", data.slim(entry), entry["n"])
                if waited >= STATE_EVERY_SECONDS:
                    self._event("state", data.state(self.app.console))
                    waited = 0.0
                self.app.stopping.wait(STREAM_POLL_SECONDS)
                waited += STREAM_POLL_SECONDS
        except (BrokenPipeError, ConnectionResetError, OSError):
            return
        finally:
            # The loop asks the body for a fresh snapshot only while someone
            # is watching (decision 2026-11-09 "Console": refresh every second).
            self.app.console.unwatch()

    def _event(self, name: str, payload: Any, number: Optional[int] = None) -> None:
        lines = [f"event: {name}"]
        if number is not None:
            lines.append(f"id: {number}")
        lines.append("data: " + json.dumps(payload, ensure_ascii=False, default=str))
        self.wfile.write(("\n".join(lines) + "\n\n").encode("utf-8"))
        self.wfile.flush()

    def _static(self, path: str) -> None:
        relative = path.lstrip("/") or "index.html"
        root = self.app.static_dir.resolve()
        target = (root / relative).resolve()
        if root not in target.parents or not target.is_file():
            self._error(HTTPStatus.NOT_FOUND, "not found")
            return
        content_type = CONTENT_TYPES.get(target.suffix, "application/octet-stream")
        self._send(HTTPStatus.OK, target.read_bytes(), content_type)


def _int(query: dict[str, list[str]], name: str) -> Optional[int]:
    try:
        return int(query[name][0])
    except (KeyError, IndexError, ValueError):
        return None


def _int_value(value: Optional[str]) -> Optional[int]:
    try:
        return int(value) if value is not None else None
    except ValueError:
        return None


def _kinds(query: dict[str, list[str]]) -> list[str]:
    return [k for k in (query.get("kinds") or [""])[0].split(",") if k]


__all__ = [
    "ACTIONS",
    "ConsoleServer",
    "DEFAULT_PORT",
    "FRAME_HEADERS",
    "GOAL_EXAMPLES",
    "check_control",
]

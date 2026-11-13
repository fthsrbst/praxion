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

from .. import voice
from ..conditions import ConditionError, canonical_text
from ..ledger import Ledger
from . import data
from .hub import Console
from .icons import ItemIcons
from .log import EvidenceLog
from .run import JoinError, SessionManager

STATIC_DIR = Path(__file__).resolve().parent / "static"
DEFAULT_PORT = 8765
ACTIONS = ("pause", "resume", "stop_goal", "new_goal", "task")
#: The only lifecycle mutations the page may ask for. Each one is a fixed
#: server-side action; no browser value reaches a process, a path or a port.
SESSION_ACTIONS = ("start_server", "start_bot", "stop_bot", "stop_server", "join")
MAX_BODY_BYTES = 4096
#: A push-to-talk clip is far bigger than any JSON body; cap it at 2 MB.
MAX_AUDIO_BYTES = 2 * 1024 * 1024
#: Raw TTS text before cleaning; the spoken line itself is capped at 300.
MAX_VOICE_TEXT_CHARS = 2000
MAX_TEXT_CHARS = 200
EVIDENCE_PAGE = 200
EVIDENCE_PAGE_MAX = 1000
STREAM_POLL_SECONDS = 0.25
STATE_EVERY_SECONDS = 1.0
#: A half-open keep-alive connection must not hold a handler thread forever.
SOCKET_TIMEOUT_SECONDS = 30
#: Every response: no frame may embed this page (clickjacking), and the page
#: may only load its own files plus the two Google Fonts hosts the UI uses.
#: ``media-src`` admits the blob URL the voice panel plays its TTS clip from.
FRAME_HEADERS = (
    ("X-Frame-Options", "DENY"),
    (
        "Content-Security-Policy",
        "default-src 'self'; script-src 'self'; "
        "style-src 'self' 'unsafe-inline' https://fonts.googleapis.com; "
        "font-src 'self' https://fonts.gstatic.com; "
        "img-src 'self' data:; media-src 'self' blob:; connect-src 'self'; "
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
    """Owns the HTTP server and its daemon thread.

    A managed console has no :class:`~.hub.Console` until a session is live:
    the manager installs one per run and removes it again on teardown. Reading
    ``console`` always takes the current one, so an already-open SSE stream
    follows the swap and releases the watcher it actually counted.
    """

    def __init__(
        self,
        console: Optional[Console] = None,
        port: int = DEFAULT_PORT,
        static_dir: Path = STATIC_DIR,
        *,
        manager: Optional[SessionManager] = None,
        ledger: Optional[Ledger] = None,
        icons: Optional[ItemIcons] = None,
    ) -> None:
        if console is None and manager is None:
            raise ValueError("the console server needs a Console or a SessionManager")
        self.manager = manager
        self.ledger = ledger or (
            console.ledger if console is not None else manager.ledger  # type: ignore[union-attr]
        )
        evidence = (
            console.evidence if console is not None else manager.evidence  # type: ignore[union-attr]
        )
        self.log = EvidenceLog(evidence.path)
        # Voice calls record their characters/seconds in this run's evidence.
        voice.configure(evidence=evidence)
        # One resolver per server: it owns one jar handle and its own caches.
        self.icons = icons or ItemIcons()
        self.static_dir = Path(static_dir)
        self.stopping = threading.Event()
        self._console = console
        self._console_lock = threading.Lock()
        self._httpd = ThreadingHTTPServer(("127.0.0.1", port), _Handler)
        self._httpd.daemon_threads = True
        self._httpd.app = self  # type: ignore[attr-defined]
        self._thread: Optional[threading.Thread] = None

    @property
    def console(self) -> Optional[Console]:
        """The console of the live session, or ``None`` while managed and idle."""
        with self._console_lock:
            return self._console

    def set_console(self, console: Optional[Console]) -> None:
        """Install one session's console (or clear it); a whole-object swap."""
        with self._console_lock:
            self._console = console

    def state(self) -> dict[str, Any]:
        """The state endpoint's value; the legacy call shape stays patchable."""
        console = self.console
        if self.manager is None:
            # Existing brain-only consoles and their tests call data.state with
            # the console alone; keep that exact call.
            return data.state(console)
        return data.state(
            console,
            session=self.manager.status(),
            ledger=self.ledger,
        )

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
        self.icons.close()
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
        # Keep blank values: `/api/voice/lines?after=` is a real request ("this
        # page is new, say what comes next"), not a missing parameter.
        query = parse_qs(url.query, keep_blank_values=True)
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
        path = urlparse(self.path).path
        if path == "/api/control":
            self._post_control()
        elif path == "/api/session":
            self._post_session()
        elif path == "/api/voice/tts":
            self._post_voice_tts()
        elif path == "/api/voice/stt":
            self._post_voice_stt()
        else:
            self._error(HTTPStatus.NOT_FOUND, "not found")

    def _body_length(self) -> Optional[int]:
        """The request's declared size, or ``None`` when it is unusable."""
        try:
            return int(self.headers.get("Content-Length") or 0)
        except ValueError:
            return None

    def _post_voice_tts(self) -> None:
        """``{"text": str}`` -> mp3; the key stays on this side."""
        if not voice.enabled():
            self._error(
                HTTPStatus.SERVICE_UNAVAILABLE,
                f"voice needs {voice.API_KEY_ENV} in .env",
            )
            return
        length = self._body_length()
        if length is None:
            self._error(HTTPStatus.BAD_REQUEST, "bad content length")
            return
        if length <= 0:
            self._error(HTTPStatus.BAD_REQUEST, "the body must be a JSON object")
            return
        if length > MAX_BODY_BYTES:
            self._error(HTTPStatus.REQUEST_ENTITY_TOO_LARGE, "body too large")
            return
        try:
            body = json.loads(self.rfile.read(length))
        except ValueError:
            self._error(HTTPStatus.BAD_REQUEST, "the body must be valid JSON")
            return
        if not isinstance(body, dict):
            self._error(HTTPStatus.BAD_REQUEST, "the body must be a JSON object")
            return
        text = str(body.get("text") or "")
        if not text.strip():
            self._error(HTTPStatus.BAD_REQUEST, "a line to speak is required")
            return
        if len(text) > MAX_VOICE_TEXT_CHARS:
            self._error(
                HTTPStatus.BAD_REQUEST,
                f"text is limited to {MAX_VOICE_TEXT_CHARS} characters",
            )
            return
        try:
            audio = voice.synthesize(text)
        except voice.VoiceError as error:
            self._error(HTTPStatus.BAD_GATEWAY, str(error))
            return
        self._send(HTTPStatus.OK, audio, "audio/mpeg")

    def _post_voice_stt(self) -> None:
        """The recording itself (``Content-Type`` of the clip) -> ``{"text"}``."""
        if not voice.enabled():
            self._error(
                HTTPStatus.SERVICE_UNAVAILABLE,
                f"voice needs {voice.API_KEY_ENV} in .env",
            )
            return
        content_type = (self.headers.get_content_type() or "").lower()
        if not content_type.startswith("audio/"):
            self._error(
                HTTPStatus.UNSUPPORTED_MEDIA_TYPE,
                "the body must be the recorded audio, with its own Content-Type",
            )
            return
        length = self._body_length()
        if length is None:
            self._error(HTTPStatus.BAD_REQUEST, "bad content length")
            return
        if length <= 0:
            self._error(HTTPStatus.BAD_REQUEST, "the body must be the recorded audio")
            return
        if length > MAX_AUDIO_BYTES:
            self._error(HTTPStatus.REQUEST_ENTITY_TOO_LARGE, "recording too large")
            return
        audio = self.rfile.read(length)
        try:
            text = voice.transcribe(audio, content_type, "tr")
        except voice.VoiceError as error:
            self._error(HTTPStatus.BAD_GATEWAY, str(error))
            return
        self._json({"text": text})

    def _post_control(self) -> None:
        try:
            length = int(self.headers.get("Content-Length") or 0)
            if length > MAX_BODY_BYTES:
                self._error(HTTPStatus.REQUEST_ENTITY_TOO_LARGE, "body too large")
                return
            request = check_control(json.loads(self.rfile.read(length) or b"null"))
        except ValueError as error:
            self._error(HTTPStatus.BAD_REQUEST, str(error))
            return
        console = self.app.console
        if self.app.manager is not None:
            # Goal controls exist only while the managed session is live. The
            # UI disables them in idle/start/stop; this is the server's own
            # refusal, so a stale page cannot queue work for no loop.
            if console is None or not self.app.manager.bot_live():
                self._error(
                    HTTPStatus.CONFLICT,
                    "no live bot: start the server, start the bot and wait until it is running",
                )
                return
        if console is None:
            self._error(HTTPStatus.SERVICE_UNAVAILABLE, "no console is available")
            return
        if request["action"] == "task" and not console.accepts_tasks:
            # Never accept a request that would be silently dropped; the talk
            # design owns free text -> verifiable goal (decision 2026-11-09).
            self._error(
                HTTPStatus.SERVICE_UNAVAILABLE,
                "free-text tasks need a running loop; give a goal with a condition",
            )
            return
        console.control.put(request)
        self._json({"ok": True, "queued": request["action"]}, HTTPStatus.ACCEPTED)

    def _post_session(self) -> None:
        """The only lifecycle mutations: fixed start/stop/join, nothing else."""
        manager = self.app.manager
        if manager is None:
            self._error(HTTPStatus.NOT_FOUND, "not found")
            return
        if (self.headers.get_content_type() or "").lower() != "application/json":
            self._error(
                HTTPStatus.UNSUPPORTED_MEDIA_TYPE,
                "the lifecycle endpoint takes application/json",
            )
            return
        try:
            length = int(self.headers.get("Content-Length") or 0)
        except ValueError:
            self._error(HTTPStatus.BAD_REQUEST, "bad content length")
            return
        if length <= 0:
            self._error(HTTPStatus.BAD_REQUEST, "the body must be a JSON object")
            return
        if length > MAX_BODY_BYTES:
            self._error(HTTPStatus.REQUEST_ENTITY_TOO_LARGE, "body too large")
            return
        try:
            body = json.loads(self.rfile.read(length))
        except ValueError:
            self._error(HTTPStatus.BAD_REQUEST, "the body must be valid JSON")
            return
        if not isinstance(body, dict):
            self._error(HTTPStatus.BAD_REQUEST, "the body must be a JSON object")
            return
        extra = set(body) - {"action"}
        if extra:
            # A port, path, executable, operator or flag from the browser is
            # never accepted: names are fixed by the server's own configuration.
            self._error(
                HTTPStatus.BAD_REQUEST,
                "only the action field is allowed; the server owns the rest",
            )
            return
        action = body.get("action")
        if action not in SESSION_ACTIONS:
            self._error(
                HTTPStatus.BAD_REQUEST,
                f"action must be one of {', '.join(SESSION_ACTIONS)}",
            )
            return
        try:
            if action in ("start_server", "start_bot"):
                started = (
                    manager.start_server()
                    if action == "start_server"
                    else manager.start_bot()
                )
                if not started:
                    if action == "start_server":
                        message = "a server is already starting, running or stopping"
                    else:
                        message = (
                            "the bot cannot start until the server is ready, or it "
                            "is already starting or running"
                        )
                    self._error(HTTPStatus.CONFLICT, message)
                    return
                self._json(
                    {"ok": True, "action": action, "session": manager.status()},
                    HTTPStatus.ACCEPTED,
                )
                return
            if action == "stop_bot":
                manager.request_stop_bot()
                self._json(
                    {"ok": True, "action": action, "session": manager.status()},
                    HTTPStatus.ACCEPTED,
                )
                return
            if action == "stop_server":
                # Works while the bot is starting or dying too: the server
                # worker cancels both and waits for the bot's cleanup.
                manager.request_stop_server()
                self._json(
                    {"ok": True, "action": action, "session": manager.status()},
                    HTTPStatus.ACCEPTED,
                )
                return
            try:
                client = manager.join()
            except JoinError as error:
                self._error(HTTPStatus.CONFLICT, str(error))
                return
            self._json(
                {"ok": True, "action": action, "client": client, "session": manager.status()},
                HTTPStatus.ACCEPTED,
            )
        except Exception as error:  # noqa: BLE001 - a request must not kill the thread
            self._error(
                HTTPStatus.INTERNAL_SERVER_ERROR,
                f"session control failed ({type(error).__name__})",
            )

    def _api(self, name: str, query: dict[str, list[str]]) -> None:
        console, log = self.app.console, self.app.log
        if name == "state":
            self._json(self.app.state())
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
        elif name == "goals/timeline":
            self._json(data.goals_timeline(log, _int(query, "run")))
        elif name == "goals":
            self._json(data.goals(console))
        elif name == "exchanges":
            self._json(
                data.exchanges(
                    log,
                    limit=_int(query, "limit") or 60,
                    kinds=_kinds(query),
                )
            )
        elif name == "wiki/search":
            raw = query.get("q")
            self._json(data.wiki_search(raw[0] if raw else ""))
        elif name == "skills":
            self._json(data.skills(console))
        elif name == "metrics":
            self._json(data.metrics(console, log, ledger=self.app.ledger))
        elif name == "voice/status":
            live = voice.enabled()
            self._json(
                {
                    "enabled": live,
                    "voice_id": voice.voice_id() if live else "",
                }
            )
        elif name == "voice/lines":
            # An absent `after` asks for the newest id only; an empty one says
            # "this page is new, say what comes next" (see data.narration_lines).
            raw = query.get("after")
            after = raw[0] if raw else None
            self._json(data.narration_lines(log, after))
        elif name.startswith("item-icon/"):
            icon = self.app.icons.get(name[len("item-icon/"):])
            if icon is None:
                self._error(HTTPStatus.NOT_FOUND, "no icon for that item")
            else:
                self._json(icon)
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
        watched: Optional[Console] = None
        try:
            while not self.app.stopping.is_set():
                console = self.app.console
                if console is not watched:
                    # A managed run swaps the whole console: follow the new one
                    # and release the watcher on the object this stream actually
                    # counted, so an old session's count cannot leak.
                    if watched is not None:
                        watched.unwatch()
                    watched = console
                    if watched is not None:
                        watched.watch()
                for entry in log.tail(
                    since=since, kinds=kinds, limit=EVIDENCE_PAGE_MAX
                ):
                    since = entry["n"]
                    self._event("evidence", data.slim(entry), entry["n"])
                if waited >= STATE_EVERY_SECONDS:
                    self._event("state", self.app.state())
                    waited = 0.0
                self.app.stopping.wait(STREAM_POLL_SECONDS)
                waited += STREAM_POLL_SECONDS
        except (BrokenPipeError, ConnectionResetError, OSError):
            return
        finally:
            # The loop asks the body for a fresh snapshot only while someone
            # is watching (decision 2026-11-09 "Console": refresh every second).
            if watched is not None:
                watched.unwatch()

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
    "MAX_AUDIO_BYTES",
    "MAX_VOICE_TEXT_CHARS",
    "SESSION_ACTIONS",
    "check_control",
]

"""Minimal Minecraft RCON client (stdlib only).

Packet format (matches ``spikes/round0/rcon.py``):
  length (i32 LE) | request id (i32) | type (i32) | body (utf-8) | 0x00 0x00

Auth is type 3, commands type 2. Large responses are split across several packets; we
detect the end by sending a follow-up empty command with a different request id and
reading until that reply arrives.
"""

from __future__ import annotations

import socket
import struct
import threading
import time
from typing import Optional


class RconError(Exception):
    """Generic RCON failure."""


class RconAuthError(RconError):
    """Authentication rejected by the server."""


class RconClient:
    def __init__(self, host: str = "127.0.0.1", port: int = 25575,
                 password: str = "praxion-local", timeout: float = 10.0):
        self.host = host
        self.port = port
        self.password = password
        self.timeout = timeout
        self._sock: Optional[socket.socket] = None
        self._lock = threading.Lock()
        self._next_id = 0
        self._connected_once = False

    # ------------------------------------------------------------------ #
    def _new_id(self) -> int:
        self._next_id += 1
        return self._next_id

    def connect(self) -> None:
        """Open the socket and authenticate. Raises on failure."""
        self.close()
        sock = socket.create_connection((self.host, self.port), timeout=self.timeout)
        sock.settimeout(self.timeout)
        self._sock = sock
        self._connected_once = True
        req_id = self._new_id()
        self._send_packet(req_id, 3, self.password)
        reply_id, _kind, _body = self._read_packet()
        if reply_id == -1:
            self.close()
            raise RconAuthError("rcon authentication failed (server returned -1)")
        if reply_id != req_id:
            # Some servers echo; accept any non -1 as success for auth.
            pass

    def close(self) -> None:
        sock = self._sock
        self._sock = None
        if sock is not None:
            try:
                sock.close()
            except OSError:
                pass

    def __enter__(self) -> "RconClient":
        self.connect()
        return self

    def __exit__(self, *exc) -> None:
        self.close()

    # ------------------------------------------------------------------ #
    def _send_packet(self, req_id: int, kind: int, body: str) -> None:
        if self._sock is None:
            raise RconError("not connected")
        payload = struct.pack("<ii", req_id, kind) + body.encode("utf-8") + b"\x00\x00"
        packet = struct.pack("<i", len(payload)) + payload
        self._sock.sendall(packet)

    def _read_exact(self, n: int) -> bytes:
        if self._sock is None:
            raise RconError("not connected")
        buf = b""
        while len(buf) < n:
            chunk = self._sock.recv(n - len(buf))
            if not chunk:
                raise ConnectionError("rcon connection closed")
            buf += chunk
        return buf

    def _read_packet(self) -> tuple[int, int, str]:
        (length,) = struct.unpack("<i", self._read_exact(4))
        if length < 8:
            raise RconError(f"invalid rcon packet length: {length}")
        data = self._read_exact(length)
        req_id, kind = struct.unpack("<ii", data[:8])
        body = data[8:-2].decode("utf-8", "replace")
        return req_id, kind, body

    # ------------------------------------------------------------------ #
    def command(self, command: str) -> str:
        """Send a command and return its full (possibly multi-packet) response."""
        for attempt in range(2):
            try:
                with self._lock:
                    if self._sock is None:
                        if attempt == 0:
                            raise RconError("not connected")
                        self.connect()
                    req_id = self._new_id()
                    term_id = self._new_id()
                    self._send_packet(req_id, 2, command)
                    time.sleep(0.05)
                    self._send_packet(term_id, 2, "")
                    time.sleep(0.05)
                    chunks: list[str] = []
                    deadline_guard = 0
                    while True:
                        rid, _kind, body = self._read_packet()
                        if rid == term_id:
                            break
                        chunks.append(body)
                        deadline_guard += 1
                        if deadline_guard > 10000:
                            raise RconError("too many rcon packets")
                    return "".join(chunks)
            except (ConnectionError, RconError, OSError):
                self.close()
                if attempt == 1 or not self._connected_once:
                    raise
                try:
                    self.connect()
                except Exception:
                    raise

    # Backwards-friendly alias used by some callers.
    def run(self, command: str) -> str:
        return self.command(command)


__all__ = ["RconClient", "RconError", "RconAuthError"]
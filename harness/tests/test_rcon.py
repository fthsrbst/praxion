"""Unit tests for the RCON client against a tiny fake server."""

import logging
import socket
import struct
import threading

import pytest

from praxion_harness.rcon import RconClient, RconAuthError, RconError

logger = logging.getLogger(__name__)


def _pack(req_id, kind, body):
    payload = struct.pack("<ii", req_id, kind) + body.encode("utf-8") + b"\x00\x00"
    return struct.pack("<i", len(payload)) + payload


class FakeRconServer:
    def __init__(self, password="secret"):
        self.password = password
        self.errors = []
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.sock.bind(("127.0.0.1", 0))
        self.sock.listen(8)
        self.port = self.sock.getsockname()[1]
        self.thread = threading.Thread(target=self._serve, daemon=True)
        self.thread.start()

    def _serve(self):
        while True:
            try:
                conn, _ = self.sock.accept()
            except OSError:
                return
            threading.Thread(target=self._handle, args=(conn,), daemon=True).start()

    def _handle(self, conn):
        """Serve one client; a reset connection ends this reader quietly."""
        try:
            # Auth.
            data = self._recv_packet(conn)
            if data is None:
                return
            rid, kind, body = data
            if kind == 3:
                if body != self.password:
                    conn.sendall(_pack(-1, 2, ""))
                    return
                conn.sendall(_pack(rid, 2, ""))
            # Commands loop.
            while True:
                data = self._recv_packet(conn)
                if data is None:
                    break
                rid, kind, body = data
                if body == "BIG":
                    # Split response into two packets; the client's empty
                    # follow-up request terminates it.
                    conn.sendall(_pack(rid, 0, "PART1"))
                    conn.sendall(_pack(rid, 0, "PART2"))
                elif body == "":
                    conn.sendall(_pack(rid, 0, ""))
                else:
                    conn.sendall(_pack(rid, 0, f"echo:{body}"))
        except (ConnectionResetError, ConnectionAbortedError, OSError) as exc:
            # A client that closes mid-response is normal at test teardown;
            # record it instead of letting the thread raise.
            self.errors.append(exc)
            logger.debug("fake rcon client connection ended: %s", exc)
        finally:
            try:
                conn.close()
            except OSError:
                pass

    @staticmethod
    def _recv_packet(conn):
        def read_exact(n):
            buf = b""
            while len(buf) < n:
                try:
                    c = conn.recv(n - len(buf))
                except OSError:
                    return None
                if not c:
                    return None
                buf += c
            return buf
        raw = read_exact(4)
        if raw is None or len(raw) < 4:
            return None
        (length,) = struct.unpack("<i", raw)
        data = read_exact(length)
        if data is None:
            return None
        rid, kind = struct.unpack("<ii", data[:8])
        body = data[8:-2].decode("utf-8", "replace")
        return rid, kind, body

    def close(self):
        try:
            self.sock.close()
        except OSError:
            pass


def test_connect_auth_and_command():
    srv = FakeRconServer(password="pw")
    try:
        c = RconClient(host="127.0.0.1", port=srv.port, password="pw", timeout=5)
        c.connect()
        assert c.command("hello") == "echo:hello"
        c.close()
    finally:
        srv.close()


def test_auth_failure_raises():
    srv = FakeRconServer(password="pw")
    try:
        c = RconClient(host="127.0.0.1", port=srv.port, password="wrong", timeout=5)
        with pytest.raises(RconAuthError):
            c.connect()
    finally:
        srv.close()


def test_multi_packet_response():
    srv = FakeRconServer(password="pw")
    try:
        c = RconClient(host="127.0.0.1", port=srv.port, password="pw", timeout=5)
        c.connect()
        assert c.command("BIG") == "PART1PART2"
        c.close()
    finally:
        srv.close()


def test_close_idempotent():
    srv = FakeRconServer(password="pw")
    try:
        c = RconClient(host="127.0.0.1", port=srv.port, password="pw", timeout=5)
        c.connect()
        c.close()
        c.close()  # should not raise
    finally:
        srv.close()


def test_command_without_connect_raises():
    c = RconClient(host="127.0.0.1", port=1, password="pw", timeout=1)
    with pytest.raises(RconError):
        c.command("x")

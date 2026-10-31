"""Minimal RCON client for the local test server. Usage: python rcon.py "<command>" [...]"""
import socket
import struct
import sys

HOST, PORT, PASSWORD = "127.0.0.1", 25575, "praxion-local"


def packet(req_id: int, kind: int, body: str) -> bytes:
    payload = struct.pack("<ii", req_id, kind) + body.encode("utf-8") + b"\x00\x00"
    return struct.pack("<i", len(payload)) + payload


def read_packet(sock: socket.socket) -> tuple[int, int, str]:
    def read_exact(n: int) -> bytes:
        buf = b""
        while len(buf) < n:
            chunk = sock.recv(n - len(buf))
            if not chunk:
                raise ConnectionError("rcon closed")
            buf += chunk
        return buf

    (length,) = struct.unpack("<i", read_exact(4))
    data = read_exact(length)
    req_id, kind = struct.unpack("<ii", data[:8])
    return req_id, kind, data[8:-2].decode("utf-8", "replace")


def main() -> None:
    with socket.create_connection((HOST, PORT), timeout=10) as sock:
        sock.sendall(packet(1, 3, PASSWORD))
        if read_packet(sock)[0] == -1:
            sys.exit("rcon: auth failed")
        for i, command in enumerate(sys.argv[1:], start=2):
            sock.sendall(packet(i, 2, command))
            print(read_packet(sock)[2])


if __name__ == "__main__":
    main()

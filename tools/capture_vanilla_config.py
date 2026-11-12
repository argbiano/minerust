#!/usr/bin/env python3
"""Capture the configuration-phase data a vanilla server sends to a client.

Run a vanilla Minecraft server with `online-mode=false` and
`network-compression-threshold=-1`, then:

    python3 tools/capture_vanilla_config.py 127.0.0.1 25566 data/vanilla_config.bin

With --full the client claims to know no packs and the server sends every
registry entry with its NBT body, which is what non-vanilla clients get.

The output is a sequence of `[varint packet_id][varint length][payload]` records
containing the registry_data, update_tags and update_enabled_features packets.
The client answers select_known_packs with the server's own list, so registry
entries arrive without NBT bodies (the client loads them from its built-in pack).
"""
import socket
import struct
import sys
import uuid
import zlib

PROTOCOL = 777

# configuration clientbound ids (26.3, from the data generator's packets.json)
CB_CUSTOM_PAYLOAD = 1
CB_DISCONNECT = 2
CB_FINISH = 3
CB_KEEP_ALIVE = 4
CB_PING = 5
CB_REGISTRY_DATA = 7
CB_ENABLED_FEATURES = 13
CB_UPDATE_TAGS = 14
CB_SELECT_KNOWN_PACKS = 15
CAPTURED = {CB_ENABLED_FEATURES, CB_SELECT_KNOWN_PACKS, CB_REGISTRY_DATA, CB_UPDATE_TAGS}


def varint(v):
    v &= 0xFFFFFFFF
    out = bytearray()
    while True:
        b = v & 0x7F
        v >>= 7
        if v:
            out.append(b | 0x80)
        else:
            out.append(b)
            return bytes(out)


def read_varint_buf(buf, pos):
    result = shift = 0
    while True:
        b = buf[pos]
        pos += 1
        result |= (b & 0x7F) << shift
        if not b & 0x80:
            return result, pos
        shift += 7


def string(s):
    b = s.encode()
    return varint(len(b)) + b


class Conn:
    def __init__(self, host, port):
        self.sock = socket.create_connection((host, port))
        self.rbuf = b""
        self.threshold = None

    def _read(self, n):
        while len(self.rbuf) < n:
            chunk = self.sock.recv(65536)
            if not chunk:
                raise EOFError("connection closed")
            self.rbuf += chunk
        out, self.rbuf = self.rbuf[:n], self.rbuf[n:]
        return out

    def recv(self):
        length = shift = 0
        while True:
            b = self._read(1)[0]
            length |= (b & 0x7F) << shift
            if not b & 0x80:
                break
            shift += 7
        data = self._read(length)
        if self.threshold is not None:
            size, pos = read_varint_buf(data, 0)
            data = zlib.decompress(data[pos:]) if size else data[pos:]
        pid, pos = read_varint_buf(data, 0)
        return pid, data[pos:]

    def send(self, pid, payload=b""):
        body = varint(pid) + payload
        if self.threshold is not None:
            # Below-threshold framing is always valid, so nothing is ever compressed.
            body = varint(0) + body
        self.sock.sendall(varint(len(body)) + body)


def main():
    host, port, out_path = sys.argv[1], int(sys.argv[2]), sys.argv[3]
    # --full: claim to know no packs, so the server sends every registry entry in full
    full = "--full" in sys.argv[4:]
    c = Conn(host, port)
    c.send(0, varint(PROTOCOL) + string(host) + struct.pack(">H", port) + varint(2))
    c.send(0, string("capture") + uuid.uuid4().bytes)

    pid, payload = c.recv()
    if pid == 3:  # login_compression
        c.threshold, _ = read_varint_buf(payload, 0)
        pid, payload = c.recv()
    if pid != 2:
        raise SystemExit(f"expected login_finished, got packet {pid}: {payload!r}")
    c.send(3)  # login_acknowledged

    records = bytearray()
    while True:
        pid, payload = c.recv()
        if pid == CB_SELECT_KNOWN_PACKS:
            c.send(7, varint(0) if full else payload)
        elif pid == CB_KEEP_ALIVE:
            c.send(4, payload)
        elif pid == CB_PING:
            c.send(5, payload)
        elif pid == CB_DISCONNECT:
            raise SystemExit(f"disconnected: {payload!r}")
        elif pid == CB_FINISH:
            break
        if pid in CAPTURED:
            records += varint(pid) + varint(len(payload)) + payload
    with open(out_path, "wb") as f:
        f.write(records)
    print(f"wrote {len(records)} bytes to {out_path}")


if __name__ == "__main__":
    main()

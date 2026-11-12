#!/usr/bin/env python3
"""Print a summary of a capture written by capture_vanilla_config.py."""
import sys

from capture_vanilla_config import read_varint_buf


def read_string(buf, pos):
    n, pos = read_varint_buf(buf, pos)
    return buf[pos:pos + n].decode(), pos + n


def main():
    data = open(sys.argv[1], "rb").read()
    pos = 0
    while pos < len(data):
        pid, pos = read_varint_buf(data, pos)
        length, pos = read_varint_buf(data, pos)
        payload = data[pos:pos + length]
        pos += length
        if pid == 7:
            registry, q = read_string(payload, 0)
            count, q = read_varint_buf(payload, q)
            with_data = 0
            for _ in range(count):
                _, q = read_string(payload, q)
                if payload[q]:
                    with_data += 1
                    break
                q += 1
            print(f"registry_data {registry}: {count} entries, with_data={with_data}")
        else:
            print(f"packet {pid}: {length} bytes")


if __name__ == "__main__":
    main()

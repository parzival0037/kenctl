#!/usr/bin/env python
"""Stage probe of the Kensington vendor HID config interface via raw hidraw.

The config interface (iface 1, report page 0xFF00) exposes a 32-byte input
report and a 32-byte output report with NO report ids and NO feature reports,
so plain hidraw read/write is the correct transport.

Usage:
  probe_config.py stage1     passive read only
  probe_config.py stage2     + single 32-byte zero write
  probe_config.py stage3     + bounded opcode probe (single byte, rest zero)
"""
import errno
import os
import select
import sys
import time

VID = 0x047D
PID = 0x8018
CONFIG_IFACE = 1
REPORT_LEN = 32


def find_config_hidraw():
    base = "/sys/bus/hid/devices"
    for name in sorted(os.listdir(base)):
        if not name.startswith(f"0003:{VID:04X}:{PID:04X}"):
            continue
        dev = os.path.join(base, name)
        with open(os.path.join(dev, "report_descriptor"), "rb") as f:
            desc = f.read()
        if b"\x06\x00\xff" not in desc:  # vendor usage page 0xFF00
            continue
        if b"\x85\x01" in desc:  # mouse interface, has report ids
            continue
        hr = os.path.join(dev, "hidraw")
        node = os.path.join("/dev", os.listdir(hr)[0])
        return node, desc
    raise SystemExit("config interface not found")


def read_reports(fd, seconds=0.4):
    out = []
    end = time.time() + seconds
    while time.time() < end:
        timeout = max(0.0, end - time.time())
        r, _, _ = select.select([fd], [], [], min(timeout, 0.1))
        if not r:
            continue
        try:
            data = os.read(fd, 64)
        except OSError as e:
            if e.errno in (errno.EAGAIN, errno.EWOULDBLOCK):
                continue
            raise
        if data:
            out.append(bytes(data))
            time.sleep(0.02)
    return out


def send(fd, payload, label):
    assert len(payload) == REPORT_LEN, f"{label}: bad len {len(payload)}"
    n = os.write(fd, payload)
    got = read_reports(fd, 0.35)
    print(f"  [{label}] wrote {n} -> {len(got)} report(s)")
    for r in got:
        print(f"      {r.hex()}")
    return got


def main():
    stage = sys.argv[1] if len(sys.argv) > 1 else "stage1"
    node, desc = find_config_hidraw()
    print(f"config hidraw: {node}  (descriptor {len(desc)} bytes)")
    print(f"descriptor: {desc.hex()}")

    fd = os.open(node, os.O_RDWR | os.O_NONBLOCK)
    try:
        print("\n-- stage1: passive read --")
        got = read_reports(fd, 1.0)
        print(f"  unsolicited reports: {len(got)}")
        for r in got:
            print(f"      {r.hex()}")

        if stage in ("stage2", "stage3"):
            print("\n-- stage2: zero write --")
            send(fd, bytes(REPORT_LEN), "zero")

        if stage == "stage3":
            print("\n-- stage3: opcode probe --")
            opcodes = list(range(0x00, 0x21)) + [0x80, 0x81, 0x90, 0xA0, 0xC0, 0xF0]
            responsive = []
            for op in opcodes:
                payload = bytearray(REPORT_LEN)
                payload[0] = op
                got = send(fd, bytes(payload), f"op=0x{op:02X}")
                if got:
                    responsive.append((op, got))
            print("\n== responsive opcodes ==")
            for op, got in responsive:
                print(f"  0x{op:02X} -> {[r.hex() for r in got]}")
            if not responsive:
                print("  none")
    finally:
        os.close(fd)


if __name__ == "__main__":
    main()

#!/usr/bin/env python
"""Determine which write sizes/headers the vendor OUT endpoint accepts."""
import errno
import os
import select
import sys
import time

ERR = {
    errno.EPROTO: "EPROTO(stall/protocol)",
    errno.EINVAL: "EINVAL",
    errno.EPIPE: "EPIPE(stall)",
    errno.ETIMEDOUT: "ETIMEDOUT",
    errno.ENOSYS: "ENOSYS",
    errno.EIO: "EIO",
    errno.EOVERFLOW: "EOVERFLOW",
    errno.ERANGE: "ERANGE",
}


def node():
    base = "/sys/bus/hid/devices"
    for name in sorted(os.listdir(base)):
        if not name.startswith("0003:047D:8018"):
            continue
        d = os.path.join(base, name)
        desc = open(os.path.join(d, "report_descriptor"), "rb").read()
        if b"\x06\x00\xff" in desc and b"\x85\x01" not in desc:
            return os.path.join("/dev", os.listdir(os.path.join(d, "hidraw"))[0])
    raise SystemExit("not found")


def drain(fd, secs=0.25):
    out = []
    end = time.time() + secs
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], 0.05)
        if r:
            try:
                out.append(bytes(os.read(fd, 64)))
            except OSError:
                pass
    return out


def try_write(fd, payload):
    try:
        n = os.write(fd, payload)
        return ("OK", n, drain(fd))
    except OSError as e:
        return (ERR.get(e.errno, f"errno{e.errno}"), 0, [])


def main():
    n = node()
    print(f"node: {n}")

    fd = os.open(n, os.O_RDWR | os.O_NONBLOCK)
    try:
        print("\n== size matrix (payload = [len,0,0,...]) ==")
        for size in (1, 2, 4, 8, 16, 31, 32, 33, 40, 64):
            p = bytearray(size)
            p[0] = size & 0xFF
            st, w, got = try_write(fd, bytes(p))
            print(f"  size={size:3d} -> {st:22s} wrote={w} resp={[r.hex() for r in got]}")

        print("\n== header byte sweep at fixed size 32 ==")
        ok = []
        for head in range(0, 0x100, 1):
            p = bytearray(32)
            p[0] = head
            st, w, got = try_write(fd, bytes(p))
            if st == "OK":
                ok.append((head, got))
            if got:
                print(f"  head=0x{head:02X} OK wrote={w} resp={[r.hex() for r in got]}")
        print(f"\n  accepted headers: {len(ok)}/256")
        print("  " + " ".join(f"{h:02x}" for h, _ in ok[:80]) + (" ..." if len(ok) > 80 else ""))
    finally:
        os.close(fd)


if __name__ == "__main__":
    main()

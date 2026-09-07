"""Command line:  python -m dbk2jp <command>

    devices            list DBK2JP interface paths on this machine
    status             one-shot status: unlock, inputs, cache
    unlock [set]       replay the auth frames (default: the 3-frame minimum)
    inputs [secs]      live input/SGIN view, edge timings on exit
    out <port> <0|1>   set an output port
    jump <x> <y>       move the galvos (hex or decimal)
"""

import sys
import time

from . import protocol as S
from .usb import Board, find_devices
from .unlock import unlock, encrypt_state, SETS
from .job import Job


def _inputs(secs):
    with Job(unlock_now=False) as j:
        t0, prev, edges = time.time(), None, {}
        while secs is None or time.time() - t0 < secs:
            st = j.status()
            if st is None:
                continue
            w = (st[5] << 8) | st[6]
            cur = ((w >> 8) & 0x0F, bool(st[2] & Job.SGIN_BIT))
            if cur != prev:
                if prev is not None:
                    for i in range(4):
                        if (cur[0] ^ prev[0]) >> i & 1:
                            edges.setdefault("IN%d" % i if i < 3 else "REMARK",
                                             []).append(time.time() - t0)
                    if cur[1] != prev[1]:
                        edges.setdefault("SGIN", []).append(time.time() - t0)
                print("%7.3f  word=%04x  IN0=%d IN1=%d IN2=%d REMARK=%d  SGIN=%d  cache=%d"
                      % (time.time() - t0, w, cur[0] & 1, cur[0] >> 1 & 1,
                         cur[0] >> 2 & 1, cur[0] >> 3 & 1, cur[1], w & 0xFF))
                prev = cur
            time.sleep(0.004)
        for name, ts in sorted(edges.items()):
            if len(ts) > 2:
                d = [ts[i + 1] - ts[i] for i in range(len(ts) - 1)]
                print("%s: %d edges, mean half-period %.3f s"
                      % (name, len(ts), sum(d) / len(d)))


def main(argv):
    if not argv or argv[0] in ("-h", "--help", "help"):
        print(__doc__)
        return 0
    what, args = argv[0], argv[1:]

    if what == "devices":
        paths = find_devices()
        print("\n".join(paths) if paths else "no DBK2JP found")
        return 0 if paths else 1

    if what == "unlock":
        name = args[0] if args else "bare"
        if name not in SETS:
            print("unknown set %r -- pick one of %s" % (name, ", ".join(SETS)))
            return 2
        with Board() as b:
            ok = unlock(b, SETS[name])
            print("UNLOCKED / LED green" if ok else "still locked")
        return 0 if ok else 1

    if what == "status":
        with Job(unlock_now=False) as j:
            st = j.status()
            print("path       ", j.b.path)
            print("status     ", st.hex(" ") if st else "no reply")
            print("unlocked   ", j.unlocked())
            print("armed      ", j.armed())
            print("inputs     ", format(j.inputs() & 0x0F, "04b"), "(REMARK IN2 IN1 IN0)")
            print("sgin ok    ", j.sgin())
            print("free cache ", j.free_cache(), "of 256")
        return 0

    if what == "inputs":
        _inputs(float(args[0]) if args else None)
        return 0

    if what == "out":
        port, val = int(args[0], 0), int(args[1], 0)
        with Job(unlock_now=False) as j:
            j.out(port, val)
        return 0

    if what == "jump":
        x, y = int(args[0], 0), int(args[1], 0)
        with Job() as j:
            j.jump(x, y)
            time.sleep(0.2)
        return 0

    print("unknown command %r" % what)
    print(__doc__)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))

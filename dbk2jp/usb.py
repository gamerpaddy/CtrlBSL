"""
Transport for the BSL/SeaCAD DBK2JP board (Cypress FX2LP, VID 04B4 PID 1004).

One public class, `Board`, on every platform. The platform-specific part is only
how bytes reach the endpoints, so that lives behind a backend:

    _cyusb.py    Windows -- the stock CYUSB3.sys driver's IOCTLs
    _libusb.py   everything else -- plain bulk transfers through pyusb

A backend supplies `find_devices()` and a `Transport` with `xfer`, `abort_pipe`,
`reset_pipe` and `close`. Everything above that -- the command framing, the
stale-reply fix, endpoint recovery -- is shared and lives here.
"""

import sys
import time

VID, PID = 0x04B4, 0x1004

EP_DATA_OUT = 0x02          # job batch: parameters and geometry
EP_CTRL_OUT = 0x06          # session/control commands
EP_DATA_IN = 0x84
EP_CTRL_IN = 0x88           # replies

if sys.platform == "win32":
    from . import _cyusb as _backend
else:
    from . import _libusb as _backend

BACKEND = _backend.__name__.rsplit(".", 1)[-1]


def find_devices(any_vidpid=False):
    """Every DBK2JP on this machine, as backend-specific path strings."""
    return _backend.find_devices(any_vidpid)


def enumerate_paths():
    """Deprecated alias for find_devices()."""
    return find_devices()


class Board:
    """Open transport to one DBK2JP board.

        with Board() as b:
            b.ask(cmd(0x0101))
    """

    def __init__(self, path=None, index=0, backend=None):
        self._t = (backend or _backend).Transport(path=path, index=index)

    @property
    def path(self):
        return self._t.path

    def close(self):
        self._t.close()

    def __enter__(self):
        return self

    def __exit__(self, *a):
        self.close()

    # ---- raw ------------------------------------------------------------

    def _xfer(self, ep, payload=b"", read_len=0, timeout_ms=3000):
        return self._t.xfer(ep, payload, read_len, timeout_ms)

    def abort_pipe(self, ep):
        return self._t.abort_pipe(ep)

    def reset_pipe(self, ep):
        return self._t.reset_pipe(ep)

    def recover(self):
        """Clear any stalled/pending state on all four endpoints."""
        for ep in (EP_CTRL_IN, EP_DATA_IN, EP_CTRL_OUT, EP_DATA_OUT):
            self.abort_pipe(ep)
            self.reset_pipe(ep)

    # ---- framed ---------------------------------------------------------

    def write_cmd(self, cmd):
        """One 12-byte command on EP 0x06."""
        assert len(cmd) == 12
        nt, usbd, _, moved = self._xfer(EP_CTRL_OUT, cmd)
        return nt, usbd, moved

    def write_data(self, blob):
        """Batched 12-byte commands on EP 0x02."""
        assert len(blob) % 12 == 0 and len(blob) >= 12
        nt, usbd, _, moved = self._xfer(EP_DATA_OUT, blob)
        return nt, usbd, moved

    def read_status(self, ep=EP_CTRL_IN, timeout_ms=2000):
        nt, usbd, data, moved = self._xfer(ep, read_len=12, timeout_ms=timeout_ms)
        return nt, usbd, data[:moved] if moved else data

    def ask(self, cmd_bytes, timeout_ms=1200, retries=3, settle=0.04):
        """Send a command on EP 0x06 and return ITS reply, not a stale one.

        EP 0x88 runs one behind after a burst of mixed opcodes, so a plain
        write-then-read hands back the previous command's answer. Matching the
        echoed opcode and discarding mismatches is the fix; without it status
        reads intermittently carry another command's payload.
        """
        self.write_cmd(cmd_bytes)
        time.sleep(settle)
        for _ in range(retries):
            try:
                r = self.read_status(EP_CTRL_IN, timeout_ms)[2]
            except Exception:
                return None
            if len(r) >= 2 and r[:2] == cmd_bytes[:2]:
                return r
            time.sleep(0.01)
        return None


SeaBoard = Board          # old name, so existing scripts still import

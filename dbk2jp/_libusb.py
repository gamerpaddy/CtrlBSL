"""
Linux / macOS transport: plain libusb through pyusb.

Selected automatically by usb.py off win32. Exposes find_devices() and
Transport, the same pair every backend provides.

The board's protocol is nothing but bulk transfers on four endpoints, so no
driver-specific layer is needed here -- the CYUSB3 IOCTL wrapper the Windows
backend goes through has no equivalent and no purpose.

Requires pyusb (`pip install pyusb`) and libusb. On Linux the device is claimed
by no kernel driver by default, but usbfs access needs either root or a udev
rule:

    # /etc/udev/rules.d/99-dbk2jp.rules
    SUBSYSTEM=="usb", ATTR{idVendor}=="04b4", ATTR{idProduct}=="1004", MODE="0666"

UNTESTED -- written against the protocol, not verified on Linux hardware.
"""

VID, PID = 0x04B4, 0x1004


def _usb():
    try:
        import usb.core
        import usb.util
        return usb.core, usb.util
    except ImportError:
        raise RuntimeError(
            "pyusb is required on this platform: pip install pyusb")


def find_devices(any_vidpid=False):
    """-> ['usb:<bus>.<address>', ...] -- the cross-platform stand-in for the
    Windows interface paths."""
    core, _ = _usb()
    return ["usb:%d.%d" % (d.bus, d.address)
            for d in core.find(find_all=True, idVendor=VID, idProduct=PID)]


class Transport:
    """One claimed libusb handle."""

    def __init__(self, path=None, index=0):
        core, util = _usb()
        self._util = util
        devs = list(core.find(find_all=True, idVendor=VID, idProduct=PID))
        if not devs:
            raise RuntimeError("no DBK2JP found (VID %04X PID %04X)" % (VID, PID))
        if path:
            want = path.split(":", 1)[-1]
            devs = [d for d in devs if "%d.%d" % (d.bus, d.address) == want]
            if not devs:
                raise RuntimeError("no board at %s" % path)
        elif index >= len(devs):
            raise RuntimeError("only %d board(s) present" % len(devs))

        self.dev = devs[0] if path else devs[index]
        self.path = "usb:%d.%d" % (self.dev.bus, self.dev.address)

        try:
            if self.dev.is_kernel_driver_active(0):
                self.dev.detach_kernel_driver(0)
        except Exception:
            pass                      # not supported on every platform
        try:
            self.dev.set_configuration()
        except Exception:
            pass                      # already configured is fine
        try:
            util.claim_interface(self.dev, 0)
        except Exception as e:
            raise RuntimeError(
                "cannot claim %s: %s. On Linux this is usually permissions -- "
                "add a udev rule (see the module docstring) or run as root."
                % (self.path, e))

    def close(self):
        if getattr(self, "dev", None) is not None:
            try:
                self._util.release_interface(self.dev, 0)
                self._util.dispose_resources(self.dev)
            except Exception:
                pass
            self.dev = None

    def abort_pipe(self, ep):
        return False                  # no libusb equivalent; clear_halt is below

    def reset_pipe(self, ep):
        try:
            self.dev.clear_halt(ep)
            return True
        except Exception:
            return False

    def xfer(self, ep, payload=b"", read_len=0, timeout_ms=3000):
        """-> (nt_status, usbd_status, data, bytes_moved)

        The two status words exist only to match the Windows backend's shape;
        libusb signals failure by raising, so they are always 0 on success.
        """
        import usb.core as core
        try:
            if read_len:
                got = self.dev.read(ep, read_len, timeout_ms)
                data = bytes(got)
                return 0, 0, data + b"\x00" * (read_len - len(data)), len(data)
            moved = self.dev.write(ep, payload, timeout_ms)
            return 0, 0, b"", moved
        except core.USBTimeoutError:
            raise TimeoutError("transfer timed out")

"""
Windows transport: the stock CYUSB3.sys driver, driven through its IOCTLs.

Selected automatically by usb.py on win32. Exposes find_devices() and
Transport, the same pair every backend provides.

IOCTL codes and the SINGLE_TRANSFER layout were extracted from the CyAPI code
statically linked into SeaMark.dll -- see DBK2JP_PROTOCOL.md.

    0x220024  send non-EP0 transfer   (bulk)
    0x220020  send EP0 control transfer
    0x22002C  reset pipe              (in: 1 byte ep address)
    0x220044  abort pipe
    0x220038  set transfer size       (in: 1 byte ep + u32 size)

SINGLE_TRANSFER header is 0x26 bytes; for a bulk transfer only these matter:
    +0x0D u8   endpoint address
    +0x0E u32  NtStatus      (out)
    +0x12 u32  UsbdStatus    (out)
    +0x1E u32  BufferOffset  (= 0x26)
    +0x22 u32  BufferLength
    +0x26      payload
"""

import ctypes as ct
import struct
from ctypes import wintypes

VID, PID = 0x04B4, 0x1004
HWID = "vid_%04x&pid_%04x" % (VID, PID)

# Cypress's stock CyUSB interface GUID. It is a DRIVER guid, not a device one --
# a board bound by a different OEM .inf gets a different one (a WinUSB-bound
# instance of this same VID/PID was seen carrying
# {090DE41C-61CA-48A8-AAA8-BBBB057F58A1}), so it is only the fallback. Discovery
# reads the real GUID out of the registry; see find_devices().
CYUSBDRV_GUID = "{AE18AA60-7F6A-11D4-97DD-00010229B959}"

# Only CYUSB3 speaks the IOCTLs below. A board bound to WinUSB or libusb still
# enumerates but rejects every transfer.
CY_SERVICES = ("CYUSB3", "CYUSB")

IOCTL_NON_EP0_XFER = 0x220024
IOCTL_RESET_PIPE = 0x22002C
IOCTL_ABORT_PIPE = 0x220044
IOCTL_SET_XFER_SIZE = 0x220038

HDR = 0x26

GENERIC_READ = 0x80000000
GENERIC_WRITE = 0x40000000
FILE_SHARE_RW = 0x03
OPEN_EXISTING = 3
FILE_FLAG_OVERLAPPED = 0x40000000
INVALID_HANDLE = ct.c_void_p(-1).value
DIGCF_PRESENT = 0x02
DIGCF_DEVICEINTERFACE = 0x10
ERROR_IO_PENDING = 997

setupapi = ct.WinDLL("setupapi", use_last_error=True)
kernel32 = ct.WinDLL("kernel32", use_last_error=True)

# Explicit signatures: on 64-bit the default int restype truncates HANDLEs.
setupapi.SetupDiGetClassDevsW.restype = ct.c_void_p
setupapi.SetupDiGetClassDevsW.argtypes = [ct.c_void_p, ct.c_wchar_p, ct.c_void_p, ct.c_ulong]
setupapi.SetupDiEnumDeviceInterfaces.argtypes = [
    ct.c_void_p, ct.c_void_p, ct.c_void_p, ct.c_ulong, ct.c_void_p]
setupapi.SetupDiGetDeviceInterfaceDetailW.argtypes = [
    ct.c_void_p, ct.c_void_p, ct.c_void_p, ct.c_ulong, ct.c_void_p, ct.c_void_p]
setupapi.SetupDiDestroyDeviceInfoList.argtypes = [ct.c_void_p]

kernel32.CreateFileW.restype = ct.c_void_p
kernel32.CreateFileW.argtypes = [ct.c_wchar_p, ct.c_ulong, ct.c_ulong, ct.c_void_p,
                                 ct.c_ulong, ct.c_ulong, ct.c_void_p]
kernel32.CreateEventW.restype = ct.c_void_p
kernel32.CreateEventW.argtypes = [ct.c_void_p, ct.c_int, ct.c_int, ct.c_wchar_p]
kernel32.CloseHandle.argtypes = [ct.c_void_p]
kernel32.DeviceIoControl.argtypes = [ct.c_void_p, ct.c_ulong, ct.c_void_p, ct.c_ulong,
                                     ct.c_void_p, ct.c_ulong, ct.c_void_p, ct.c_void_p]
kernel32.GetOverlappedResult.argtypes = [ct.c_void_p, ct.c_void_p, ct.c_void_p, ct.c_int]
kernel32.WaitForSingleObject.argtypes = [ct.c_void_p, ct.c_ulong]
kernel32.WaitForSingleObject.restype = ct.c_ulong
kernel32.CancelIo.argtypes = [ct.c_void_p]


class GUID(ct.Structure):
    _fields_ = [("Data1", ct.c_ulong), ("Data2", ct.c_ushort),
                ("Data3", ct.c_ushort), ("Data4", ct.c_ubyte * 8)]

    @classmethod
    def parse(cls, s):
        s = s.strip("{}")
        p = s.split("-")
        d4 = bytes.fromhex(p[3] + p[4])
        return cls(int(p[0], 16), int(p[1], 16), int(p[2], 16),
                   (ct.c_ubyte * 8)(*d4))


class SP_DEVICE_INTERFACE_DATA(ct.Structure):
    _fields_ = [("cbSize", ct.c_ulong), ("InterfaceClassGuid", GUID),
                ("Flags", ct.c_ulong), ("Reserved", ct.POINTER(ct.c_ulong))]


class OVERLAPPED(ct.Structure):
    _fields_ = [("Internal", ct.c_void_p), ("InternalHigh", ct.c_void_p),
                ("Offset", ct.c_ulong), ("OffsetHigh", ct.c_ulong),
                ("hEvent", ct.c_void_p)]



def _registry_guids():
    """Interface GUIDs registered for this VID/PID, CYUSB3 instances first.

    The interface GUID is not a constant -- it comes from whichever .inf bound
    the board. Reading it from the device's own registry key is what lets this
    run on a machine other than the one it was developed on.
    """
    import winreg
    key = "SYSTEM\\CurrentControlSet\\Enum\\USB\\VID_%04X&PID_%04X" % (VID, PID)
    cy, other = [], []
    try:
        root = winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE, key)
    except OSError:
        return []
    with root:
        for i in range(winreg.QueryInfoKey(root)[0]):
            try:
                inst = winreg.EnumKey(root, i)
            except OSError:
                continue
            service = ""
            try:
                with winreg.OpenKey(root, inst) as k:
                    service = winreg.QueryValueEx(k, "Service")[0]
            except OSError:
                pass
            found = []
            try:
                with winreg.OpenKey(root, inst + "\\Device Parameters") as dp:
                    for name in ("DeviceInterfaceGUIDs", "DeviceInterfaceGUID",
                                 "DriverGUID"):
                        try:
                            v = winreg.QueryValueEx(dp, name)[0]
                        except OSError:
                            continue
                        found += [v] if isinstance(v, str) else list(v)
            except OSError:
                pass
            bucket = cy if service.upper() in CY_SERVICES else other
            bucket += [g.strip("{} ") for g in found if g]
    seen, out = set(), []
    for g in cy + other:
        if g and g.lower() not in seen:
            seen.add(g.lower())
            out.append("{%s}" % g)
    return out


def _paths_for_guid(guid_str):
    guid = GUID.parse(guid_str)
    hdev = setupapi.SetupDiGetClassDevsW(ct.byref(guid), None, None,
                                         DIGCF_PRESENT | DIGCF_DEVICEINTERFACE)
    if hdev == INVALID_HANDLE:
        return []
    paths = []
    try:
        idx = 0
        while True:
            did = SP_DEVICE_INTERFACE_DATA()
            did.cbSize = ct.sizeof(did)
            if not setupapi.SetupDiEnumDeviceInterfaces(hdev, None, ct.byref(guid),
                                                        idx, ct.byref(did)):
                break
            need = ct.c_ulong(0)
            setupapi.SetupDiGetDeviceInterfaceDetailW(hdev, ct.byref(did), None, 0,
                                                      ct.byref(need), None)
            buf = ct.create_string_buffer(need.value)
            # cbSize of SP_DEVICE_INTERFACE_DETAIL_DATA_W is 6 on 32-bit, 8 on 64-bit
            ct.memmove(buf, struct.pack("I", 8 if ct.sizeof(ct.c_void_p) == 8 else 6), 4)
            if setupapi.SetupDiGetDeviceInterfaceDetailW(hdev, ct.byref(did), buf,
                                                         need, ct.byref(need), None):
                paths.append(ct.wstring_at(ct.addressof(buf) + 4))
            idx += 1
    finally:
        setupapi.SetupDiDestroyDeviceInfoList(hdev)
    return paths


def find_devices(any_vidpid=False):
    """Every DBK2JP interface path on this machine.

    Tries the GUIDs the registry says this VID/PID is bound to, then Cypress's
    stock one, and keeps only paths carrying our VID/PID -- a machine can hold
    several CyUSB devices, and taking the first one enumerated is a coin flip.
    """
    paths, seen = [], set()
    for guid in _registry_guids() + [CYUSBDRV_GUID]:
        for p in _paths_for_guid(guid):
            low = p.lower()
            if low in seen:
                continue
            if any_vidpid or HWID in low:
                seen.add(low)
                paths.append(p)
    return paths


def enumerate_paths():
    """Deprecated alias for find_devices()."""
    return find_devices()

class Transport:
    """One open CYUSB3 handle."""

    def __init__(self, path=None, index=0):
        if path is None:
            paths = find_devices()
            if not paths:
                raise RuntimeError(
                    "no DBK2JP found (VID %04X PID %04X): unplugged, or bound to "
                    "a driver other than CYUSB3" % (VID, PID))
            if index >= len(paths):
                raise RuntimeError("only %d board(s) present" % len(paths))
            path = paths[index]
        self.path = path
        self.h = kernel32.CreateFileW(path, GENERIC_READ | GENERIC_WRITE,
                                      FILE_SHARE_RW, None, OPEN_EXISTING,
                                      FILE_FLAG_OVERLAPPED, None)
        if self.h == INVALID_HANDLE:
            raise ct.WinError(ct.get_last_error())

    def close(self):
        if self.h and self.h != INVALID_HANDLE:
            kernel32.CloseHandle(self.h)
            self.h = None

    def _ioctl(self, code, inbuf, insize, outbuf, outsize, timeout_ms=3000):
        ov = OVERLAPPED()
        ov.hEvent = kernel32.CreateEventW(None, 1, 0, None)
        got = ct.c_ulong(0)
        try:
            ok = kernel32.DeviceIoControl(self.h, code, inbuf, insize,
                                          outbuf, outsize, ct.byref(got), ct.byref(ov))
            if not ok:
                err = ct.get_last_error()
                if err != ERROR_IO_PENDING:
                    raise ct.WinError(err)
                if kernel32.WaitForSingleObject(ov.hEvent, timeout_ms) != 0:
                    kernel32.CancelIo(self.h)
                    raise TimeoutError("transfer timed out")
                if not kernel32.GetOverlappedResult(self.h, ct.byref(ov),
                                                    ct.byref(got), 0):
                    raise ct.WinError(ct.get_last_error())
            return got.value
        finally:
            kernel32.CloseHandle(ov.hEvent)

    def _pipe_ioctl(self, code, ep):
        """Abort/reset take a 1-byte endpoint address. The handle is overlapped,
        so these must be issued overlapped too."""
        b = ct.create_string_buffer(bytes([ep]), 1)
        try:
            self._ioctl(code, b, 1, None, 0, 1000)
            return True
        except Exception:
            return False

    def abort_pipe(self, ep):
        return self._pipe_ioctl(IOCTL_ABORT_PIPE, ep)

    def reset_pipe(self, ep):
        return self._pipe_ioctl(IOCTL_RESET_PIPE, ep)

    def xfer(self, ep, payload=b"", read_len=0, timeout_ms=3000):
        """-> (nt_status, usbd_status, data, bytes_moved)"""
        n = read_len if read_len else len(payload)
        buf = ct.create_string_buffer(HDR + n)
        ct.memset(buf, 0, HDR + n)
        struct.pack_into("<B", buf, 0x0D, ep)
        struct.pack_into("<I", buf, 0x1E, HDR)
        struct.pack_into("<I", buf, 0x22, n)
        if payload:
            ct.memmove(ct.addressof(buf) + HDR, payload, len(payload))
        got = self._ioctl(IOCTL_NON_EP0_XFER, buf, HDR + n, buf, HDR + n, timeout_ms)
        nt = struct.unpack_from("<I", buf, 0x0E)[0]
        usbd = struct.unpack_from("<I", buf, 0x12)[0]
        data = bytes(buf[HDR:HDR + n])
        moved = max(0, got - HDR)
        return nt, usbd, data, moved

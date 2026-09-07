"""
dbk2jp -- drive a BSL/SeaCAD DBK2JP galvo laser controller directly over USB.

    from dbk2jp import Job

    with Job() as j:                 # opens, unlocks, leaves the board armed
        j.laser(freq_khz=20, power_pct=50)
        j.jump(0x4000, 0x8000)
        j.pwm_burst(seconds=5)

Layout:
    usb.py       device discovery + CYUSB3 IOCTL transport (Board)
    protocol.py  the 12-byte tagSeaCMD wire format and parameter packing
    unlock.py    the 3-frame ATSHA204 replay that turns the LED green
    job.py       the high-level API (Job)

THE RULE THAT MATTERS: session/control commands go on EP 0x06; anything that
configures a job goes inline in the EP 0x02 batch ahead of the vectors. The
board ACKs parameter commands sent on EP 0x06 and then silently ignores them.

See API.md for the full surface and DBK2JP_PROTOCOL.md for the reverse
engineering behind it.
"""

from .usb import Board, SeaBoard, find_devices, VID, PID
from .protocol import cmd, parse, set_power_0210, set_power_raw
from .unlock import unlock, encrypt_state, FRAMES, SETS
from .job import (Job, CENTRE, LASER_CO2, LASER_FIBER, LASER_UV, LASER_GREEN,
                  LASER_MOPA)

__all__ = [
    "Board", "SeaBoard", "find_devices", "VID", "PID",
    "cmd", "parse", "set_power_0210", "set_power_raw",
    "unlock", "encrypt_state", "FRAMES", "SETS",
    "Job", "CENTRE",
    "LASER_CO2", "LASER_FIBER", "LASER_UV", "LASER_GREEN", "LASER_MOPA",
]

__version__ = "1.0"

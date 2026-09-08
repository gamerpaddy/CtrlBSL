"""
dbk2jp -- drive a BSL/SeaCAD DBK2JP galvo laser controller directly over USB.

    from dbk2jp import Job

    from dbk2jp import Job, CO2

    with Job(CO2) as j:              # opens, unlocks, leaves the board armed
        j.configure(freq_khz=20, power_pct=50)
        j.begin(start=(0x4000, 0x4000))
        j.lines([(0xC000, 0x4000), (0xC000, 0xC000), (0x4000, 0x4000)])

Layout:
    usb.py       device discovery and transport (Board)
    laser.py     laser types and how each one is driven
    field.py     millimetres to galvo counts: field size, offsets, distortion
    cor.py       .cor optical correction (scaffold, format not recovered)
    protocol.py  the 12-byte tagSeaCMD wire format and parameter packing
    unlock.py    the 3-frame ATSHA204 replay that turns the LED green
    job.py       the high-level API (Job)

THE RULE THAT MATTERS: session/control commands go on EP 0x06; anything that
configures a job goes inline in the EP 0x02 batch ahead of the vectors. The
board ACKs parameter commands sent on EP 0x06 and then silently ignores them.

See API.md for the full surface and DBK2JP_PROTOCOL.md for the reverse
engineering behind it.
"""

from .usb import Board, BoardError, SeaBoard, find_devices, VID, PID
from .protocol import cmd, parse, set_power_0210, set_power_raw
from .unlock import unlock, encrypt_state, FRAMES, SETS
from .laser import CO2, FIBER, UV, GREEN, MOPA, YAG, LASERS, Laser
from .field import Field, read_markcfg
from .cor import Correction, GridCorrection, PolyCorrection, load_cor, parse_cor
from .job import (Job, CENTRE, LASER_CO2, LASER_FIBER, LASER_UV, LASER_GREEN,
                  LASER_MOPA)

__all__ = [
    "Board", "BoardError", "SeaBoard", "find_devices", "VID", "PID",
    "cmd", "parse", "set_power_0210", "set_power_raw",
    "unlock", "encrypt_state", "FRAMES", "SETS",
    "Job", "CENTRE",
    "Field", "read_markcfg",
    "Correction", "GridCorrection", "PolyCorrection", "load_cor", "parse_cor",
    "CO2", "FIBER", "UV", "GREEN", "MOPA", "YAG", "LASERS", "Laser",
    "LASER_CO2", "LASER_FIBER", "LASER_UV", "LASER_GREEN", "LASER_MOPA",
]

__version__ = "1.0"

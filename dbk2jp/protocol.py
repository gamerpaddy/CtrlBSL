"""Wire format and parameter packing for the DBK2JP.

Every command is a 12-byte big-endian tagSeaCMD: CMD_ID then five Params.
No header, no checksum, no sequence number.
"""

import struct

# Session / control -- EP 0x06.
CMD_STATUS         = 0x0101
CMD_ENCRYPT_STATE  = 0x0102
CMD_RUN            = 0x0104
CMD_RESET          = 0x0105
CMD_CLEAR_CACHE    = 0x0106
CMD_PORT_OUT       = 0x0111
CMD_PORT_OUT_STATE = 0x0112
CMD_ONLINE         = 0x0118
CMD_REDLIGHT       = 0x0171

# Job parameters and geometry -- MUST go inline in the EP 0x02 batch.
CMD_LASER_TYPE = 0x0211
CMD_POWER      = 0x0210
CMD_TICK       = 0x0217
CMD_PEN_FPK    = 0x0218
CMD_LASER_GATE = 0x0208
CMD_JUMP       = 0x0241
CMD_MARK       = 0x0243
CMD_AXIS_COUNT = 0x0230
CMD_AXIS_RATE  = 0x0231
CMD_AXIS_P232  = 0x0232
CMD_AXIS_GO    = 0x0233
CMD_PORT_PULSE = 0x2F82

FPGA_CLK_KHZ = 48000.0      # timebase for every period/width field


def cmd(cmd_id, p0=0, p1=0, p2=0, p3=0, p4=0):
    """Build a 12-byte tagSeaCMD. All fields big-endian."""
    return struct.pack(">HHHHHH", cmd_id, p0 & 0xFFFF, p1 & 0xFFFF,
                       p2 & 0xFFFF, p3 & 0xFFFF, p4 & 0xFFFF)


def parse(reply):
    """Split a 12-byte reply into (cmd_id, p0, p1, p2, p3, p4)."""
    return struct.unpack(">HHHHHH", reply[:12])


def set_power_0210(freq_khz, power_pct, wait=0):
    """
    Build CMD 0x0210, the marking power/frequency word.

    By raw offset the fields are:
        +0x00 CMD_ID = 0x0210
        +0x02 Param0 = 0
        +0x04 Param1 = (period >> 8) & 0xFF | (round(freq*0.5) << 8)
        +0x06 Param2 = (width  >> 8)        | ((period & 0xFF) << 8)
        +0x08 Param3 = (power*0xFF)/100     | ((width  & 0xFF) << 8)
        +0x0A Param4 = nEffectWaitTime

    period and width are 48 MHz ticks, each split across two params.
    """
    period = int(round(48000.0 / freq_khz))            # ticks, freq in kHz
    duty_us = (power_pct / 100.0) * (1000.0 / freq_khz)
    width = int(round(duty_us * 48.0)) & 0xFFFF        # ticks
    fbyte = int(round(freq_khz * 0.5)) & 0xFF
    p1 = ((period >> 8) & 0xFF) | (fbyte << 8)
    p2 = ((width >> 8) & 0xFF) | ((period & 0xFF) << 8)
    p3 = (((power_pct * 0xFF) // 100) & 0xFF) | ((width & 0xFF) << 8)
    return cmd(0x0210, 0, p1 & 0xFFFF, p2 & 0xFFFF, p3 & 0xFFFF, wait), period, width


def set_power_raw(freq_khz, power_byte, wait=0):
    """0x0210 with an EXPLICIT 8-bit power word instead of a percentage.

    For fiber, that byte is what appears on the parallel power pins P0..P7, so
    driving it directly lets you exercise one bit at a time. Packing is
    identical to set_power_0210; only the power byte differs.
    """
    period = int(round(48000.0 / freq_khz))
    duty_us = 0.5 * (1000.0 / freq_khz)          # fixed 50% so width is sane
    width = int(round(duty_us * 48.0)) & 0xFFFF
    fbyte = int(round(freq_khz * 0.5)) & 0xFF
    p1 = ((period >> 8) & 0xFF) | (fbyte << 8)
    p2 = ((width >> 8) & 0xFF) | ((period & 0xFF) << 8)
    p3 = (power_byte & 0xFF) | ((width & 0xFF) << 8)
    return cmd(0x0210, 0, p1 & 0xFFFF, p2 & 0xFFFF, p3 & 0xFFFF, wait)

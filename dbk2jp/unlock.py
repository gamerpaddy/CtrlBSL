"""
Unlock the DBK2JP board (加密 LED red -> green).

The board carries an Atmel ATSHA204A authentication chip reached through the
0x0C5C / 0x0C5D passthrough on EP 0x06.  The exchange is:

    digestInput[0x00..0x1F] = DataSlot[0]         # 32-byte key, read in clear
    digestInput[0x20..0x3F] = 32 random bytes     # challenge, drawn ONCE per
                                                  # process, then reused
    digest = SHA256(digestInput[0..0x57])         # 88 bytes = ATSHA MAC format
    SendSha256Digest(digest)     -> 0x0C5D + 32 bytes         (frame 10)
    Sleep(40 ms)
    SendSha256RandomData(shaCmd) -> 0x0C5C + MAC cmd          (frame 11)
                                    Sleep(40 ms), 0x0C5C 8888 (frame 12)

The host computes the digest in software and hands it to the FPGA; the chip
computes the same MAC in hardware; the FPGA compares the two and ungates.

So no key is needed to replay: the challenge is host-supplied and constant, the
key travels in the clear, and the chip is on the board -- replaying the captured
frames makes the real chip produce the real answer again.

TIMING IS LOAD-BEARING.  The ATSHA204 needs 40-120 ms per command; back-to-back
replay reads stale result registers and the board stays locked.  The gaps in
FRAMES are the captured inter-frame delays.

SUCCESS IS READ FROM 0x0102 (GetEncryptState), BYTE 7: 0x02 = authenticated,
0x00 = not.  0x0101 bit 5 is a separate "armed/ready" bit set by the reset tail
(frames 67..73) -- it moves without the crypto and the LED stays red, so it is
not an unlock indicator.

The latch is STICKY: once green it survives everything short of a power cycle
(verified -- a deliberately corrupted digest does not re-lock it).  That is why
narrowing SETS below costs one power cycle per experiment.

Self-contained: no capture .pkl needed.

VERIFIED MINIMUM: 3 frames -- 10, 11, 12.  Nothing else in the 201-frame
capture is required to turn the LED green:

    0C5D 0000 <32-byte host digest>                 wait ~60 ms
    0C5C 7727 08000000 <32-byte challenge> <CRC16>  wait ~80 ms
    0C5C 8888                                       (transmit token)

Usage:
    from dbk2jp import Board, unlock
    with Board() as b:
        unlock(b)

    python -m dbk2jp unlock [set]     # bare | rb | wake | core | auth | min | full
"""

import time

from . import usb
from .protocol import cmd

# (gap_after_seconds, frame_hex) -- EP 0x06 writes, each answered on EP 0x88.
#
#  0..1    identify / version
#  2..9    ATSHA204 Read(config, 0x0000) -> card id, and the key into DataSlot
#  10      0x0C5D  host-computed SHA-256 digest, 32 bytes
#  11..17  ATSHA204 MAC(challenge) -> chip digest   <-- the authentication
#  18..66  0x0102 GetEncryptState, then Read(data zone 0x2000/0x3000/0x3800),
#          each issued twice; a licence check on the host side, not the gate
#  67..73  reset / arm: 0x0106 0x0105 0x0104 [EP02 blob] 0x0105 0x0118 0x0105

FRAMES = [
    (0.010, "014000000000000000000001"),                                    # 0
    (0.057, "011000000000000000000000"),
    (0.061, "0C5C77070280000009AD"),                                        # 2  Read config
    (0.071, "0C5C8888"),
    (0.010, "010800000000000000000000"),
    (0.010, "010900000000000000000000"),
    (0.010, "010A00000000000000000000"),
    (0.051, "010B00000000000000000000"),
    (0.113, "0C5CBB"),
    (0.063, "0C5CBB"),
    (0.102, "0C5D000009D6956A72D6622E779074242405FC5CBFEE96B17460B9AF8CC7323408A05959"),   # 10
    (0.060, "0C5C77270800000036215A9F208A3A8C1CA354D8E78898392CC22F256E35C7BC79C23842FBB3D9CD29E2"),  # 11 MAC
    (0.081, "0C5C8888"),
    (0.010, "010800000000000000000000"),
    (0.010, "010900000000000000000000"),
    (0.010, "010A00000000000000000000"),
    (0.051, "010B00000000000000000000"),
    (0.063, "0C5CBB"),                                                      # 17
    (0.053, "010200000000000000000000"),                                    # 18
    (0.060, "0C5C77070282200009B0"),
    (0.071, "0C5C8888"),
    (0.010, "010800000000000000000000"),
    (0.010, "010900000000000000000000"),
    (0.010, "010A00000000000000000000"),
    (0.050, "010B00000000000000000000"),
    (0.113, "0C5CBB"),
    (0.123, "0C5CBB"),
    (0.061, "0C5C77070282200009B0"),
    (0.071, "0C5C8888"),
    (0.010, "010800000000000000000000"),
    (0.010, "010900000000000000000000"),
    (0.010, "010A00000000000000000000"),
    (0.051, "010B00000000000000000000"),
    (0.113, "0C5CBB"),
    (0.113, "0C5CBB"),
    (0.061, "0C5C7707028230000A00"),
    (0.071, "0C5C8888"),
    (0.010, "010800000000000000000000"),
    (0.010, "010900000000000000000000"),
    (0.010, "010A00000000000000000000"),
    (0.051, "010B00000000000000000000"),
    (0.114, "0C5CBB"),
    (0.125, "0C5CBB"),
    (0.060, "0C5C7707028230000A00"),
    (0.070, "0C5C8888"),
    (0.010, "010800000000000000000000"),
    (0.010, "010900000000000000000000"),
    (0.010, "010A00000000000000000000"),
    (0.050, "010B00000000000000000000"),
    (0.114, "0C5CBB"),
    (0.113, "0C5CBB"),
    (0.060, "0C5C77070282380009E0"),
    (0.071, "0C5C8888"),
    (0.010, "010800000000000000000000"),
    (0.010, "010900000000000000000000"),
    (0.010, "010A00000000000000000000"),
    (0.050, "010B00000000000000000000"),
    (0.113, "0C5CBB"),
    (0.123, "0C5CBB"),
    (0.061, "0C5C77070282380009E0"),
    (0.071, "0C5C8888"),
    (0.010, "010800000000000000000000"),
    (0.010, "010900000000000000000000"),
    (0.010, "010A00000000000000000000"),
    (0.051, "010B00000000000000000000"),
    (0.112, "0C5CBB"),
    (0.062, "0C5CBB"),                                                      # 66
    (0.010, "010100000000000000000000"),                                    # 67
    (0.010, "010600000000000000000000"),
    (0.010, "010500000000000000000000"),
    (0.010, "010400000000000000000000"),                                    # 70 -> EP 0x02 blob
    (0.010, "010500000000000000000000"),
    (0.010, "011800000000000000000000"),
    (0.010, "010500000000000000000000"),                                    # 73
]


# Sent on EP 0x02 immediately after frame 70 (0x0104).
EP2_BLOB = "0241271080008000000001F4"
EP2_AFTER = 70

SETS = {
    # VERIFIED MINIMUM -- 3 frames, cold power-up to LED green.
    # Digest, MAC command, transmit token.  Nothing else.
    "bare":  [10, 11, 12],

    # Supersets, each also verified to work; kept for reference and debugging.
    "rb":    list(range(10, 18)),             # + response readback
    "wake":  [8, 9] + list(range(10, 18)),    # + idle/wake tokens
    "core":  [0, 1, 8, 9] + list(range(10, 18)),
    "auth":  list(range(0, 18)),              # + identify and Read(config)
    "min":   list(range(0, 18)) + list(range(67, 74)),   # + reset/arm tail
    "full":  None,                            # everything captured

    # Reset/arm only.  Sets 0x0101 bit 5 but leaves GetEncryptState 0 and the
    # LED red -- proof that bit 5 is a ready flag, not the unlock.
    "tailonly": list(range(67, 74)),
}
SETS["full"] = list(range(len(FRAMES)))

MINIMAL = SETS["bare"]


def unlock(b, indices=None, verbose=True, ep2=True):
    """Replay the arm sequence. Returns True if the board reports unlocked."""
    idx = MINIMAL if indices is None else indices
    for i in idx:
        gap, hx = FRAMES[i]
        b._xfer(usb.EP_CTRL_OUT, bytes.fromhex(hx), timeout_ms=2000)
        time.sleep(gap * 0.6)
        try:
            b.read_status(usb.EP_CTRL_IN, 800)
        except Exception:
            pass
        time.sleep(gap * 0.4)
        if ep2 and i == EP2_AFTER:
            b.write_data(bytes.fromhex(EP2_BLOB))
    return encrypt_state(b, verbose, len(idx)) == 2


def encrypt_state(b, verbose=False, nframes=None):
    """0x0102 GetEncryptState byte 7: 2 = authenticated (LED green), 0 = not."""
    b.write_cmd(cmd(0x0102))
    time.sleep(0.06)
    st = b.read_status(usb.EP_CTRL_IN, 1500)[2]
    if verbose:
        tag = f"  {nframes} frames" if nframes is not None else "  state"
        print(f"{tag}   encstate={st[7]}   0x0102={st.hex(' ')}")
    return st[7]

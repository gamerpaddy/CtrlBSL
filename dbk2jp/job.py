"""
High-level job API for the BSL/SeaCAD DBK2JP board.

Everything here is verified on hardware -- see DBK2JP_PROTOCOL.md.

THE RULE THAT MATTERS: session/control commands go on EP 0x06; everything that
configures a job goes inline in the EP 0x02 batch ahead of the vectors. The
board ACKs parameter commands on EP 0x06 and then silently ignores them.

    from dbk2jp import Job
    with Job() as j:
        j.laser(freq_khz=20, power_pct=50)
        j.pwm_burst(seconds=10)          # continuous PWM for scope work
"""

import atexit
import time

from . import protocol as S
from .usb import Board
from .unlock import unlock, encrypt_state
from . import laser as _laser
from .field import Field
from .laser import CO2, FIBER, UV, GREEN, MOPA, YAG, LASERS, Laser

# Raw type codes, kept for callers that had them hardcoded. Prefer the names.
LASER_CO2, LASER_FIBER, LASER_UV, LASER_GREEN, LASER_MOPA = 0x22, 0x11, 0x33, 0x44, 0x55

SEG_TIME = 0.00083          # measured cost of one lit segment, seconds
MAX_SEGS = 1400             # per EP 0x02 transfer (flush limit is 1920)
CENTRE = 0x8000


class Job:
    """A session against one board.

        with Job(CO2) as j:
            j.configure(freq_khz=20, power_pct=50)
    """

    def __init__(self, laser=CO2, board=None, index=0, unlock_now=True,
                 field=None):
        self.b = board if board is not None else Board(index=index)
        self.field = field if field is not None else Field()
        self._live = False          # has this job programmed a laser output?
        self._closed = False
        self.select(laser)
        # Last-resort net: an interpreter that exits without close() -- an
        # unhandled exception, a bare script with no "with" -- still silences
        # the board. A hard kill cannot be caught, so this is not a substitute
        # for close(), and neither is a substitute for a hardware interlock.
        atexit.register(self._atexit)
        if unlock_now:
            self.ensure_unlocked()

    # ---- laser selection -------------------------------------------------

    def select(self, kind):
        """Pick the laser type: a name (CO2, FIBER, UV, GREEN, MOPA, YAG), a
        Laser, or a raw type code. Loads that type's defaults."""
        self.laser = _laser.get(kind)
        self.laser_type = self.laser.code
        self._freq = self.laser.freq_khz
        self._power = 50                 # percent
        self._power_byte = 0x80
        self._mopa_pulse = None
        self._mo = False                 # MO / PA enable, 0x0211 Param1 bit 8
        self._tickle = self.laser.tickle      # CO2 gets a tickle by default
        self._tick_khz = self.laser.tick_khz
        self._tick_us = self.laser.tick_us
        return self.laser

    def configure(self, freq_khz=None, power_pct=None, power_byte=None,
                  mopa_pulse=None, tickle=None, tick_khz=None, tick_us=None,
                  mo=None):
        """Set the laser parameters for this job.

        power_pct is PWM duty; power_byte is the 8-bit parallel word on P0..P7.
        Pass whichever suits the laser -- the other is derived, so the header is
        always consistent. Nothing reaches the board here: these are emitted
        into each job's EP 0x02 header, because the board ACKs parameter
        commands sent on EP 0x06 and then ignores them.
        """
        if freq_khz is not None:
            lo, hi = self.laser.freq_range
            if not lo <= freq_khz <= hi:
                raise ValueError("%s wants %g..%g kHz, got %g"
                                 % (self.laser.name, lo, hi, freq_khz))
            self._freq = freq_khz
        if power_pct is not None:
            self._power = power_pct
            self._power_byte = (int(power_pct) * 0xFF) // 100
        if power_byte is not None:
            self._power_byte = power_byte & 0xFF
            self._power = round(self._power_byte * 100.0 / 0xFF)
        if mopa_pulse is not None:
            if not self.laser.mopa_pulse:
                raise ValueError("%s has no pulse-width setting" % self.laser.name)
            self._mopa_pulse = mopa_pulse
        if mo is not None:
            self._mo = bool(mo)
        if tick_khz is not None or tick_us is not None:
            self.tick(freq_khz=tick_khz, width_us=tick_us, enable=None)
        if tickle is not None:
            if tickle and not self.laser.tickle:
                raise ValueError("%s has no tickle" % self.laser.name)
            self._tickle = tickle
        return self.settings()

    def settings(self):
        """What would go on the wire, as a dict."""
        period = int(round(S.FPGA_CLK_KHZ / self._freq))
        return {
            "laser": self.laser.name,
            "code": self.laser.code,
            "freq_khz": 48e3 / (period + 1),      # N+1 counter, actual output
            "power_pct": self._power,
            "power_byte": self._power_byte,
            "mopa_pulse": self._mopa_pulse,
            "mo": self._mo,
            "tickle": self._tickle,
            "tick_khz": self._tick_khz if self.laser.tickle else None,
            "tick_us": self._tick_us if self.laser.tickle else None,
            "verified": self.laser.verified,
        }

    # ---- plumbing -------------------------------------------------------
    def _cmd(self, c, t=1200, retries=3):
        """Send on EP 0x06 and return the matching reply (see Board.ask)."""
        return self.b.ask(c, timeout_ms=t, retries=retries)

    def status(self):
        return self._cmd(S.cmd(0x0101))

    def unlocked(self):
        """True once the board is authenticated (加密 LED green).

        Read from 0x0102 GetEncryptState byte 7, NOT 0x0101 bit 5 -- that bit is
        a ready/arm flag that the reset tail sets with the LED still red.
        """
        st = self._cmd(S.cmd(0x0102))
        return bool(st and st[7] == 2)

    # ---- inputs ---------------------------------------------------------

    IN_SHIFT = 8            # input bits start at bit 8 of the status word
    IN_MASK  = 0xFF00       # bits 8..15 are not part of the cache count
    REMARK_BIT = 0x0800     # bit 11: mark-repeat trigger (there is no IN3)

    def status_word(self):
        """The 16-bit word at bytes 5..6 of the 0x0101 reply.

        Low bits are the free-cache count; the high bits carry the opto inputs.
        """
        st = self.status()
        return ((st[5] << 8) | st[6]) if st else None

    def inputs(self):
        """Raw input bits, IN0 in bit 0. A bit reads 1 when the pin is idle.

        IN0 doubles as the X axis origin/home switch on some machine setups.
        """
        w = self.status_word()
        return None if w is None else (w & self.IN_MASK) >> self.IN_SHIFT

    def input_pin(self, n):
        """State of INn (0-based, matching the connector labels).

        True = idle/high, False = driven.
        """
        v = self.inputs()
        return None if v is None else bool(v >> n & 1)

    def remark(self):
        """REMARK trigger input, bit 11. True = idle/high, False = driven."""
        w = self.status_word()
        return None if w is None else bool(w & self.REMARK_BIT)

    # ---- outputs --------------------------------------------------------

    def out(self, port, value):
        """Set output port `port` to `value`. Verified on OUT0 and OUT1.

        OUT2 and OUT3 are the stepper DIR and PULSE pins -- driven by axis_move()
        via 0x0230-0x0233. Do not write them here while an axis move runs.

        0x0111 takes the port index in the HIGH BYTE of Param0 and the level in
        Param1 -- not a bitmask in Param0, which is why writing 0x0001 there
        does nothing. Goes on EP 0x06, the immediate path, not the EP 0x02 batch.
        """
        return self._cmd(S.cmd(0x0111, (port & 0xFF) << 8, value & 0xFFFF))

    def out_pulse(self, port, value, ms):
        """Timed output pulse via 0x2F82 on EP 0x02, duration scaled by 2000.
        UNTESTED."""
        ticks = int(ms) * 2000
        blob = S.cmd(0x2F82,
                     ((port & 0xFF) << 8) | (1 if ms > 0 else 0),
                     ((value & 0xFF) << 8) | ((ticks >> 24) & 0xFF),
                     ((ticks >> 8) & 0xFF) | (((ticks >> 16) & 0xFF) << 8),
                     (int(ms) * -0x3000) & 0xFFFF, 0)
        self.b.write_data(blob)

    def out_state(self):
        """0x0112 GetOutPortState."""
        return self._cmd(S.cmd(0x0112))

    def armed(self):
        """0x0101 bit 5: board reset and online. Independent of the unlock."""
        st = self.status()
        return bool(st and st[2] & 0x20)

    def ensure_unlocked(self):
        self._cmd(S.cmd(0x0105))
        self._cmd(S.cmd(0x0106))
        self._cmd(S.cmd(0x0105))
        if not self.unlocked():
            unlock(self.b, verbose=False)      # 3-frame ATSHA204 replay
        return self.unlocked()

    # ---- configuration --------------------------------------------------
    def mopa_pulse(self, value):
        """MOPA pulse width, 0x0206 Param0=0xA501 Param1=value, on EP 0x02.

        UNTESTED -- no MOPA laser here to measure.
        """
        self._mopa_pulse = value
        self._live = True
        self.b.write_data(S.cmd(0x0206, 0xA501, value & 0xFFFF, 0, 0, 0))
        return value

    def tick(self, freq_khz=None, width_us=None, enable=True):
        """Set the tickle shape. Frequency and width are independent settings.

        The tickle is its own generator, unrelated to the marking PWM: 0x0217
        Param0 = flags<<8 (bit0 ENPWMTICK, bit1 ENCO2FPK), Param1 = period in
        48 MHz ticks, Param2 = width in ticks.

        Either argument may be omitted to keep the current value. `enable=None`
        leaves the on/off state alone, which is how configure() calls it.

        Returns (actual_freq_hz, width_ticks, duty_pct). The period is an N+1
        counter, so the frequency lands on the nearest achievable value.
        """
        if not self.laser.tickle:
            raise ValueError("%s has no tickle" % self.laser.name)

        f = self._tick_khz if freq_khz is None else freq_khz
        w = self._tick_us if width_us is None else width_us

        lo, hi = self.laser.tick_range
        if not lo <= f <= hi:
            raise ValueError("tickle frequency %g kHz outside %g..%g kHz"
                             % (f, lo, hi))
        period_us = 1000.0 / f
        if not 0 < w < period_us:
            raise ValueError(
                "tickle width %g us must be >0 and shorter than the %.1f us "
                "period at %g kHz" % (w, period_us, f))

        self._tick_khz, self._tick_us = f, w
        if enable is not None:
            self._tickle = enable

        period = int(round(S.FPGA_CLK_KHZ / f))
        ticks = int(round(w * 48))
        return 48e6 / (period + 1), ticks, 100.0 * w / period_us

    def _header(self):
        if self.laser.power == "byte":
            pwr = S.set_power_raw(self._freq, self._power_byte,
                                  duty_pct=self._power)
        else:
            pwr = S.set_power_0210(self._freq, self._power)[0]
        tk = getattr(self, "_tick_khz", self._freq)
        tus = getattr(self, "_tick_us", 1.0)
        tperiod = int(round(48000.0 / tk))
        twidth = int(round(tus * 48))
        h = S.cmd(0x0211, (self.laser_type << 8),
                  S.MO_ENABLE if self._mo else 0, 0, 0, 0)
        h += pwr
        if self._mopa_pulse is not None:
            h += S.cmd(0x0206, 0xA501, self._mopa_pulse & 0xFFFF, 0, 0, 0)
        h += S.cmd(0x0217, 0x0100 if self._tickle else 0x0000,
                   tperiod & 0xFFFF, twidth & 0xFFFF, 0, 0)
        h += S.cmd(0x0208, 0, 0, 0, 0, 0)
        self._live = True          # this header programs a laser output
        return h

    # ---- execution ------------------------------------------------------
    def begin(self, start=(0x4000, CENTRE), speed=200):
        """Reset, arm, and push the parameter header + opening jump."""
        self._cmd(S.cmd(0x0106))
        self._cmd(S.cmd(0x0105))
        self._cmd(S.cmd(0x0104))
        blob = self._header() + S.cmd(0x0241, speed, start[0], start[1], 0, 500)
        self._live = True
        self.b.write_data(blob)

    def lines(self, points, speed=200):
        """Emit lit (laser-on) vectors through `points`, paced to the board."""
        for i in range(0, len(points), MAX_SEGS):
            chunk = points[i:i + MAX_SEGS]
            blob = b"".join(S.cmd(0x0243, speed, x, y, 0, 500) for x, y in chunk)
            self.b.write_data(blob)
            time.sleep(len(chunk) * SEG_TIME * 0.92)

    def jump(self, x, y, speed=0x2710, delay=0x01F4):
        """Unlit move to (x, y). 0x8000 is centre, full span 0x0000..0xFFFF.

        Verified on both galvo pins: alternating 0x4000/0xC000 at 1 Hz on X
        or Y moves the corresponding mirror. Goes on EP 0x02 like every other geometry command.
        """
        self.b.write_data(S.cmd(0x0241, speed, x & 0xFFFF, y & 0xFFFF, 0, delay))

    # ---- millimetres -----------------------------------------------------
    #
    # The board only understands 16-bit galvo counts. These convert through
    # self.field, which carries the machine's field size, offsets and optical
    # correction. See field.py.

    def mm(self, x_mm, y_mm, clamp=False):
        """(x_mm, y_mm) -> (x_counts, y_counts) for this machine's field."""
        return self.field.to_counts(x_mm, y_mm, clamp=clamp)

    def where_mm(self, x_counts, y_counts):
        """Counts back to millimetres."""
        return self.field.to_mm(x_counts, y_counts)

    def jump_mm(self, x_mm, y_mm, speed=0x2710, delay=0x01F4, clamp=False):
        """Unlit move to a point in millimetres."""
        x, y = self.mm(x_mm, y_mm, clamp)
        return self.jump(x, y, speed=speed, delay=delay)

    def begin_mm(self, start=(0.0, 0.0), speed=200, clamp=False):
        """begin() with the start point in millimetres."""
        return self.begin(start=self.mm(start[0], start[1], clamp), speed=speed)

    def lines_mm(self, points_mm, speed=200, clamp=False):
        """lines() with every point in millimetres."""
        return self.lines([self.mm(x, y, clamp) for x, y in points_mm],
                          speed=speed)

    def free_cache(self):
        """Free queue slots, from bytes 5-6 of the 0x0101 reply.

        Bits 8..15 of that word are the opto inputs, not part of the count, so
        they are masked off here. Reading the raw 16-bit word (as this did
        before) makes the result jump by 256 per input whenever one changes.
        The count is the low byte only -- a 256-slot queue.
        """
        st = self.status()
        return (((st[5] << 8) | st[6]) & ~self.IN_MASK & 0xFFFF) if st else 0

    RUNNING_BIT = 0x08       # byte 2: set by 0x0104 Run, cleared by 0x0105

    def running(self):
        """True once the marking engine has been started (0x0104).

        This is NOT "still marking" -- it stays set until a reset. No
        queue-drained or job-complete indicator has been found: free_cache()
        reads idle even while vectors are executing, so it cannot be polled for
        completion either. Time your own waits.
        """
        st = self.status()
        return None if st is None else bool(st[2] & self.RUNNING_BIT)

    def pwm_burst(self, seconds=10, speed=200, span=(0x4000, 0xC000), margin=48):
        """Sustained laser PWM for scope work.

        Closed-loop paced off the board's own free-cache counter rather than a
        fixed sleep: open-loop pacing drains the queue between chunks and the
        output visibly drops back to tickle-only about once a second. Keeping the
        queue topped up (never closer than `margin` slots to full) gives
        continuous output without the overrun that silently clears the unlock bit.

        `margin` was 512 back when free_cache() returned the raw 16-bit word
        (~4029 idle). Now that the input bits are masked off the counter reads
        ~189 idle out of 256, so the margin scales down with it.
        """
        self.begin(start=(span[0], CENTRE), speed=speed)
        t0 = time.time()
        n = 0
        while time.time() - t0 < seconds:
            free = self.free_cache()
            want = min(MAX_SEGS, max(0, free - margin))
            if want < 64:                      # queue is full enough; let it drain
                time.sleep(0.01)
                continue
            pts = [((span[1] if i % 2 == 0 else span[0]), CENTRE) for i in range(want)]
            blob = b"".join(S.cmd(0x0243, speed, x, y, 0, 500) for x, y in pts)
            try:
                self.b.write_data(blob)
                n += want
            except Exception as e:
                print("stalled:", repr(e))
                break
        return n

    def power_byte(self, value, freq_khz=None):
        """Fiber parallel power word P0..P7 (0x0210 Param3 low byte).

        WARNING: this puts a LIVE SIGNAL on the laser control output and leaves
        it there. It is a power level, not a one-shot, so it persists until
        laser_off(), close(), or a power cycle. On a machine whose laser
        control pin is PWM, the level appears there as a continuous modulated
        signal at the configured frequency.

        Static and latched -- no marking run needed -- and the board strobes
        PLATCH on every change so the laser clocks the new word.

        Use this rather than laser(power_pct=...) for bit-level work: the
        percentage path quantises as (pct*255)//100, so values like 0x80 are
        unreachable through it.
        """
        f = freq_khz if freq_khz is not None else self._freq
        self._cmd(S.cmd(0x0106))
        self._cmd(S.cmd(0x0105))
        self._cmd(S.cmd(0x0104))
        blob = S.cmd(0x0211, (self.laser_type << 8), 0, 0, 0, 0)
        blob += S.set_power_raw(f, value & 0xFF)
        blob += S.cmd(0x0208, 0, 0, 0, 0, 0)
        self._live = True
        self.b.write_data(blob)
        return value & 0xFF

    def dac(self, value12, mark=False):
        """Analog power out, 0x0207 Param0 = 12-bit word (CON3 pin 15 / DA1).

        NOT CONFIRMED WORKING -- produced no voltage on this board under every
        condition tried (idle, while marking, and with the word placed in each
        of Param0..Param4). Kept because the encoding is correct per
        the gate is believed to be a board-side
        analog enable that no observed command writes. See DBK2JP_PROTOCOL.md.
        """
        self._cmd(S.cmd(0x0106))
        self._cmd(S.cmd(0x0105))
        self._cmd(S.cmd(0x0104))
        blob = S.cmd(0x0211, (self.laser_type << 8), 0, 0, 0, 0)
        blob += S.set_power_raw(self._freq, 0x80)
        blob += S.cmd(0x0207, value12 & 0x0FFF, 0, 0, 0, 0)
        blob += S.cmd(0x0208, 0, 0, 0, 0, 0)
        if mark:
            blob += S.cmd(0x0241, 200, 0x4000, CENTRE, 0, 500)
        self._live = True
        self.b.write_data(blob)
        return value12 & 0x0FFF

    def mo(self, on=True):
        """Master oscillator and power amplifier enable (CON3 pins 18 and 19).

        This is 0x0211 Param1 bit 8, not the 0x0281 / 0x0280 command pair. That
        pair has no observable effect on either pin, and neither does running
        the engine: a 6 s mark, 6 s idle, 6 s mark run with the bit clear left
        MO and PA low the whole time. With the bit set they both come up as the
        job starts and drop when it ends.

        MO and PA are amplifier enables on the laser side, so this is off by
        default and has to be asked for. laser_off() clears it.

        Takes effect on the next job header. Call it before begin().
        """
        self._mo = bool(on)
        return self._mo

    # 0x0230 Param4 flag bits
    AX_REVROT    = 0x100     # +0x06 REVROT  -> DIR pin (verified on scope)
    AX_FLAG_200  = 0x200     # field@0x82 != 1
    AX_ZEROTYPE  = 0x004     # +0x34 nZeroType
    AX_MOMODE    = 0x001     # +0x48 nMoMode
    AX_FLAG_008  = 0x008     # field@0xb0
    AX_FLAG_002  = 0x002     # field@0xc4

    def axis_move(self, pulses, pps, direction=0, flags=0,
                  min_pps=None, acctime=100, p232=175):
        """Stepper axis move -> PULSE / DIR pins.

        Uses the newer FPGA path (0x0230/0x0232/0x0231/0x0233); the legacy
        0x2D80/0x2D81 pair is dead on this board. 0x0233 with all-zero params is
        the execute trigger, and each trigger runs ONE finite move.

            0x0230  Param0/1 = 32-bit pulse count, hi/lo
                    Param4   = flag word; bit 0x100 (REVROT) drives DIR
            0x0231  Param0/1 = speed pair; effective rate = max(Param0, Param1)
            0x0233  GO

        Speed is pulses-per-second directly (2000 -> 2 kHz, measured), and the
        move self-terminates after `pulses`, so duration = pulses/pps.

        DIRECTION IS 0x0230 Param4 BIT 0x100, NOT 0x0232. Driving 0x0232 Param0
        between 0 and 1 changes nothing on DIR -- verified.

        ACCELERATION: 0x0231 Param2's HIGH byte is the ramp time. Taken from a
        LightBurn rotary jog capture, which sent Param2 = 0x6400 -- high byte
        100, matching AXISACCTIME=100 in markcfg0. Verified: ACCTIME 255 gives a
        long visible ramp, 20 a much shorter one. Leave it non-zero or the move
        starts and stops abruptly.

        0x0232 Param0 = 175 in every captured jog; purpose unknown but sent for
        fidelity with the known-good sequence.

        Let the move finish rather than cutting it short with a reset, otherwise
        the deceleration ramp never happens.
        """
        self._cmd(S.cmd(0x0106))
        self._cmd(S.cmd(0x0105))
        self._cmd(S.cmd(0x0104))
        head = S.cmd(0x0211, (self.laser_type << 8), 0, 0, 0, 0)
        head += S.set_power_raw(self._freq, 0x80)
        head += S.cmd(0x0208, 0, 0, 0, 0, 0)
        self.b.write_data(head)
        mn = min_pps if min_pps is not None else pps
        p4 = (flags | (self.AX_REVROT if direction else 0)) & 0xFFFF
        blob = S.cmd(0x0230, (pulses >> 16) & 0xFFFF, pulses & 0xFFFF, 0, 0, p4)
        blob += S.cmd(0x0231, mn & 0xFFFF, pps & 0xFFFF,
                      (acctime & 0xFF) << 8, 0, 0)
        blob += S.cmd(0x0232, p232 & 0xFFFF, 0, 0, 0, 0)
        blob += S.cmd(0x0233, 0, 0, 0, 0, 0)
        self.b.write_data(blob)
        return pulses / float(pps) if pps else 0.0      # expected duration, seconds

    def laser_port_switch(self, p1=0, p2=0, p3=0, p4=0, p5=0, p6=0):
        """0x2F84 laser port switch:
            Param0 = p1*0x100 + p4
            Param1 = p5*0x300
            Param2 = p3*2
            Param3 = p6*2
        Purpose not established; a candidate for routing/enabling analog out."""
        self._cmd(S.cmd(0x0106))
        self._cmd(S.cmd(0x0105))
        self._cmd(S.cmd(0x0104))
        blob = S.cmd(0x2F84, (p1 * 0x100 + p4) & 0xFFFF, (p5 * 0x300) & 0xFFFF,
                     (p3 * 2) & 0xFFFF, (p6 * 2) & 0xFFFF, 0)
        self.b.write_data(blob)

    def red_light(self, on=True):
        """Pilot / red pointer (CON3 pin 22).

        Not a GPIO: 0x0112 never changes. The pointer is driven by the marking
        engine's red-light mode, selected by the LOW byte of 0x0211 Param0 --
        0x22 = red light, 0x00 = normal marking. The high byte stays the laser
        type. So for CO2, Param0 = 0x2222 on, 0x2200 off.

        This is just
        SendLenPara(lmc, p2, p3) + flush, with p3 as the red-light flag.

        It latches: once the header is sent the pointer stays on with nothing
        streaming, so this is a static on/off, not a per-job mode. Like every
        other parameter it must travel the EP 0x02 batch path.
        """
        self._cmd(S.cmd(0x0106))
        self._cmd(S.cmd(0x0105))
        self._cmd(S.cmd(0x0104))
        p0 = (self.laser_type << 8) | (0x22 if on else 0x00)
        pwr, _, _ = S.set_power_0210(self._freq, self._power)
        blob = S.cmd(0x0211, p0, 0, 0, 0, 0) + pwr
        blob += S.cmd(0x0217, 0x0100 if self._tickle else 0x0000,
                      int(round(48000.0 / getattr(self, "_tick_khz", 5.0))) & 0xFFFF,
                      int(round(getattr(self, "_tick_us", 1.0) * 48)) & 0xFFFF, 0, 0)
        blob += S.cmd(0x0208, 0, 0, 0, 0, 0)
        self.b.write_data(blob)
        return on

    def tick_off(self):
        """Stop the tickle generator.

        It is free-running: it keeps pulsing after a job ends and after the host
        process exits, so it must be switched off explicitly. 0x0217 with
        Param0 = 0 (ENPWMTICK clear) has to travel the EP 0x02 batch path like
        any other parameter -- sending it on EP 0x06 is ACKed and ignored.
        """
        self._tickle = False
        self._cmd(S.cmd(0x0106))
        self._cmd(S.cmd(0x0105))
        self._cmd(S.cmd(0x0104))
        tk = getattr(self, "_tick_khz", 5.0)
        tus = getattr(self, "_tick_us", 1.0)
        blob = S.cmd(0x0217, 0x0000, int(round(48000.0 / tk)) & 0xFFFF,
                     int(round(tus * 48)) & 0xFFFF, 0, 0)
        blob += S.cmd(0x0241, 200, 0x4000, CENTRE, 0, 500)
        self.b.write_data(blob)
        time.sleep(0.05)
        self._cmd(S.cmd(0x0105))

    def stop(self):
        self._cmd(S.cmd(0x0105))

    # ---- laser status / safety ------------------------------------------

    SGIN_BIT = 0x02          # byte 2 of the 0x0101 reply

    def sgin(self):
        """Laser status input (SGIN). True = OK, False = fault asserted.

        SGIN0, SGIN1 and SGIN2 all drive the SAME bit -- byte 2 bit 1 -- so the
        board reports only "some SGIN is asserted", not which. SGIN3 does not
        appear in the status reply at all. Verified by moving a 500 ms square
        wave to each pin in turn.

        Which physical fault each SGIN carries (overheat, back-reflection,
        ready, ...) depends on the laser model; the board does not distinguish
        them, so treat any assertion as a stop condition.
        """
        st = self.status()
        return None if st is None else bool(st[2] & self.SGIN_BIT)

    def abort(self):
        """Stop marking and kill laser output now.

        Order matters: cut the laser gate first, then drop the queued vectors,
        then reset. Returns once the board has acknowledged the reset.
        """
        try:
            self._cmd(S.cmd(0x0208))          # laser off / gate closed
        except Exception:
            pass
        for op in (S.CMD_CLEAR_CACHE, S.CMD_RESET, S.CMD_RUN, S.CMD_RESET):
            try:
                self._cmd(S.cmd(op))
            except Exception:
                pass
        self._tickle = False

    def guard(self, seconds, poll=0.005, on_fault=None):
        """Poll SGIN for `seconds`, calling abort() the moment it asserts.

        Returns True if it ran clean, False if a fault stopped it.

        NOT a substitute for a hardware interlock. This is a USB poll: each
        round trip costs ~4-8 ms, so worst-case reaction is tens of ms, and it
        stops if the host stalls. Real E-stop belongs in hardware.
        """
        t0 = time.time()
        while time.time() - t0 < seconds:
            if self.sgin() is False:
                self.abort()
                if on_fault:
                    on_fault(time.time() - t0)
                return False
            time.sleep(poll)
        return True

    def _atexit(self):
        if self._closed or not self._live:
            return
        try:
            self.laser_off()
        except Exception:
            pass

    def laser_off(self):
        """Silence every laser output: marking PWM, tickle, gate.

        A plain reset (0x0105) does NOT stop the marking PWM generator: once
        0x0210 has programmed a period and width and the engine has been
        started, the pin keeps modulating. The generator has to be zeroed
        through the EP 0x02 header like any other parameter, and only then
        reset. Order matters: arming first would restart the engine with the
        old values still loaded and emit a burst on the way down.
        """
        blob = S.cmd(0x0211, (self.laser_type << 8), 0, 0, 0, 0)  # MO / PA off
        blob += S.cmd(S.CMD_POWER, 0, 0, 0, 0, 0)      # period, width, power = 0
        blob += S.cmd(S.CMD_TICK, 0x0000, 0, 0, 0, 0)  # tickle disabled
        blob += S.cmd(S.CMD_LASER_GATE, 0, 0, 0, 0, 0)
        try:
            # Zero the generator FIRST, with the engine in whatever state it is
            # already in. Sending 0x0104 to "arm" before this restarts the
            # engine with the PWM values still loaded and puts a burst on the
            # pin, which is exactly what this method exists to prevent.
            self.b.write_data(blob)
            time.sleep(0.05)
        except Exception:
            pass
        finally:
            self._tickle = False
            self._mo = False
            self._live = False
            try:
                self._cmd(S.cmd(S.CMD_CLEAR_CACHE))
                self._cmd(S.cmd(S.CMD_RESET))
            except Exception:
                pass

    def close(self, quiet=True):
        """Leave the board silent: every laser output off, then reset.

        Any job that programmed a laser output gets silenced, whether or not it
        used the tickle. An earlier version only switched the tickle off, and
        only when that job had turned it on, so the marking PWM was left running
        on the laser control pin after a job that had set a power level.

        A session that never programmed an output writes nothing, so reading
        status does not disturb a mark already running from elsewhere. Pass
        quiet=False to skip the shutdown entirely.
        """
        try:
            if quiet and self._live:
                self.laser_off()
            else:
                self.stop()
        finally:
            self._closed = True
            self.b.close()

    def __enter__(self):
        return self

    def __exit__(self, *a):
        self.close()

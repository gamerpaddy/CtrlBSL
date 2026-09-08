"""
High-level job API for the BSL/SeaCAD DBK2JP board.

Verified on hardware unless a docstring says otherwise -- see
DBK2JP_PROTOCOL.md for what was measured and how.

THE RULE THAT MATTERS: session/control commands go on EP 0x06; everything that
configures a job goes inline in the EP 0x02 batch ahead of the vectors. The
board ACKs parameter commands on EP 0x06 and then silently ignores them.

    from dbk2jp import Job
    with Job() as j:
        j.configure(freq_khz=20, power_pct=50)
        j.pwm_burst(seconds=10)          # continuous PWM for scope work
"""

import atexit
import math
import time
import warnings

from . import protocol as S
from .usb import Board
from .unlock import unlock, encrypt_state
from . import laser as _laser
from .field import Field
from .laser import CO2, FIBER, UV, GREEN, MOPA, YAG, LASERS, Laser

# Raw type codes, kept for callers that had them hardcoded. Prefer the names.
LASER_CO2, LASER_FIBER, LASER_UV, LASER_GREEN, LASER_MOPA = 0x22, 0x11, 0x33, 0x44, 0x55

SEG_TIME = 0.00083          # conservative per-segment cost, seconds
SEG_MIN = 0.00003           # measured floor: ~32 800 segments/s with pacing
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
        # Machine kinematics, if you know them. Nothing is assumed: with these
        # unset the geometry checks are purely geometric, and a wiggle can ask
        # for motion no galvo will produce. See set_limits() and wiggle_load().
        self.limits = {"max_mm_s": None, "max_accel_mm_s2": None,
                       "max_loop_hz": None}
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
            # Gated on the power style, not the type name: a MOPA source has
            # to be driven under the fiber code here, since 0x55 mutes every
            # output, so fiber must accept a pulse width too.
            if self.laser.power != "byte":
                raise ValueError("%s has no pulse-width setting: it is not a "
                                 "parallel-power laser" % self.laser.name)
            self._mopa_pulse = int(mopa_pulse)
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

    READY_BIT = 0x20         # byte 2: set when reset and online, clear while busy

    def armed(self):
        """0x0101 bit 5: board reset and online. Independent of the unlock.

        The same bit clears while the vector queue is executing, so this reads
        False mid-mark on a board that is perfectly healthy. See busy().
        """
        st = self.status()
        return bool(st and st[2] & self.READY_BIT)

    def busy(self):
        """True while the vector queue is still executing. None if unreadable.

        Both vendor hosts clear 0x0101 byte 2 bit 5 for exactly the duration of
        a mark and set it again when the queue drains, over jobs from 34 ms to
        2.5 s, with no host action in between. That makes it the job-complete
        indicator this library otherwise lacks. Read from captures rather than
        measured here, so treat a surprising answer as the flag being wrong
        rather than the board being stuck, and keep timing your own waits as a
        fallback.
        """
        st = self.status()
        return None if st is None else not (st[2] & self.READY_BIT)

    def wait_idle(self, timeout=30.0, poll=0.02):
        """Block until busy() goes False. -> True if it did, False on timeout.

        Never raises and never assumes: on a board that keeps the bit clear this
        just times out, so callers that cannot tolerate that should keep using a
        computed duration.
        """
        t0 = time.time()
        while time.time() - t0 < timeout:
            if self.busy() is False:
                return True
            time.sleep(poll)
        return False

    def ensure_unlocked(self, attempts=2):
        """Reset, then replay the unlock until the board reports authenticated.

        The ATSHA204 needs 40-120 ms per command and answers from stale result
        registers when it is rushed, so a single replay can lose the race on a
        loaded machine. The latch is sticky and a repeat costs nothing, so this
        tries again rather than leaving a locked board that marks nothing.
        """
        self._cmd(S.cmd(0x0105))
        self._cmd(S.cmd(0x0106))
        self._cmd(S.cmd(0x0105))
        for i in range(max(1, attempts)):
            if self.unlocked():
                return True
            unlock(self.b, verbose=False)      # 3-frame ATSHA204 replay
        if self.unlocked():
            return True
        warnings.warn(
            "board still reports locked after %d unlock attempts: it will "
            "accept commands and emit nothing." % max(1, attempts), stacklevel=2)
        return False

    # ---- configuration --------------------------------------------------
    def _check_spi_clash(self):
        """Warn if the power word is sitting on the MOPA SPI lines.

        The pulse width goes out as a four byte SPI frame on P1 (data) and P2
        (clock), and those are also bits 1 and 2 of the parallel power word. A
        power byte with either bit set holds them high after the frame, so the
        clock never returns to idle and the next frame's first byte is
        mangled. Verified on the wire: 0x7F corrupts every frame after the
        first, 0x00 gives three clean ones.
        """
        if self._mopa_pulse is not None and (self._power_byte & S.MOPA_SPI_MASK):
            warnings.warn(
                "power byte 0x%02X has bit 1 or 2 set, and those are the MOPA "
                "SPI data and clock lines. Pulse width frames after the first "
                "will be corrupted. Use a power byte with 0x06 clear."
                % self._power_byte, stacklevel=3)

    def mopa_pulse(self, ns):
        """MOPA pulse width in NANOSECONDS.

        Emitted as 0x0206, which carries a four byte SPI frame rather than a
        plain parameter: 0xA5 0x01 then the width big-endian, clocked out on
        P1 (data) and P2 (clock). 100 ns goes out as A5 01 00 64. Confirmed on
        the wire at 100, 150 and 200 ns.

        The optical result is unverified, since there is no MOPA source here,
        but the frame on the wire is what the laser expects.

        The power byte must have bits 1 and 2 clear or consecutive frames
        corrupt. See _check_spi_clash.
        """
        if self.laser.power != "byte":
            raise ValueError("%s has no pulse-width setting: it is not a "
                             "parallel-power laser" % self.laser.name)
        self._mopa_pulse = int(ns)
        self._live = True
        self._check_spi_clash()
        # The frame is only shifted out as part of an armed job header. Sent
        # on its own it produces nothing at all on P1 and P2, so arm and send
        # the header the same way power_byte() does.
        self._cmd(S.cmd(0x0106))
        self._cmd(S.cmd(0x0105))
        self._cmd(S.cmd(0x0104))
        self.b.write_data(self._header())
        return self._mopa_pulse

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
            self._check_spi_clash()
            h += S.mopa_pulse_ns(self._mopa_pulse)
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

    def lines(self, points, speed=200, delay=500):
        """Emit lit (laser-on) vectors through `points`, paced to the board.

        `speed` is the per-segment duration in microseconds (see jump()), so the
        pace has to follow it: a slow mark executes far longer than the host
        write takes, and pacing on a fixed per-segment cost alone overruns the
        queue and leaves the board in the stuck 0x0e state. Sleep on whichever
        is larger, the commanded duration or the measured host cost.

        `delay` is Param4, a dwell in microseconds at each point. It defaults to
        500 for compatibility with everything that was measured through this
        method, but a run of vectors wants 0 at the interior points: the vendor
        hosts spend it only where a lit run ends. path() does that placement
        itself.
        """
        per_seg = max(SEG_TIME, speed * 1e-6)
        for i in range(0, len(points), MAX_SEGS):
            chunk = points[i:i + MAX_SEGS]
            blob = b"".join(S.cmd(0x0243, speed, x, y, 0, delay)
                            for x, y in chunk)
            self.b.write_data(blob)
            time.sleep(len(chunk) * per_seg * 0.92)

    # ---- position streaming ---------------------------------------------
    #
    # One write per batch instead of one per call, and the laser state carried
    # per segment rather than per method. Geometry only: no shapes, no fills,
    # just cheaper ways to hand the board a run of points.

    FULL = 0xFFFF               # galvo travel limit, both axes

    def _len_mm(self, p0, p1):
        """Straight-line length of a counts-space move, in millimetres.

        Goes through the field, so aspect, mirror and swap are accounted for.
        That is what makes a feed rate mean the same thing on both axes.
        """
        ax, ay = self.field.to_mm(*p0)
        bx, by = self.field.to_mm(*p1)
        return ((bx - ax) ** 2 + (by - ay) ** 2) ** 0.5

    def _duration(self, p0, p1, speed=None, mm_s=None):
        """Param0 for the move p0->p1: microseconds, floor of 1.

        Give either `speed` (raw Param0, the duration itself) or `mm_s` (a feed
        rate, converted per segment through the field). A feed rate is the
        honest way to paint unequal segments evenly, since Param0 is a duration
        and one fixed value across mixed lengths gives mixed speeds.
        """
        if (speed is None) == (mm_s is None):
            raise ValueError("give exactly one of speed= (microseconds) or "
                             "mm_s= (millimetres per second)")
        if speed is not None:
            return max(1, int(speed))
        if mm_s <= 0:
            raise ValueError("mm_s must be positive")
        return max(1, int(round(self._len_mm(p0, p1) / mm_s * 1e6)))

    def _in_field(self, pt):
        return 0 <= pt[0] <= self.FULL and 0 <= pt[1] <= self.FULL

    def _require_in_field(self, pts, what="point"):
        for x, y in pts:
            if not self._in_field((x, y)):
                raise ValueError(
                    "%s (%d, %d) is outside the galvo travel limits 0..%d"
                    % (what, x, y, self.FULL))

    def _lead(self, p0, p1, counts, at_start):
        """A point `counts` beyond p0 (or past p1) along the p0->p1 line."""
        dx, dy = p1[0] - p0[0], p1[1] - p0[1]
        d = (dx * dx + dy * dy) ** 0.5
        if d == 0 or counts <= 0:
            return p0 if at_start else p1
        ux, uy = dx / d, dy / d
        if at_start:
            return (int(round(p0[0] - ux * counts)),
                    int(round(p0[1] - uy * counts)))
        return (int(round(p1[0] + ux * counts)),
                int(round(p1[1] + uy * counts)))

    def _fit_overshoot(self, p0, p1, counts, at_start):
        """Longest run-up up to `counts` that stays inside the travel limits.

        Shrinking beats refusing: a shorter run-up still marks the requested
        geometry, while a refusal turns a reachable job into no job. Returns
        (point, granted_counts).
        """
        lo, hi = 0, int(counts)
        best = ((p0 if at_start else p1), 0)
        while lo <= hi:
            mid = (lo + hi) // 2
            pt = self._lead(p0, p1, mid, at_start)
            if self._in_field(pt):
                best = (pt, mid)
                lo = mid + 1
            else:
                hi = mid - 1
        return best

    def _wiggle(self, p0, p1, radius, pitch, steps):
        """p0 -> p1 traced as small circles, to widen the burn.

        The mark walks the straight line while orbiting it, so the lit area is
        the line plus a circle of `radius` swept along it: a kerf about
        2 * radius wide. `pitch` is how far along the line one full circle
        advances, `steps` how many points make up each circle.

        The exact endpoints are kept at both ends, so joints stay where the
        caller put them and only the middle is widened. Returns the points
        after p0, p1 included, as floats so the resampling below stays exact;
        path() rounds them to counts before timing and emitting.
        """
        dx, dy = p1[0] - p0[0], p1[1] - p0[1]
        length = (dx * dx + dy * dy) ** 0.5
        if length == 0 or radius <= 0:
            return [p1]
        ux, uy = dx / length, dy / length
        turns = max(1.0, length / float(pitch))
        n = max(int(round(turns * steps)), steps)

        def at(f):
            a = 2.0 * math.pi * turns * f
            return (p0[0] + ux * length * f + radius * math.cos(a),
                    p0[1] + uy * length * f + radius * math.sin(a))

        # Sampling the loop at even parameter steps bunches the points where the
        # curve doubles back on itself: with a radius near the pitch, one step
        # covers a couple of counts and the next covers fifty, so the exposure
        # is uneven and the short chords quantise badly. Resample by arc length
        # instead, which is what makes every sub-segment the same length and
        # therefore the same duration.
        dense = max(n * 8, 512)
        pts = [at(i / float(dense)) for i in range(dense + 1)]
        acc, cum = 0.0, [0.0]
        for a, b in zip(pts, pts[1:]):
            acc += math.hypot(b[0] - a[0], b[1] - a[1])
            cum.append(acc)
        total = acc or 1.0

        out, j = [], 0
        for i in range(1, n + 1):
            want = total * i / float(n)
            while j < len(cum) - 2 and cum[j + 1] < want:
                j += 1
            span = cum[j + 1] - cum[j]
            t = 0.0 if span <= 0 else (want - cum[j]) / span
            ax, ay = pts[j]
            bx, by = pts[j + 1]
            out.append((ax + (bx - ax) * t, ay + (by - ay) * t))
        out[-1] = (float(p1[0]), float(p1[1]))     # land exactly on the end
        return out                                 # floats: see path()

    def set_limits(self, max_mm_s=None, max_accel_mm_s2=None,
                   max_loop_hz=None):
        """State what the machine can actually do, for the wiggle checks.

        Nothing here is guessed. Until you measure your own galvos these stay
        unset and the streaming methods only check geometry, which is exactly
        as far as this library can honestly go: the board takes a duration per
        vector and says nothing about whether the mirrors kept up.

        `max_accel_mm_s2` is the lateral acceleration the mirrors will follow at
        the working distance, `max_loop_hz` the rate at which a small circle
        still comes out round rather than smoothed into an oval, and `max_mm_s`
        the marking speed ceiling. Measure the first two by cutting test loops
        and looking at where the corners start rounding off.
        """
        for k, v in (("max_mm_s", max_mm_s),
                     ("max_accel_mm_s2", max_accel_mm_s2),
                     ("max_loop_hz", max_loop_hz)):
            if v is not None:
                self.limits[k] = float(v)
        return dict(self.limits)

    def wiggle_load(self, radius_mm, pitch_mm, mm_s, steps=16):
        """What a wiggle asks of the mirrors, before you cut anything.

        Returns loop rate, the lateral acceleration the circles demand, the
        exposure multiplier, the vector rate the board has to consume, and a
        list of whatever exceeds the limits set on this Job.

        The acceleration is the honest number that makes or breaks a wiggle: a
        circle of radius r walked at v needs v^2 / r sideways, all the time.
        0.1 mm at 600 mm/s is 3.6e6 mm/s^2, some 367 g, which no galvo follows.
        What comes out instead is a smoothed, smaller loop with the dwell piling
        up wherever the servo reverses, so the exposure bunches at the turns
        rather than spreading along the cut. Slower feed or a larger radius are
        the two ways out, and both cost throughput.
        """
        r = float(radius_mm)
        pitch = float(pitch_mm)
        v = float(mm_s)
        if r <= 0 or pitch <= 0 or v <= 0:
            raise ValueError("radius, pitch and mm_s must all be positive")
        traced_per_turn = ((2.0 * math.pi * r) ** 2 + pitch ** 2) ** 0.5
        out = {
            "loop_hz": v / pitch,
            "accel_mm_s2": v * v / r,
            "accel_g": v * v / r / 9810.0,
            "exposure": traced_per_turn / pitch,
            "chord_mm": traced_per_turn / max(4, int(steps)),
            "vectors_per_s": v * (traced_per_turn / pitch) /
                             (traced_per_turn / max(4, int(steps))),
        }
        over = []
        lim = self.limits
        if lim.get("max_mm_s") and v > lim["max_mm_s"]:
            over.append("feed %g mm/s over the %g limit" % (v, lim["max_mm_s"]))
        if lim.get("max_accel_mm_s2") and out["accel_mm_s2"] > lim["max_accel_mm_s2"]:
            over.append("needs %.3g mm/s^2 lateral, limit %.3g"
                        % (out["accel_mm_s2"], lim["max_accel_mm_s2"]))
        if lim.get("max_loop_hz") and out["loop_hz"] > lim["max_loop_hz"]:
            over.append("%.0f loops/s over the %.0f limit"
                        % (out["loop_hz"], lim["max_loop_hz"]))
        if out["vectors_per_s"] > 1.0 / SEG_MIN:
            over.append("%.0f vectors/s over the board's measured %.0f"
                        % (out["vectors_per_s"], 1.0 / SEG_MIN))
        out["exceeded"] = over
        return out

    def runup_mm(self, mm_s, accel_mm_s2=None):
        """Run-up length that actually reaches `mm_s`, as v^2 / 2a.

        Uses `max_accel_mm_s2` from set_limits() unless you pass one. The
        overshoot arguments elsewhere take whatever number you give them and
        make no claim that the mirrors are up to speed by the end of it; this is
        how to pick that number once you know the machine.
        """
        a = accel_mm_s2 or self.limits.get("max_accel_mm_s2")
        if not a:
            raise ValueError("no acceleration known: pass accel_mm_s2 or call "
                             "set_limits(max_accel_mm_s2=...)")
        return float(mm_s) ** 2 / (2.0 * float(a))

    def _emit(self, cmds):
        """Write batched commands, chunked and paced against their durations.

        The floor here is the measured host throughput, ~30 us per command, not
        the conservative 0.83 ms `lines()` uses. These callers know exactly how
        long the board will take because they built every Param0, so the sum is
        the real pace; holding a wiggle of eight thousand short vectors at
        0.83 ms each would sleep about twenty-five times longer than the job
        runs.
        """
        total = 0
        for i in range(0, len(cmds), MAX_SEGS):
            chunk = cmds[i:i + MAX_SEGS]
            us = sum(d for _, d in chunk)
            total += us
            self.b.write_data(b"".join(c for c, _ in chunk))
            time.sleep(max(len(chunk) * SEG_MIN, us * 1e-6) * 0.92)
        self._live = True
        return total

    def path(self, points, lit=None, speed=None, mm_s=None,
             jump_speed=0x2710, jump_delay=0x01F4, overshoot=0, delay=500,
             wiggle=0, wiggle_pitch=None, wiggle_steps=16,
             unlit_at_feed=False, corner_delay=0):
        """Stream a run of points with the laser on or off per segment.

        `points` is a list of (x, y) in counts. `lit` is one boolean per
        segment, so segment i runs points[i] -> points[i+1] as `0x0243` when
        lit and `0x0241` when not. All lit if `lit` is omitted. That gates the
        laser inside one continuous position stream instead of one call per
        piece, and the whole run goes out in MAX_SEGS-sized writes.

        Unlit segments travel at `jump_speed`, since they are traverses between
        pieces of work. `unlit_at_feed=True` runs them at the marking rate
        instead, for the rare case where the slow dark move is deliberate.

        `overshoot` is a laser-off run-up in counts, added before the first
        segment of each lit run and after the last, along that segment's own
        direction, so the mirrors are already moving when the laser strikes.
        It is trimmed to the travel limits rather than refused; the granted
        length comes back in the return value.

        Speed: pass `speed` for a raw Param0 duration, or `mm_s` for a feed
        rate converted per segment. The run-up and run-out move at the same
        rate as the segment they belong to.

        `delay` is the laser-off dwell in microseconds, and it goes only on the
        vector that ends a lit run, which is where both vendor hosts put it.
        Interior vertices get `corner_delay` (0 by default; the vendors use tens
        of microseconds there). Paying the off-delay at every point instead
        would add half a millisecond of dwell per vector, which on a wiggled
        segment of a few hundred vectors is most of the job.

        The path is kinematically ideal: Param0 is a duration and the board
        interpolates it, so nothing here knows whether the mirrors kept up.
        A wiggle is where that bites, since a circle of radius r at v mm/s needs
        v^2 / r of lateral acceleration continuously. Call wiggle_load() for the
        numbers, set_limits() to have them checked, and runup_mm() to size the
        overshoot.

        `wiggle` widens the burn: each lit segment is traced as small circles of
        that radius in counts, advancing `wiggle_pitch` counts per turn, at
        `wiggle_steps` points per turn. The kerf comes out about 2 * wiggle
        wide. Unlit segments are left as plain jumps. With `mm_s` the feed rate
        holds along the real, longer, traced path, so a wiggled segment takes
        proportionally longer; with `speed` the given duration is split across
        the traced path instead, so the segment still takes what you asked.

        The geometry is bounds-checked before anything is written, so a point
        outside 0..0xFFFF stops the job instead of clipping it mid-stream. That
        includes the wiggle: a circle that would leave the field raises rather
        than being flattened against the edge.

        Returns {"commands", "us", "overshoot", "exposure"}, where `exposure`
        is the traced lit length over the straight lit length: how many times
        the beam covers the cut line, which is what a wiggle is bought for.
        """
        pts = [(int(x), int(y)) for x, y in points]
        if len(pts) < 2:
            raise ValueError("path needs at least two points")
        flags = [True] * (len(pts) - 1) if lit is None else [bool(v) for v in lit]
        if len(flags) != len(pts) - 1:
            raise ValueError("lit needs one flag per segment: %d flags for %d "
                             "points" % (len(pts) - 1, len(pts)))
        self._require_in_field(pts)
        over = max(0, int(overshoot))
        wig = max(0, int(wiggle))
        if wig:
            if wiggle_pitch is None or wiggle_pitch <= 0:
                raise ValueError("wiggle needs a positive wiggle_pitch "
                                 "(counts of travel per circle)")
            if int(wiggle_steps) < 4:
                raise ValueError("wiggle_steps must be at least 4")
            if mm_s:
                load = self.wiggle_load(wig * self.field.mm_per_count,
                                        wiggle_pitch * self.field.mm_per_count,
                                        mm_s, wiggle_steps)
                if load["exceeded"]:
                    warnings.warn(
                        "wiggle asks for %.0f loops/s and %.3g mm/s^2 (%.0f g) "
                        "of lateral acceleration: %s. The mirrors will round "
                        "the loops off and the exposure will bunch at the "
                        "turns instead of spreading along the cut."
                        % (load["loop_hz"], load["accel_mm_s2"],
                           load["accel_g"], "; ".join(load["exceeded"])),
                        stacklevel=2)

        granted = over
        cmds = []
        at = None
        straight = 0.0        # lit length as asked for, mm
        lit_path = 0.0        # lit length actually traced, mm
        for i, on in enumerate(flags):
            p0, p1 = pts[i], pts[i + 1]
            starts_run = on and (i == 0 or not flags[i - 1])
            ends_run = on and (i == len(flags) - 1 or not flags[i + 1])

            if starts_run and over:
                lead, got = self._fit_overshoot(p0, p1, over, True)
                granted = min(granted, got)
                cmds.append((S.cmd(0x0241, jump_speed, lead[0], lead[1], 0,
                                   jump_delay), jump_speed))
                if got:
                    us = self._duration(lead, p0, speed, mm_s)
                    cmds.append((S.cmd(0x0241, us, p0[0], p0[1], 0, 0), us))
                at = p0
            elif at != p0:
                cmds.append((S.cmd(0x0241, jump_speed, p0[0], p0[1], 0,
                                   jump_delay), jump_speed))
                at = p0

            if on and wig:
                curve = self._wiggle(p0, p1, wig, wiggle_pitch,
                                     int(wiggle_steps))
                chain = [(int(round(x)), int(round(y))) for x, y in curve]
                self._require_in_field(chain, "wiggle point")
                # Time the rounded points, not the ideal curve: those integers
                # are the only thing the board ever moves between, so timing
                # anything else puts the error straight into the feed rate.
                pairs = list(zip([p0] + chain[:-1], chain))
                if mm_s is not None:
                    subs = [self._duration(a, b, None, mm_s) for a, b in pairs]
                else:
                    # Split the requested duration along the traced path, so a
                    # wiggled segment still takes the time the caller asked for.
                    lens = [self._len_mm(a, b) for a, b in pairs]
                    tot = sum(lens) or 1.0
                    subs = [max(1, int(round(speed * L / tot))) for L in lens]
                last = len(chain) - 1
                for k, ((bx, by), sub) in enumerate(zip(chain, subs)):
                    # Param4 is a dwell at the end point. Inside a wiggle every
                    # point is an interior point, so it stays 0; only the vector
                    # that ends a lit run carries the laser-off delay.
                    p4 = delay if (k == last and ends_run) else 0
                    cmds.append((S.cmd(0x0243, sub, bx, by, 0, p4), sub))
                straight += self._len_mm(p0, p1)
                lit_path += sum(self._len_mm(a, b) for a, b in pairs)
            elif on:
                us = self._duration(p0, p1, speed, mm_s)
                p4 = delay if ends_run else corner_delay
                cmds.append((S.cmd(0x0243, us, p1[0], p1[1], 0, p4), us))
                straight += self._len_mm(p0, p1)
                lit_path += self._len_mm(p0, p1)
            else:
                # An unlit leg is a move between two pieces of work, so it runs
                # at jump speed. Timing it at the marking feed rate instead
                # spends the whole traverse at cutting speed: 20 mm at 600 mm/s
                # is 33 ms of nothing, per gap. Pass unlit_at_feed=True when the
                # controlled slow move is the point.
                us = (self._duration(p0, p1, speed, mm_s) if unlit_at_feed
                      else max(1, int(jump_speed)))
                cmds.append((S.cmd(0x0241, us, p1[0], p1[1], 0,
                                   0 if unlit_at_feed else jump_delay), us))
            at = p1

            if ends_run and over:
                out, got = self._fit_overshoot(p0, p1, over, False)
                granted = min(granted, got)
                if got:
                    us = self._duration(p1, out, speed, mm_s)
                    cmds.append((S.cmd(0x0241, us, out[0], out[1], 0, 0), us))
                    at = out

        total = self._emit(cmds)
        return {"commands": len(cmds), "us": total,
                "overshoot": granted if over else 0,
                "exposure": (lit_path / straight) if straight else 1.0}

    def segments(self, segs, speed=None, mm_s=None, jump_speed=0x2710,
                 jump_delay=0x01F4, overshoot=0, delay=500,
                 wiggle=0, wiggle_pitch=None, wiggle_steps=16,
                 unlit_at_feed=False, corner_delay=0):
        """Mark disjoint segments: an iterable of ((x0, y0), (x1, y1)) in counts.

        Each segment gets its own jump, run-up and run-out, and the lot travels
        as one batch, so n segments cost one write rather than 2n calls. Same
        arguments and return value as path().
        """
        pts, flags = [], []
        for a, b in segs:
            if pts:
                pts.append((int(a[0]), int(a[1])))
                flags.append(False)             # the connecting jump
            else:
                pts.append((int(a[0]), int(a[1])))
            pts.append((int(b[0]), int(b[1])))
            flags.append(True)
        if not flags:
            raise ValueError("no segments given")
        return self.path(pts, lit=flags, speed=speed, mm_s=mm_s,
                         jump_speed=jump_speed, jump_delay=jump_delay,
                         overshoot=overshoot, delay=delay, wiggle=wiggle,
                         wiggle_pitch=wiggle_pitch, wiggle_steps=wiggle_steps,
                         unlit_at_feed=unlit_at_feed,
                         corner_delay=corner_delay)

    def dots(self, points, dwell_us, jump_speed=0x2710, jump_delay=0x01F4):
        """Point marking: jump to each point and fire for `dwell_us`. UNTESTED.

        Emitted as a zero-length `0x0243` whose Param0 is the dwell, the shape
        both vendor hosts use for a settle. Whether the board honours a
        zero-length lit vector as a timed dot has not been measured here.
        """
        pts = [(int(x), int(y)) for x, y in points]
        self._require_in_field(pts)
        dwell = max(1, int(dwell_us))
        cmds = []
        for x, y in pts:
            cmds.append((S.cmd(0x0241, jump_speed, x, y, 0, jump_delay),
                         jump_speed))
            cmds.append((S.cmd(0x0243, dwell, x, y, 0, 0), dwell))
        total = self._emit(cmds)
        return {"commands": len(cmds), "us": total, "overshoot": 0,
                "exposure": 1.0}

    def _counts(self, mm, what=None):
        """Millimetres to a count distance, for radii and spacings.

        Warns when a non-zero request rounds to nothing: silently dropping a
        wiggle turns a cut into a scratch, and the caller would only find out
        from the workpiece.
        """
        if not mm:
            return 0
        n = int(round(float(mm) / self.field.mm_per_count))
        if n == 0 and what:
            warnings.warn("%s of %g mm is under one galvo count on a %g mm "
                          "field, so it is ignored"
                          % (what, mm, self.field.size_mm), stacklevel=3)
        return n

    def path_mm(self, points_mm, lit=None, overshoot_mm=0.0, wiggle_mm=0.0,
                wiggle_pitch_mm=0.0, clamp=False, **kw):
        """path() with points, run-up and wiggle in millimetres."""
        pts = [self.mm(x, y, clamp) for x, y in points_mm]
        return self.path(pts, lit=lit,
                         overshoot=self._counts(overshoot_mm, "run-up"),
                         wiggle=self._counts(wiggle_mm, "wiggle radius"),
                         wiggle_pitch=self._counts(wiggle_pitch_mm,
                                                   "wiggle pitch") or None,
                         **kw)

    def segments_mm(self, segs_mm, overshoot_mm=0.0, wiggle_mm=0.0,
                    wiggle_pitch_mm=0.0, clamp=False, **kw):
        """segments() with points, run-up and wiggle in millimetres."""
        segs = [(self.mm(a[0], a[1], clamp), self.mm(b[0], b[1], clamp))
                for a, b in segs_mm]
        return self.segments(segs,
                             overshoot=self._counts(overshoot_mm, "run-up"),
                             wiggle=self._counts(wiggle_mm, "wiggle radius"),
                             wiggle_pitch=self._counts(wiggle_pitch_mm,
                                                       "wiggle pitch") or None,
                             **kw)

    def dots_mm(self, points_mm, dwell_us, clamp=False, **kw):
        """dots() with points in millimetres."""
        return self.dots([self.mm(x, y, clamp) for x, y in points_mm],
                         dwell_us, **kw)

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

        Vendor captures agree: the word sat at 0x0FBD with the board idle and
        the low byte walked down to 0x96 under sustained streaming while the
        high byte stayed 0x0F, so 189 of 256 free is the idle reading.
        """
        st = self.status()
        return (((st[5] << 8) | st[6]) & ~self.IN_MASK & 0xFFFF) if st else 0

    RUNNING_BIT = 0x08       # byte 2: set by 0x0104 Run, cleared by 0x0105

    def running(self):
        """True once the marking engine has been started (0x0104).

        This is NOT "still marking" -- it stays set until a reset, and
        free_cache() reads idle even while vectors are executing, so neither can
        be polled for completion. Time your own waits.

        Byte 2 bit 5 (0x20) does look like the missing job-complete flag: it
        clears while the queue executes and sets again when it drains, in both
        BslApp and LightBurn captures, over jobs from 34 ms to 2.5 s. Untested
        from this library, so nothing here relies on it yet.
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
        except Exception as first:
            # This write is the one that silences the laser, so a stalled pipe
            # here leaves an output driving. Clear the endpoints and try once
            # more, and if that fails say so loudly rather than returning as if
            # the board were quiet.
            try:
                self.b.recover()
                self.b.write_data(blob)
                time.sleep(0.05)
            except Exception as second:
                warnings.warn(
                    "could not silence the laser (%r, then %r after recover): "
                    "an output may still be driving. Power-cycle the board."
                    % (first, second), stacklevel=2)
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

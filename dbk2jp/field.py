"""
Scan field: millimetres in, 16-bit galvo counts out.

The board only ever sees raw 16-bit DAC coordinates, 0x0000 to 0xFFFF with
0x8000 at the centre. Everything that turns a real dimension into one of those
numbers -- field size, offsets, per-axis scale, mirroring, axis swap, optical
distortion -- happens on the host. This module is that layer.

The parameters come from the machine's `markcfg0`, section [LMC_CFG]:

    FIELDSIZE             field width in mm across the full DAC span
    FIELDOFFSETX/Y        centre offset in mm
    GALVOASPECT0/1        per-axis scale, in percent
    GALVONEGATE0/1        per-axis mirror
    GALVOX                swap X and Y
    GALVODISTOR0/1        barrel / pincushion
    GALVOHORVER0/1        horizontal-vertical ratio
    GALVOTRAPEDISTOR0/1   trapezoid / keystone

    from dbk2jp import Job, Field, CO2

    with Job(CO2, field=Field.from_markcfg("markcfg0")) as j:
        j.begin_mm(start=(-20, -20))
        j.lines_mm([(20, -20), (20, 20), (-20, 20), (-20, -20)])

CAVEAT on the four distortion families. Their *names* and the fact the vendor
applies them are certain, but the exact formulas were not recovered: in the
markcfg0 available here every one of them is 1.0, i.e. identity, so there was
nothing to measure against. They are implemented with the conventional galvo
model and are a no-op at 1.0, which is the value real configs mostly carry.
Anything else should be checked on a test pattern before you trust it. Field
size, offset, aspect, mirror and swap are plain arithmetic and are exact.
"""

CENTRE = 0x8000
FULL = 0xFFFF


class Field:
    """Maps millimetres to galvo counts for one machine."""

    def __init__(self, size_mm=100.0, offset_mm=(0.0, 0.0), aspect=(100.0, 100.0),
                 negate=(False, False), swap_xy=False,
                 distor=(1.0, 1.0), horver=(1.0, 1.0), trapezoid=(1.0, 1.0),
                 correction=None):
        self.size_mm = float(size_mm)
        self.offset_mm = tuple(float(v) for v in offset_mm)
        self.aspect = tuple(float(v) for v in aspect)
        self.negate = tuple(bool(v) for v in negate)
        self.swap_xy = bool(swap_xy)
        self.distor = tuple(float(v) for v in distor)
        self.horver = tuple(float(v) for v in horver)
        self.trapezoid = tuple(float(v) for v in trapezoid)
        self.correction = correction        # see cor.py; None means none

    # ---- construction ---------------------------------------------------

    @classmethod
    def from_markcfg(cls, path, correction=None):
        """Build from a machine's markcfg0."""
        return cls.from_dict(read_markcfg(path), correction=correction)

    @classmethod
    def from_dict(cls, cfg, correction=None):
        def f(key, default=0.0):
            try:
                return float(cfg[key])
            except (KeyError, TypeError, ValueError):
                return default

        return cls(
            size_mm=f("FIELDSIZE", 100.0),
            offset_mm=(f("FIELDOFFSETX"), f("FIELDOFFSETY")),
            aspect=(f("GALVOASPECT0", 100.0), f("GALVOASPECT1", 100.0)),
            negate=(f("GALVONEGATE0") != 0, f("GALVONEGATE1") != 0),
            swap_xy=f("GALVOX") != 0,
            distor=(f("GALVODISTOR0", 1.0), f("GALVODISTOR1", 1.0)),
            horver=(f("GALVOHORVER0", 1.0), f("GALVOHORVER1", 1.0)),
            trapezoid=(f("GALVOTRAPEDISTOR0", 1.0), f("GALVOTRAPEDISTOR1", 1.0)),
            correction=correction,
        )

    # ---- geometry -------------------------------------------------------

    @property
    def half_mm(self):
        """Half the field: the largest coordinate the DAC span reaches."""
        return self.size_mm / 2.0

    @property
    def mm_per_count(self):
        return self.size_mm / float(FULL)

    def contains(self, x_mm, y_mm):
        h = self.half_mm
        return abs(x_mm - self.offset_mm[0]) <= h and abs(y_mm - self.offset_mm[1]) <= h

    def _distort(self, x, y):
        """Optical distortion, applied in mm about the field centre.

        Identity when every factor is 1.0. See the module caveat: the shape of
        these terms is the conventional galvo model, not a recovered formula.
        """
        dx, dy = self.distor
        hx, hy = self.horver
        tx, ty = self.trapezoid
        if (dx, dy, hx, hy, tx, ty) == (1.0, 1.0, 1.0, 1.0, 1.0, 1.0):
            return x, y                      # the common case, exactly

        h = self.half_mm or 1.0
        u, v = x / h, y / h                  # normalised to +/-1

        u *= 1.0 + (dx - 1.0) * v * v        # barrel / pincushion
        v *= 1.0 + (dy - 1.0) * u * u
        u *= hx                              # horizontal-vertical ratio
        v *= hy
        u *= 1.0 + (tx - 1.0) * v            # trapezoid / keystone
        v *= 1.0 + (ty - 1.0) * u
        return u * h, v * h

    def to_counts(self, x_mm, y_mm, clamp=False):
        """Millimetres to a (x, y) pair of 16-bit galvo counts.

        Raises ValueError outside the field unless `clamp` is set, because
        wrapping a coordinate silently would put the beam somewhere else
        entirely rather than at the edge.
        """
        x = x_mm - self.offset_mm[0]
        y = y_mm - self.offset_mm[1]

        if self.correction is not None:
            x, y = self.correction.apply(x, y)

        x, y = self._distort(x, y)

        h = self.half_mm or 1.0
        cx = CENTRE + (x / h) * (self.aspect[0] / 100.0) * (FULL // 2)
        cy = CENTRE + (y / h) * (self.aspect[1] / 100.0) * (FULL // 2)

        if self.negate[0]:
            cx = FULL - cx
        if self.negate[1]:
            cy = FULL - cy
        if self.swap_xy:
            cx, cy = cy, cx

        out = []
        for value, axis in ((cx, "X"), (cy, "Y")):
            value = int(round(value))
            if not 0 <= value <= FULL:
                if not clamp:
                    raise ValueError(
                        "%s=%.3f mm is outside the %g mm field (centre %g, %g)"
                        % (axis, x_mm if axis == "X" else y_mm, self.size_mm,
                           self.offset_mm[0], self.offset_mm[1]))
                value = max(0, min(FULL, value))
            out.append(value)
        return out[0], out[1]

    def to_mm(self, x_counts, y_counts):
        """Counts back to millimetres. Ignores `correction`, which is not
        generally invertible, so it round-trips only an uncorrected field."""
        cx, cy = float(x_counts), float(y_counts)
        if self.swap_xy:
            cx, cy = cy, cx
        if self.negate[0]:
            cx = FULL - cx
        if self.negate[1]:
            cy = FULL - cy
        h = self.half_mm or 1.0
        x = (cx - CENTRE) / (FULL // 2) / (self.aspect[0] / 100.0) * h
        y = (cy - CENTRE) / (FULL // 2) / (self.aspect[1] / 100.0) * h
        return x + self.offset_mm[0], y + self.offset_mm[1]

    def __repr__(self):
        return ("<Field %g mm, offset (%g, %g), aspect (%g%%, %g%%)%s%s%s>"
                % (self.size_mm, self.offset_mm[0], self.offset_mm[1],
                   self.aspect[0], self.aspect[1],
                   ", negate" if any(self.negate) else "",
                   ", swapped" if self.swap_xy else "",
                   ", corrected" if self.correction else ""))


def read_markcfg(path):
    """Parse a markcfg0 / LmcPar.cfg into a flat dict of strings.

    These are INI-shaped but the keys are unique across sections in practice,
    so the section headers are dropped.
    """
    cfg = {}
    with open(path, "r", errors="replace") as fh:
        for line in fh:
            line = line.strip()
            if not line or line.startswith((";", "#", "[")):
                continue
            key, sep, value = line.partition("=")
            if sep:
                cfg[key.strip()] = value.strip()
    return cfg

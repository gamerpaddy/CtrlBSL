"""
Optical distortion correction from a .cor file. UNFINISHED FEATURE.

A galvo head does not paint a perfect square: the two mirrors sit at different
distances from the lens, so the field is barrelled and skewed. Machines ship a
per-head correction table in a `.cor` file, applied on the host before the
coordinates ever reach the board. This board is no different: nothing in the
command set takes a correction table, so it has to happen here.

STATUS
------
On hold until a real `.cor` file turns up to test against. No sample was
available, and the tool that generates them could not be run here, so there was
nothing to check an implementation against. `load_cor()` and `parse_cor()`
raise rather than returning a transform that might be subtly wrong: a bad
correction is worse than none, because the beam still lands somewhere
plausible.

What the format is believed to be, unconfirmed: a text file carrying a grid of
measured calibration points in nominal/actual pairs, at least 3x3, fitted to
coefficients rather than used as a raw lookup table.

WHAT WORKS TODAY
----------------
Everything except reading the file. The transform interface is finished and
wired into `Field`, and both usual fit shapes are implemented and tested, so
you can correct a field right now by measuring points yourself:

    from dbk2jp import Field, GridCorrection

    # mark a grid, measure where it landed: (nominal_mm, actual_mm)
    c = GridCorrection.from_points([((-20, -20), (-19.4, -20.3)),
                                    ((20, -20), (20.6, -20.2)),
                                    ((20, 20), (20.5, 19.6)),
                                    ((-20, 20), (-19.5, 19.7))])
    field = Field(size_mm=100, correction=c)

`PolyCorrection(cx, cy)` covers the coefficient form.

FINISHING IT
------------
`parse_cor()` is the only function to write. Everything downstream already
works, so it just has to return a `PolyCorrection` or a `GridCorrection`.
"""


class Correction:
    """Interface: millimetres in, corrected millimetres out, about the centre."""

    def apply(self, x_mm, y_mm):
        return x_mm, y_mm

    def __repr__(self):
        return "<%s>" % type(self).__name__


class PolyCorrection(Correction):
    """Bivariate polynomial, the shape a coefficient fit implies.

    x' = sum over i,j of cx[i][j] * x**i * y**j, and the same for y'. Identity
    is cx = [[0, 0], [1, 0]], cy = [[0, 1], [0, 0]].
    """

    def __init__(self, cx, cy):
        self.cx = [list(row) for row in cx]
        self.cy = [list(row) for row in cy]

    @staticmethod
    def _eval(coefs, x, y):
        total = 0.0
        for i, row in enumerate(coefs):
            xi = x ** i
            for j, c in enumerate(row):
                if c:
                    total += c * xi * (y ** j)
        return total

    def apply(self, x_mm, y_mm):
        return (self._eval(self.cx, x_mm, y_mm),
                self._eval(self.cy, x_mm, y_mm))

    @classmethod
    def identity(cls):
        return cls([[0.0, 0.0], [1.0, 0.0]], [[0.0, 1.0], [0.0, 0.0]])


class GridCorrection(Correction):
    """Bilinear interpolation over a regular grid of offsets.

    `offsets` is rows of (dx, dy) in mm, row 0 at -half_mm in Y and column 0 at
    -half_mm in X. Outside the grid the edge cell is extrapolated, which keeps
    the transform continuous instead of stepping at the border.
    """

    def __init__(self, offsets, half_mm):
        self.offsets = [list(row) for row in offsets]
        self.half_mm = float(half_mm)
        self.rows = len(self.offsets)
        self.cols = len(self.offsets[0]) if self.rows else 0
        if self.rows < 2 or self.cols < 2:
            raise ValueError("need at least a 2x2 grid")

    def apply(self, x_mm, y_mm):
        span = 2.0 * self.half_mm
        gx = (x_mm + self.half_mm) / span * (self.cols - 1)
        gy = (y_mm + self.half_mm) / span * (self.rows - 1)

        x0 = max(0, min(self.cols - 2, int(gx)))
        y0 = max(0, min(self.rows - 2, int(gy)))
        fx, fy = gx - x0, gy - y0

        def lerp(idx):
            a = self.offsets[y0][x0][idx]
            b = self.offsets[y0][x0 + 1][idx]
            c = self.offsets[y0 + 1][x0][idx]
            d = self.offsets[y0 + 1][x0 + 1][idx]
            return (a * (1 - fx) * (1 - fy) + b * fx * (1 - fy)
                    + c * (1 - fx) * fy + d * fx * fy)

        return x_mm + lerp(0), y_mm + lerp(1)

    @classmethod
    def from_points(cls, pairs, half_mm=None, size=3):
        """Build from measured ((nominal_x, nominal_y), (actual_x, actual_y))
        pairs by inverse-distance weighting onto a `size` x `size` grid.

        Calibration normally wants at least 3x3 measured points; this accepts
        any number and is the practical way to calibrate without a .cor file.
        """
        if not pairs:
            raise ValueError("no calibration points")
        if half_mm is None:
            half_mm = max(max(abs(p[0][0]), abs(p[0][1])) for p in pairs)
        if half_mm <= 0:
            raise ValueError("calibration points are all at the origin")

        grid = []
        for r in range(size):
            row = []
            gy = -half_mm + 2.0 * half_mm * r / (size - 1)
            for c in range(size):
                gx = -half_mm + 2.0 * half_mm * c / (size - 1)
                num_x = num_y = den = 0.0
                for (nx, ny), (ax, ay) in pairs:
                    d2 = (gx - nx) ** 2 + (gy - ny) ** 2
                    if d2 < 1e-12:
                        num_x, num_y, den = nx - ax, ny - ay, 1.0
                        break
                    w = 1.0 / d2
                    num_x += w * (nx - ax)
                    num_y += w * (ny - ay)
                    den += w
                row.append((num_x / den, num_y / den))
            grid.append(row)
        return cls(grid, half_mm)


def parse_cor(data):
    """Turn the bytes of a .cor into a Correction. UNFINISHED.

    The one function left to write. Return a PolyCorrection or a
    GridCorrection; everything downstream already works.
    """
    raise NotImplementedError(
        ".cor parsing is an unfinished feature: the format could not be "
        "confirmed without a real file to test against. See dbk2jp/cor.py. "
        "Calibrate with GridCorrection.from_points(measured_pairs) instead."
    )


def load_cor(path):
    """Read a .cor file and return a Correction. UNFINISHED, raises.

    The read and the sniffing below are real, so a file dropped in here will at
    least report what shape it is, which is the first thing anyone finishing
    this feature needs to know.
    """
    with open(path, "rb") as fh:
        data = fh.read()

    kind = "text" if _looks_like_text(data) else "binary"
    raise NotImplementedError(
        "%s: %d bytes, looks like %s. .cor parsing is an unfinished feature -- "
        "see dbk2jp/cor.py. If you can share this file, it is the missing "
        "piece needed to finish it."
        % (path, len(data), kind)
    )


def _looks_like_text(data, sample=512):
    chunk = data[:sample]
    if not chunk:
        return False
    printable = sum(1 for b in chunk if 32 <= b < 127 or b in (9, 10, 13))
    return printable / float(len(chunk)) > 0.9

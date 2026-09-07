"""
Laser selection.

One place that says what a laser type is and how it wants to be driven, so
callers pick a name instead of remembering that fiber wants a parallel power
byte while CO2 wants PWM duty.

    from dbk2jp import Job, CO2, FIBER, MOPA

    with Job(CO2) as j:
        j.configure(freq_khz=20, power_pct=50)

The type code is the high byte of 0x0211 Param0.
"""

CO2 = "co2"
FIBER = "fiber"
UV = "uv"
GREEN = "green"
MOPA = "mopa"
YAG = "yag"


class Laser:
    """How one laser type is driven.

    code        high byte of 0x0211 Param0
    power       "pwm"  power is PWM duty on the marking line
                "byte" power is an 8-bit parallel word on P0..P7 (latched,
                       PLATCH strobes on change)
    freq_khz    sensible default marking frequency
    freq_range  (min, max) kHz the board was seen to produce
    tickle      laser wants a tickle/pre-ionisation train when idle, and gets
                one by default. tick_khz/tick_us are that default shape.
    mopa_pulse  laser takes a pulse-width setting (0x0206)
    verified    the type code was confirmed on hardware
    """

    def __init__(self, name, code, power, freq_khz, freq_range,
                 tickle=False, mopa_pulse=False, verified=False, note="",
                 tick_khz=5.0, tick_us=1.0):
        self.name = name
        self.code = code
        self.power = power
        self.freq_khz = freq_khz
        self.freq_range = freq_range
        self.tickle = tickle
        self.tick_khz = tick_khz
        self.tick_us = tick_us
        self.mopa_pulse = mopa_pulse
        self.verified = verified
        self.note = note

    def __repr__(self):
        return "<Laser %s code=0x%02X power=%s %g kHz%s>" % (
            self.name, self.code, self.power, self.freq_khz,
            "" if self.verified else " UNVERIFIED")


LASERS = {
    CO2: Laser(CO2, 0x22, "pwm", 20.0, (1.0, 40.0), tickle=True, verified=True,
               tick_khz=5.0, tick_us=1.0,
               note="PWM duty is the power. Tickle is on by default: a CO2 tube "
                    "wants priming between marks. It is a separate free-running "
                    "generator, muxed out while marking."),
    FIBER: Laser(FIBER, 0x11, "byte", 20.0, (1.0, 40.0), verified=True,
                 note="Power is the 8-bit word on P0..P7, static and latched. "
                      "No marking run needed to set it."),
    UV: Laser(UV, 0x33, "pwm", 30.0, (1.0, 40.0), verified=False),
    GREEN: Laser(GREEN, 0x44, "pwm", 30.0, (1.0, 40.0), verified=False),
    MOPA: Laser(MOPA, 0x55, "byte", 30.0, (1.0, 40.0), mopa_pulse=True,
                verified=False,
                note="Pulse width is a separate setting, 0x0206. Config allows "
                     "1 kHz to 2 MHz, far above anything measured here."),
    YAG: Laser(YAG, 0x00, "pwm", 20.0, (1.0, 40.0), verified=False,
               note="Type code is a guess: it is the only unused low nibble and "
                    "was never confirmed on hardware."),
}


def get(kind):
    """Accept a name ('co2'), a Laser, or a raw type code (0x22)."""
    if isinstance(kind, Laser):
        return kind
    if isinstance(kind, int):
        for laser in LASERS.values():
            if laser.code == kind:
                return laser
        return Laser("0x%02X" % kind, kind, "pwm", 20.0, (1.0, 40.0))
    try:
        return LASERS[str(kind).lower()]
    except KeyError:
        raise ValueError("unknown laser %r, pick one of: %s"
                         % (kind, ", ".join(sorted(LASERS))))

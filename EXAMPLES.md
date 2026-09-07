# Examples

Runnable snippets for the DBK2JP. Every one of these was executed against a real
board; what varies is whether a laser of that type was attached to confirm the
optical side. See [API.md](API.md) for the full surface.

Coordinates are 16-bit. `0x8000` is field centre, `0x0000` and `0xFFFF` are the
corners. `speed` is the galvo rate for that move.

- [CO2](#co2)
- [Fiber](#fiber)
- [MOPA](#mopa)
- [UV and green](#uv-and-green)
- [YAG](#yag)
- [Working in millimetres](#working-in-millimetres)
- [Machine integration](#machine-integration) (rotary, homing, triggers, faults)

---

## CO2

Power is **PWM duty** on the marking line. CO2 is the only type with a
**tickle**: a separate free-running pre-ionisation train that keeps the tube
primed between marks. `Job(CO2)` enables it by default at 5 kHz / 1 us, since a
tube normally wants it.

```python
from dbk2jp import Job, CO2

with Job(CO2) as j:
    j.configure(freq_khz=20, power_pct=40)      # 20 kHz, 40% duty
                                                # tickle already on, 5 kHz / 1 us
    j.begin(start=(0x4000, 0x4000), speed=300)
    j.lines([(0xC000, 0x4000),
             (0xC000, 0xC000),
             (0x4000, 0xC000),
             (0x4000, 0x4000)], speed=300)
```

### Tickle

A separate free-running generator with **its own frequency and pulse width**,
both independent of the marking PWM. It is on by default for CO2 at 5 kHz / 1 us.

Set both at once through `configure()`:

```python
with Job(CO2) as j:
    j.configure(freq_khz=20, power_pct=40,      # marking PWM
                tick_khz=10.0, tick_us=2.0)     # tickle
    j.begin(start=(0x4000, 0x8000), speed=300)
    j.lines([(0xC000, 0x8000)], speed=300)
```

Or through `tick()`, which returns what the board will actually produce:

```python
freq_hz, width_ticks, duty_pct = j.tick(freq_khz=10.0, width_us=2.0)
print(freq_hz, width_ticks, duty_pct)       # 9997.9 Hz, 96 ticks, 2.0 %
```

The period is an N+1 counter at 48 MHz, so the frequency lands on the nearest
achievable value. Width is in 48 MHz ticks, 1 us being 48 of them.

Either setting can be changed on its own:

```python
j.tick(width_us=5.0)        # wider pulses, same frequency
j.tick(freq_khz=2.0)        # slower, same width
```

Typical shapes. Width keeps the tube primed without lasing, so it stays short;
frequency sets how often.

```python
j.tick(freq_khz=5.0,  width_us=1.0)     # common starting point, 0.5 % duty
j.tick(freq_khz=20.0, width_us=1.0)     # faster, shorter dead time, 2 % duty
j.tick(freq_khz=1.0,  width_us=5.0)     # slow and wide, 0.5 % duty
```

Both are range checked. The generator covers **0.74 to 100 kHz**, and the width
must be shorter than the period, so an impossible pair is rejected rather than
silently wrapping:

```python
j.tick(freq_khz=5.0, width_us=300)
# ValueError: tickle width 300 us must be >0 and shorter than the
#             200.0 us period at 5 kHz
```

Warm the tube before marking:

```python
import time

with Job(CO2) as j:
    j.configure(freq_khz=20, power_pct=40, tick_khz=5.0, tick_us=1.0)
    j.begin(start=(0x4000, 0x8000), speed=300)  # tickle starts here
    time.sleep(2.0)                             # let the tube settle
    j.lines([(0xC000, 0x8000)], speed=300)
```

Turn it off. It is **free-running**: it keeps pulsing after the job ends and
after your script exits.

```python
with Job(CO2) as j:
    j.tick_off()
```

`close()` does this for you if that job had the tickle on, but a killed process
does not. If a pin is still ticking from an earlier run, the snippet above
clears it.

Marking with no tickle at all, leaving the shape configured:

```python
with Job(CO2) as j:
    j.configure(freq_khz=20, power_pct=40, tickle=False)
    j.begin(start=(0x4000, 0x8000), speed=300)
    j.lines([(0xC000, 0x8000)], speed=300)
```

The tickle does **not** appear in the low phases of the marking PWM. The board
multiplexes it out while marking, so you get tickle or marking, never both
interleaved. That is hardware behaviour, not a limitation here.

**Analog power (DA1, pin 15) does not work.** `ENPOWERANALOGOUT=0` in the
machine config, and nothing else drives it on this machine either. Use PWM duty.

---

## Fiber

Power is an **8-bit parallel word on P0..P7**, not a duty cycle. The board
latches it and strobes `PLATCH` on every change, so it is static: no marking run
is needed to set it, and it persists until you change it.

```python
from dbk2jp import Job, FIBER

with Job(FIBER) as j:
    j.configure(freq_khz=30, power_byte=0xC0,   # 192 of 255
                mo=True)                        # MO and PA enable
    j.begin(start=(0x4000, 0x4000), speed=400)
    j.lines([(0xC000, 0x4000),
             (0xC000, 0xC000),
             (0x4000, 0xC000),
             (0x4000, 0x4000)], speed=400)
```

**Use the raw byte, not a percentage.** The percentage path quantises as
`(pct * 255) // 100`, so many values are unreachable: no percentage produces
exactly `0x80` (50% gives 127, 51% gives 130).

```python
j.configure(power_byte=0x80)     # exact
j.configure(power_pct=50)        # 127, not 128
```

**Set the power word without marking.** Useful for bringing the laser up to a
known level, or for bit-level testing of the P0..P7 wiring.

This leaves a **live signal on the laser control output** until it is cleared.
It is a power level, not a pulse. Clear it with `j.laser_off()`, or let
`close()` do it, or run `python -m dbk2jp off`.

```python
with Job(FIBER) as j:
    j.power_byte(0x01)           # P0 only
    j.power_byte(0x80)           # P7 only
    j.power_byte(0xFF)           # all bits
```

Each write strobes `PLATCH`, so the laser clocks in the new value immediately.

### MO and PA

Most fiber and MOPA sources need the master oscillator and power amplifier
enables asserted before they emit. They are off by default here, since they are
amplifier enables on the laser side:

```python
with Job(FIBER) as j:
    j.configure(freq_khz=30, power_byte=0xC0, mo=True)
    j.begin(start=(0x4000, 0x4000), speed=400)
    j.lines([(0xC000, 0x4000), (0x4000, 0x4000)], speed=400)
```

`j.mo(True)` does the same thing outside `configure()`. Either way it takes
effect with the next job header, so set it before `begin()`. Both pins come up
as the job starts and drop when it ends, and `laser_off()` clears the flag.

---

## MOPA

A fiber laser with a settable **pulse width**, in nanoseconds.

**Use the fiber type, not `MOPA`.** On the board tested, type code `0x55`
mutes every output: PRR, P0, MO and PA all stay dead, while the identical job
under `0x11` drives all four. The pulse width command works normally under the
fiber code.

```python
from dbk2jp import Job, FIBER

with Job(FIBER) as j:
    j.configure(freq_khz=30, power_byte=0x78, mo=True)
    j.mopa_pulse(100)                  # nanoseconds
    j.begin(start=(0x4000, 0x8000), speed=400)
    j.lines([(0xC000, 0x8000)], speed=400)
```

The width travels as a four byte SPI frame, `A5 01` then the value big-endian,
clocked out on **P1 (data)** and **P2 (clock)**. So 100 ns is `A5 01 00 64`,
150 ns is `A5 01 00 96`, 200 ns is `A5 01 00 C8`.

**Keep bits 1 and 2 of the power byte clear.** Those same two pins carry the
parallel power word, so a byte like `0x7F` leaves them high after the frame,
the clock never returns to idle, and the next frame's first byte is mangled.
`0x78` and `0x00` are fine, `0x7F` is not. The API warns rather than emitting a
bad frame.

Sweeping widths at a fixed power, the usual way to find a setting for a
material:

```python
with Job(FIBER) as j:
    j.configure(freq_khz=30, power_byte=0x78, mo=True)
    for i, ns in enumerate([50, 100, 150, 200, 250, 350]):
        j.mopa_pulse(ns)
        y = 0x4000 + i * 0x1800
        j.begin(start=(0x4000, y), speed=400)
        j.lines([(0xC000, y)], speed=400)
```

The frame on the wire is confirmed at 100, 150 and 200 ns. The **optical**
result is not: there is no MOPA source here to measure what the laser does with
it.

---
## UV and green

Driven like CO2 (PWM duty) but with **no tickle**. `configure(tickle=True)`
raises for these types.

```python
from dbk2jp import Job, UV, GREEN

with Job(UV) as j:
    j.configure(freq_khz=30, power_pct=35)
    j.begin(start=(0x4000, 0x4000), speed=250)
    j.lines([(0xC000, 0x4000), (0xC000, 0xC000)], speed=250)

with Job(GREEN) as j:
    j.configure(freq_khz=30, power_pct=35)
    j.begin(start=(0x4000, 0x4000), speed=250)
    j.lines([(0xC000, 0x4000), (0xC000, 0xC000)], speed=250)
```

**Type codes unverified.** `0x33` and `0x44` were never confirmed with a laser
of either type attached. The PWM itself is the same
generator CO2 uses, which is verified.

---

## YAG

PWM duty, no tickle. Q-switched YAG lasers normally want first-pulse
suppression, which **this board does not appear to drive**.

```python
from dbk2jp import Job, YAG

with Job(YAG) as j:
    j.configure(freq_khz=20, power_pct=40)
    j.begin(start=(0x4000, 0x4000), speed=250)
    j.lines([(0xC000, 0x4000), (0xC000, 0xC000)], speed=250)
```

The YAG **type code `0x00` is a guess**: it is the only unused low nibble and was
never confirmed. If you have a YAG head, this is the first thing to check.

**FPS (pin 6) never moves.** Tried and failed: the config FPK values
(`ENFPK=1`, `FPK=40`, `OPC_FPKTIME=20`), tick flags `0x0200` and `0x0300`, a
sweep of every output port index, and `0x0218`, the Q-switch FPK branch, across
four laser types and both Q-switch flags. No `FPS` string exists in any shipped
DLL. If your machine needs first-pulse suppression, it is not reachable from
here.

The `0x0218` command is packed if you want to keep digging:

```python
from dbk2jp import Job, YAG, cmd

FPKTIME, QS = 20, 7          # QS is 3 when ENABLECLOSEQSWTICH is set, else 7

with Job(YAG) as j:
    j.configure(freq_khz=20, power_pct=40)
    j.begin(start=(0x4000, 0x8000), speed=250)
    j.b.write_data(cmd(0x0218, (QS << 8) | (FPKTIME >> 8),
                       (FPKTIME & 0xFF) << 8, 0, 0, 0))
    j.lines([(0xC000, 0x8000)], speed=250)
```

---

## Working in millimetres

The board only understands 16-bit galvo counts. `Field` converts, using the
machine's own `markcfg0`, so a dimension means the same thing here as anywhere
else on that machine.

```python
from dbk2jp import Job, Field, CO2

field = Field.from_markcfg("markcfg0")      # FIELDSIZE, offsets, aspect, mirror
print(field)                                # <Field 100 mm, offset (0, 0), ...>

with Job(CO2, field=field) as j:
    j.configure(freq_khz=20, power_pct=40)
    j.begin_mm(start=(-20, -20))            # a 40 mm square, centred
    j.lines_mm([(20, -20), (20, 20), (-20, 20), (-20, -20)])
```

### If the machine has no config

Not every machine ships with a `markcfg0`. `load_or_create()` writes a default
one the first time and loads it thereafter:

```python
field = Field.load_or_create("markcfg0", size_mm=110.0)
if field.created:
    print("wrote a fresh markcfg0, adjust it for this machine")
```

Adjust the factors and save. An existing file keeps every key it already has;
only the field factors are rewritten.

```python
field.set(size_mm=110.0,
          offset_mm=(-1.5, 0.25),
          aspect=(100.0, 99.4),      # per-axis scale in percent
          negate=(True, False),      # mirror X
          swap_xy=False).save()
```

`set()` validates: an unknown factor, a zero field size or an aspect of 0%
raises rather than writing a config that cannot work.

From the command line, without writing any code:

```bash
python -m dbk2jp field                              # show, creating if needed
python -m dbk2jp field size_mm=110                  # adjust and save
python -m dbk2jp field aspect=100,99.4 negate=1,0
python -m dbk2jp field /path/to/markcfg0 size_mm=175
```

Or skip the file entirely and state the field inline:

```python
field = Field(size_mm=110.0, offset_mm=(0.0, 0.0))
```

### Calibrating scale by hand

Mark a square of known nominal size, measure it, and scale:

```python
field = Field.load_or_create("markcfg0", size_mm=100.0)

# asked for 40 mm, measured 39.6 across X and 40.2 across Y
field.set(aspect=(field.aspect[0] * 40.0 / 39.6,
                  field.aspect[1] * 40.0 / 40.2)).save()
```

Converting by hand:

```python
j.mm(25, -10)              # -> (0xC000, 0x6667)
j.where_mm(0x8000, 0x8000) # -> (0.0, 0.0)
```

`jump_mm()`, `begin_mm()` and `lines_mm()` mirror the count-based calls. A point
outside the field **raises** rather than wrapping, since a wrapped coordinate
puts the beam somewhere plausible but wrong:

```python
j.jump_mm(80, 0)
# ValueError: X=80.000 mm is outside the 100 mm field (centre 0, 0)

j.jump_mm(80, 0, clamp=True)    # clip to the edge instead
```

### What comes out of markcfg0

| Key | Meaning | Status |
|---|---|---|
| `FIELDSIZE` | field width in mm across the full DAC span | exact |
| `FIELDOFFSETX/Y` | centre offset in mm | exact |
| `GALVOASPECT0/1` | per-axis scale, percent | exact |
| `GALVONEGATE0/1` | per-axis mirror | exact |
| `GALVOX` | swap X and Y | exact |
| `GALVODISTOR0/1` | barrel / pincushion | conventional model, unverified |
| `GALVOHORVER0/1` | horizontal-vertical ratio | conventional model, unverified |
| `GALVOTRAPEDISTOR0/1` | trapezoid / keystone | conventional model, unverified |

The four distortion families are named and applied, but their exact formulas are
not known, and in the `markcfg0` available here every one of
them is `1.0`, meaning identity, so there was nothing to measure against. They
are implemented with the conventional galvo model and are a **no-op at 1.0**,
which is what most real configs carry. If yours differs, check a test pattern
before trusting it.

### Optical correction

A galvo head does not paint a perfect square, and machines ship a per-head
correction table applied on the host. Nothing in this board's command set takes
one, so it has to happen here.

**Reading a `.cor` file is an unfinished feature.** It is on hold until a real
one turns up to test against: no sample was available and the tool that
generates them could not be run, so there was nothing to check an
implementation against. `load_cor()` raises rather than returning a transform
that might be subtly wrong, since a bad correction still puts the beam
somewhere plausible.

```python
from dbk2jp import load_cor
load_cor("machine.cor")
# NotImplementedError: machine.cor: 4096 bytes, looks like text. .cor parsing
# is an unfinished feature -- see dbk2jp/cor.py
```

It still reads the file and reports its shape, which is the first thing needed
to finish the feature.

**Correcting a field works today**, by measuring points yourself, which is how
calibration works anyway:

```python
from dbk2jp import Field, GridCorrection, Job, CO2

# mark a grid, measure where the marks actually landed
pairs = [((-20, -20), (-19.4, -20.3)),
         (( 20, -20), ( 20.6, -20.2)),
         (( 20,  20), ( 20.5,  19.6)),
         ((-20,  20), (-19.5,  19.7))]

field = Field(size_mm=100.0, correction=GridCorrection.from_points(pairs))

with Job(CO2, field=field) as j:
    j.configure(freq_khz=20, power_pct=40)
    j.begin_mm(start=(-20, -20))
    j.lines_mm([(20, -20), (20, 20), (-20, 20), (-20, -20)])
```

`PolyCorrection(cx, cy)` is there too, matching the bivariate-polynomial shape a
coefficient fit implies, for when the coefficients are known.

---

## Machine integration

These are independent of laser type.

### Rotary axis: mark, step, repeat

`OUT2` and `OUT3` are the stepper `DIR` and `PULSE` pins, driven by
`axis_move()`. Accel and decel are symmetric.

```python
import time
from dbk2jp import Job, CO2

with Job(CO2) as j:
    j.configure(freq_khz=20, power_pct=40)

    for part in range(12):
        j.begin(start=(0x6000, 0x8000), speed=300)
        j.lines([(0xA000, 0x8000)], speed=300)

        secs = j.axis_move(pulses=800, pps=2000, acctime=100)
        time.sleep(secs + 0.1)
```

`axis_move()` returns the duration it expects to take and does **not** block.
There is no job-complete flag on this board, so the mark and the rotation are
separated by time, not by a status read.

A wide rate span hides the ramp: over 200 to 5000 pps it compresses into a
fraction of a second and looks like an instant start. To see it:

```python
j.axis_move(pulses=2000, pps=500, min_pps=50, acctime=255)
```

### Homing against the origin switch

`IN0` is the X origin switch on machines wired for one. Inputs read 1 when idle
and 0 when driven, so the switch closing is a falling edge.

```python
import time
from dbk2jp import Job

with Job() as j:
    while j.input_pin(0):                       # still off the switch
        j.axis_move(pulses=50, pps=800, direction=1, acctime=20)
        time.sleep(0.1)
    j.axis_move(pulses=200, pps=400)            # back off the switch
```

### Marking from a trigger input

`REMARK` is the mark-repeat trigger. Wire a foot pedal or a part-present sensor
to it.

```python
import time
from dbk2jp import Job, CO2

with Job(CO2) as j:
    j.configure(freq_khz=20, power_pct=40)
    while True:
        while j.remark():                       # idle high, wait for the pull low
            time.sleep(0.005)

        j.begin(start=(0x4000, 0x4000), speed=300)
        j.lines([(0xC000, 0x4000), (0xC000, 0xC000),
                 (0x4000, 0xC000), (0x4000, 0x4000)], speed=300)

        while not j.remark():                   # wait for release
            time.sleep(0.005)
```

### Stopping on a laser fault

```python
from dbk2jp import Job, CO2

with Job(CO2) as j:
    j.configure(freq_khz=20, power_pct=40)
    j.begin(start=(0x4000, 0x4000), speed=300)
    j.lines([(0xC000, 0x4000), (0xC000, 0xC000)], speed=300)

    if not j.guard(2.0):                        # polls SGIN, aborts if it asserts
        print("laser fault, aborted")
```

### Stopping everything

```python
j.laser_off()      # marking PWM, tickle and gate, all off
```

```bash
python -m dbk2jp off
```

Laser outputs are levels and they latch. A plain reset does not clear them.
`close()` runs this for any job that programmed an output, `with Job(...)` calls
`close()`, and an exit hook catches a script that does neither. A hard kill is
not caught.

SGIN0, SGIN1 and SGIN2 are OR'd into a single bit, so you learn *that* a fault
fired, never *which*. `guard()` polls over USB, roughly 4 to 8 ms per round trip,
and dies with the host process: **it is not an interlock.**

**EMSTOP is not visible from here either.** The pin sits at 5 V, no command
moves it, and it appears in no status field, so emergency stop cannot be read or
asserted over USB. It has to break the circuit in hardware.

### Reading the board

```python
from dbk2jp import Job

with Job(unlock_now=False) as j:                # read-only, does not touch state
    print(j.input_pin(0), j.input_pin(1), j.input_pin(2))
    print("remark", j.remark(), "sgin ok", j.sgin())
    print("free cache", j.free_cache(), "of 256")
    print("unlocked", j.unlocked(), "armed", j.armed())
```

### Pilot pointer

```python
with Job() as j:
    j.red_light(True)
    time.sleep(3)
    j.red_light(False)
```

### Output ports

```python
with Job(unlock_now=False) as j:
    j.out(0, 1)          # OUT0 high
    j.out(1, 0)          # OUT1 low
```

`OUT2` and `OUT3` are the stepper `DIR` and `PULSE` pins. Do not drive them with
`out()` while an axis move is running.

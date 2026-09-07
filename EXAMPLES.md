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
interleaved. That is hardware behaviour: the vendor software does exactly the
same thing.

**Analog power (DA1, pin 15) does not work.** `ENPOWERANALOGOUT=0` in the
machine config and the vendor software cannot drive it either. Use PWM duty.

---

## Fiber

Power is an **8-bit parallel word on P0..P7**, not a duty cycle. The board
latches it and strobes `PLATCH` on every change, so it is static: no marking run
is needed to set it, and it persists until you change it.

```python
from dbk2jp import Job, FIBER

with Job(FIBER) as j:
    j.configure(freq_khz=30, power_byte=0xC0)   # 192 of 255
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

```python
with Job(FIBER) as j:
    j.power_byte(0x01)           # P0 only
    j.power_byte(0x80)           # P7 only
    j.power_byte(0xFF)           # all bits
```

Each write strobes `PLATCH`, so the laser clocks in the new value immediately.

---

## MOPA

A fiber laser with a settable **pulse width**, so it takes the fiber power byte
plus one extra parameter.

```python
from dbk2jp import Job, MOPA

with Job(MOPA) as j:
    j.configure(freq_khz=30, power_byte=0x80, mopa_pulse=20)
    j.begin(start=(0x4000, 0x8000), speed=400)
    j.lines([(0xC000, 0x8000)], speed=400)
```

Pulse width goes out as `0x0206`, `Param0 = 0xA501`, `Param1 = pulse`, inside
the EP 0x02 job header. To change it immediately rather than at the next job:

```python
j.mopa_pulse(35)
```

Sweeping widths at a fixed power, which is the usual way to find a setting for a
material:

```python
with Job(MOPA) as j:
    j.configure(freq_khz=30, power_byte=0x80)
    for i, pulse in enumerate([2, 4, 8, 15, 30, 60]):
        j.configure(mopa_pulse=pulse)
        y = 0x4000 + i * 0x1800
        j.begin(start=(0x4000, y), speed=400)
        j.lines([(0xC000, y)], speed=400)
```

**Untested.** The type code `0x55` and the pulse-width command were both read
out of the vendor DLLs and never confirmed against a MOPA laser. The config
allows 1 kHz to 2 MHz, far above anything measured here, so treat the frequency
range in `laser.py` as conservative rather than correct.

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

**Type codes unverified.** `0x33` and `0x44` come from the vendor code and were
never confirmed with a laser of either type attached. The PWM itself is the same
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

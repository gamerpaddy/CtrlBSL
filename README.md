# CtrlBSL

**⚠️ Work in progress.** Verified on one DBK2JP board. Expect gaps and rough edges.

Drive a **BSL/SeaCAD DBK2JP** galvo laser marking controller directly from Python.
No proprietary or paid software needed.

On Windows it goes through the board's existing Cypress CYUSB3 driver, so it
coexists with whatever is already installed and any other software you have
keeps working alongside it. On Linux and macOS it talks plain libusb instead.

Python 3.8+. Drop the `dbk2jp/` folder next to your script.

| Platform | Needs | State |
|---|---|---|
| Windows | stdlib only (`ctypes` + `winreg`), board on the CYUSB3 driver | verified on hardware |
| Linux / macOS | `pip install pyusb` plus libusb; Linux needs a udev rule or root | **untested** |

The Linux path is written against the protocol but has not been run on real
hardware yet. Same `Board` and `Job` on every platform; only the transport
module differs.

Examples: **[EXAMPLES.md](EXAMPLES.md)**  |  Full reference: **[API.md](API.md)**

---

## Quick start

Mark a square:

```python
from dbk2jp import Job, CO2

with Job(CO2) as j:                        # opens, unlocks, arms the board
    j.configure(freq_khz=20, power_pct=40,  # marking PWM
                tick_khz=5.0, tick_us=1.0)  # tickle, on by default for CO2
    j.begin(start=(0x4000, 0x4000), speed=300)
    j.lines([(0xC000, 0x4000),             # coordinates are 16-bit,
             (0xC000, 0xC000),             # 0x8000 is field centre
             (0x4000, 0xC000),
             (0x4000, 0x4000)], speed=300)
```

```bash
python -m dbk2jp devices          # list boards
python -m dbk2jp status           # unlock state, inputs, free cache
python -m dbk2jp inputs 5         # live input view with edge timings
python -m dbk2jp out 1 1          # set OUT1 high
python -m dbk2jp jump 0x4000 0x8000
python -m dbk2jp field             # scan field, creates markcfg0 if missing
python -m dbk2jp field size_mm=110 # adjust and save
```

---

## Choosing a laser

`Job(<type>)` sets the board up for that laser and loads its defaults. Power is
PWM duty for CO2, UV, green and YAG, and an 8-bit parallel word on P0..P7 for
fiber and MOPA. `configure()` accepts either and derives the other.

```python
from dbk2jp import Job, CO2, FIBER, UV, GREEN, MOPA, YAG

with Job(FIBER) as j:
    j.configure(freq_khz=30, power_byte=0xC0)
    print(j.settings())          # exactly what will go on the wire

    j.select(CO2)                # switch type mid-session
    j.configure(power_pct=35, tickle=True)
```

| Type | Code | Power | Notes |
|---|---|---|---|
| `CO2` | `0x22` | PWM duty | tickle on by default, own frequency and width, verified |
| `FIBER` | `0x11` | byte on P0..P7 | latched, PLATCH strobes on change, verified |
| `UV` | `0x33` | PWM duty | code unverified |
| `GREEN` | `0x44` | PWM duty | code unverified |
| `MOPA` | `0x55` | byte on P0..P7 | has a pulse-width setting. **Code 0x55 mutes every output on the board tested**, see below |
| `YAG` | `0x00` | PWM duty | code is a guess, still unconfirmed |

`configure()` rejects a frequency outside the type's range, a tickle on a laser
that has none, and a pulse width on any laser outside the parallel-power
family, so `FIBER` and `MOPA` both accept one.

### MOPA pulse width

`0x0206` carries a frame rather than a plain parameter. Its two params are four bytes,
frame, `A5 01` then the width big-endian, clocked out over SPI on **P1 (data)**
and **P2 (clock)**. The width is in **nanoseconds**, so 100 ns goes out as
`A5 01 00 64`. Confirmed on the wire at 100, 150 and 200 ns.

```python
from dbk2jp import Job, FIBER

with Job(FIBER) as j:
    j.configure(freq_khz=30, power_byte=0x78, mo=True)
    j.mopa_pulse(100)          # nanoseconds
```

Two things to get right:

**Use laser type `0x11`, the fiber code.** On the board tested `0x55` mutes
every output: PRR, P0, MO and PA all stay dead, while the identical job under
`0x11` drives all four. `0x0206` works normally under `0x11`.

**Keep bits 1 and 2 of the power byte clear.** P1 and P2 are also power word
bits, so a byte like `0x7F` holds them high after the frame, the clock stays
high instead of returning to idle, and the next frame's first byte is mangled. `0x7F` corrupts
every frame after the first; `0x00` gives clean ones. The API warns if you set
a pulse width with a colliding power byte.

The optical result is still unverified, there is no MOPA source here, but the
frame on the wire is what the laser expects.

The board drives **all eight** power bits, including P1 and P2: a power byte of
`0x06`, bits 1 and 2 only, raises both pins with no frame sent at all. It
asserts them from the job header and holds them for the whole job, then shifts
the pulse frame out on top of two of them. So MOPA power is eight bits with two
that collide, and keeping those two clear is on you. That leaves bits 0, 3, 4,
5, 6 and 7, so 64 usable levels.

---

## Examples

Per-laser-type examples, plus machine integration: **[EXAMPLES.md](EXAMPLES.md)**

- **CO2** PWM duty, tickle frequency and pulse width, warm-up
- **Fiber** parallel power word on P0..P7, exact byte values, PLATCH
- **MOPA** pulse width and a width sweep
- **UV / green** PWM duty, no tickle
- **YAG** and where first-pulse suppression stops
- **Millimetres** field setup from `markcfg0`, mm coordinates, optical correction
- **Machine integration** rotary axis, homing on the origin switch, trigger
  input, fault handling, pilot pointer, output ports

---

## What works

Verified on hardware with a scope.

| | |
|---|---|
| Unlock (加密 LED) | 3-frame ATSHA204 replay, no key needed |
| Marking PWM | `f = 48e6/(period+1)`, duty is the power, 1 to 40 kHz |
| CO2 tickle | independent free-running generator |
| Fiber power | 8-bit parallel word P0..P7, PLATCH strobes on change |
| Galvos | X and Y, jump and lit vectors |
| Red pilot | CON3 pin 22 |
| Inputs | IN0, IN1, IN2, REMARK |
| SGIN | laser fault line plus `abort()` and `guard()` |
| Outputs | OUT0, OUT1 via `0x0111` |
| MO / PA | `0x0211` Param1 bit 8. Both come up when the job starts and drop when it ends |
| Stepper | pulse count, rate, direction, symmetric accel/decel |
| Millimetre coordinates | field size, offsets, aspect, mirror and swap, read from `markcfg0` or created if there is none |

## What is missing

| | |
|---|---|
| **FPS** (pin 6) | holds at its idle level through every test. Config FPK values, a full output-port sweep and the `0x0218` Q-switch branch were all tried. Likely a board-variant pin |
| **DA1** analog (pin 15) | reads 0 V. `ENPOWERANALOGOUT=0`, and every other software on this machine leaves the pin alone too, so this looks like machine config rather than protocol |
| MOPA | frame verified on the wire, optical result not. Type code 0x55 mutes every output on the board tested |
| UV, green, YAG | type codes unverified |
| Linux, macOS | backend written, still to be run on hardware |
| **.cor files** | **unfinished feature.** On hold until a real `.cor` turns up to test against, so `load_cor()` raises rather than guessing. The transform side is done: calibrate with `GridCorrection.from_points()` meanwhile |
| Galvo distortion terms | `GALVODISTOR`, `GALVOHORVER`, `GALVOTRAPEDISTOR` are all `1.0` (identity) in the available config, so the conventional model used for them is unverified |
| **EMSTOP** | sits at 5 V throughout every test. The command set leaves it alone and the software here leaves it alone, so it looks like a pure hardware interlock line, readable and drivable only from hardware |
| SGIN0..2 | OR'd into one bit, so you learn *that* a fault fired, while *which* one stays hidden |
| SGIN3 | on the connector, in no status field |
| Job complete | no flag found. `0x0101` byte 2 bit 3 only says the engine was started, and `free_cache()` reads idle even while vectors execute, so neither can be polled for completion |
| Untested | `out_pulse()` (`0x2F82`), `laser_port_switch()` (`0x2F84`) |
| `0x0211` Param2 | documented as an MO delay, but sweeping it 0, 1000 and 20000 left every measurement unchanged |
| Unknown | `0x0232` Param0 = 175, opcode `0x1667`, `0x0211` Param3 and Param4 |

---

## Safety

**Laser outputs latch.** Setting a power level puts a live signal on the laser
control pin and leaves it there. It is a level rather than a one-shot, so it
persists until it is cleared or the board is power cycled, and **a plain reset
leaves it running.**

To stop everything, at any time:

```bash
python -m dbk2jp off
```

```python
j.laser_off()          # marking PWM, tickle and gate, all off
```

`close()` does this automatically for any job that programmed an output, and
`with Job(...)` calls `close()`. A script that exits without either still gets
caught by an exit hook. **A hard kill escapes all of that, and so does pulling
the USB cable while output is live.**

`guard()` polls SGIN over USB, roughly 4 to 8 ms per round trip, and it dies with
the host process. **Treat it as a status poll only.** E-stop belongs in hardware,
and so does anything that has to hold while the software is stopped.

The CO2 tickle generator is free-running and behaves the same way: it keeps
pulsing after the job ends.

---

## License

[WTFPL](LICENSE). Do whatever the fuck you want with it.

# CtrlBSL

**⚠️ Work in progress.** Verified on one DBK2JP board. Expect gaps and rough edges.

Drive a **BSL/SeaCAD DBK2JP** galvo laser marking controller directly from Python.
No proprietary or paid software needed.

On Windows it goes through the board's existing Cypress CYUSB3 driver, so nothing
has to be replaced or unbound and any vendor software you already have keeps
working alongside it. On Linux and macOS it talks plain libusb instead.

Python 3.8+. Drop the `dbk2jp/` folder next to your script.

| Platform | Needs | State |
|---|---|---|
| Windows | nothing (`ctypes` + `winreg`), board on the CYUSB3 driver | verified on hardware |
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
    j.configure(freq_khz=20, power_pct=40)  # CO2 gets a 5 kHz / 1 us tickle
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
| `CO2` | `0x22` | PWM duty | tickle on by default (5 kHz, 1 us), verified |
| `FIBER` | `0x11` | byte on P0..P7 | latched, PLATCH strobes on change, verified |
| `UV` | `0x33` | PWM duty | code unverified |
| `GREEN` | `0x44` | PWM duty | code unverified |
| `MOPA` | `0x55` | byte on P0..P7 | has a pulse-width setting, code unverified |
| `YAG` | `0x00` | PWM duty | code is a guess, never confirmed |

`configure()` rejects a frequency outside the type's range, a tickle on a laser
that has none, and a pulse width on a laser that takes none.

### MOPA pulse width

Separate from power and frequency: command `0x0206`, `Param0 = 0xA501`,
`Param1 = pulse`, sent in the EP 0x02 job header.

```python
from dbk2jp import Job, MOPA

with Job(MOPA) as j:
    j.configure(freq_khz=30, power_byte=0x80, mopa_pulse=20)
```

`j.mopa_pulse(value)` sets it immediately instead of at the next job. Both are
**untested**, read out of the vendor pen-parameter path. There is no MOPA laser
here to measure.

---

## Examples

Per-laser-type examples, plus machine integration: **[EXAMPLES.md](EXAMPLES.md)**

- **CO2** PWM duty, tickle options, warm-up
- **Fiber** parallel power word on P0..P7, exact byte values, PLATCH
- **MOPA** pulse width and a width sweep
- **UV / green** PWM duty, no tickle
- **YAG** and where first-pulse suppression stops
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
| Stepper | pulse count, rate, direction, symmetric accel/decel |
| MO / AP / GATE | verified |

## What is missing

| | |
|---|---|
| **FPS** (pin 6) | never moves. Config FPK, a full output-port sweep and the `0x0218` Q-switch branch all tried, and no `FPS` string exists in any vendor DLL. Likely a board-variant pin |
| **DA1** analog (pin 15) | no voltage. `ENPOWERANALOGOUT=0`, and the vendor software cannot drive it either, so this is machine config rather than protocol |
| MOPA | pulse width and type code both unverified, no MOPA laser here |
| UV, green, YAG | type codes unverified |
| Linux, macOS | backend written, never run |
| SGIN0..2 | OR'd into one bit, so you learn *that* a fault fired, never *which* |
| SGIN3 | on the connector, in no status field |
| Job complete | no flag found. `0x0101` byte 2 bit 3 only says the engine was started, and `free_cache()` reads idle even while vectors execute, so neither can be polled for completion |
| Untested | `out_pulse()` (`0x2F82`), `laser_port_switch()` (`0x2F84`) |
| Unknown | `0x0232` Param0 = 175, opcode `0x1667`, `0x0211` Param3 and Param4 |

---

## Safety

`guard()` polls SGIN over USB, roughly 4 to 8 ms per round trip, and it dies with
the host process. **It is not an interlock.** E-stop belongs in hardware.

The CO2 tickle generator is free-running: it keeps pulsing after your script
exits. `close()` switches it off, a killed process does not.

---

## License

[WTFPL](LICENSE). Do whatever the fuck you want with it.

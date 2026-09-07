# CtrlBSL

**⚠️ Work in progress.** Verified on one DBK2JP board. Expect gaps and rough edges.

Drive a **BSL/SeaCAD DBK2JP** galvo laser marking controller directly from Python —
no proprietary or paid software needed. Talks to the stock Cypress **CYUSB3** driver over
its IOCTLs, so the vendor software keeps working alongside it.

Python 3.8+. Drop the `dbk2jp/` folder next to your script.

- **Windows** — no dependencies; goes through the stock CYUSB3 driver.
- **Linux / macOS** — needs `pip install pyusb`; plain libusb bulk transfers.
  Untested, written against the protocol. Linux needs a udev rule or root.

Same `Board` and `Job` on both; the platform difference is one backend module.

Full reference: **[API.md](API.md)**

---

## Quick start

```python
from dbk2jp import Job

with Job() as j:                      # opens, unlocks, arms the board
    j.laser(freq_khz=20, power_pct=50)
    j.jump(0x4000, 0x8000)            # 0x8000 = centre
    j.pwm_burst(seconds=5)
```

```bash
python -m dbk2jp devices          # list boards
python -m dbk2jp status           # unlock state, inputs, free cache
python -m dbk2jp inputs 5         # live input view with edge timings
python -m dbk2jp out 1 1          # set OUT1 high
python -m dbk2jp jump 0x4000 0x8000
```

---

## Examples

**Read inputs and the laser status line**

```python
from dbk2jp import Job

with Job(unlock_now=False) as j:
    print(j.input_pin(0), j.input_pin(1), j.input_pin(2))   # True = idle
    print(j.remark(), j.sgin())                             # sgin False = fault
    print(j.free_cache(), "of 256 queue slots")
```

**Mark a square, watching the laser status line**

```python
from dbk2jp import Job

with Job() as j:
    j.laser(freq_khz=20, power_pct=40)
    j.begin(start=(0x4000, 0x4000), speed=300)
    j.lines([(0xC000, 0x4000), (0xC000, 0xC000),
             (0x4000, 0xC000), (0x4000, 0x4000)], speed=300)
    if not j.guard(2.0):
        print("SGIN fault -- aborted")
```

**Fiber power word / stepper axis**

```python
from dbk2jp import Job, LASER_FIBER

with Job(laser=LASER_FIBER) as j:
    j.power_byte(0x80)                       # P0..P7 parallel word, latched
    j.axis_move(pulses=5000, pps=3000, acctime=100)
```

---

## What works

Verified on hardware with a scope.

| | |
|---|---|
| Unlock (加密 LED) | 3-frame ATSHA204 replay, no key needed |
| Marking PWM | `f = 48e6/(period+1)`, duty = power %, 1–40 kHz |
| CO2 tickle | independent free-running generator |
| Fiber power | 8-bit parallel word P0–P7, PLATCH strobes on change |
| Galvos | X and Y, jump and lit vectors |
| Red pilot | CON3 pin 22 |
| Inputs | IN0–IN2, REMARK |
| SGIN | laser fault line + `abort()` / `guard()` |
| Outputs | OUT0, OUT1 via `0x0111` |
| Stepper | pulse count, rate, direction, symmetric accel/decel |
| MO / AP / GATE | verified |

## What's missing

| | |
|---|---|
| **FPS** (pin 6) | never moves. Config FPK, full output-port sweep and the `0x0218` Q-switch branch all tried; no `FPS` string in any vendor DLL. Likely a board-variant pin |
| **DA1** analog (pin 15) | no voltage. `ENPOWERANALOGOUT=0`; **BslApp fails too** — machine config, not protocol |
| SGIN0–2 | OR'd into one bit — you learn *that* a fault fired, never *which* |
| SGIN3 | on the connector, in no status field |
| Untested | `out_pulse()` (`0x2F82`), `laser_port_switch()` (`0x2F84`) |
| Unknown | `0x0232` Param0 = 175, opcode `0x1667`, `0x0211` Param3/4 |

---

## Safety

`guard()` polls SGIN over USB — ~4–8 ms per round trip, and it dies with the host
process. **It is not an interlock.** E-stop belongs in hardware.

The CO2 tickle generator is free-running: it keeps pulsing after your script
exits. `close()` handles it; a killed process does not.

---

## License

[WTFPL](LICENSE) — do whatever the fuck you want with it.

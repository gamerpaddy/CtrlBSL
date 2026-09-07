# `dbk2jp` - API reference

Drive a BSL/SeaCAD **DBK2JP** galvo laser controller directly over USB, with no
proprietary or paid software.

Every number here was verified on real hardware with a scope. For runnable code
per laser type, see [EXAMPLES.md](EXAMPLES.md).

---

## Requirements

- Python 3.8+.
- **Windows**: no third-party packages (`ctypes` + `winreg`), and the board bound
  to Cypress's **CYUSB3** driver. No admin rights needed.
- **Linux / macOS**: `pip install pyusb` plus libusb. **Untested** -- written
  against the protocol, not verified on non-Windows hardware. Linux needs usbfs
  access, so either root or a udev rule:

  ```
  # /etc/udev/rules.d/99-dbk2jp.rules
  SUBSYSTEM=="usb", ATTR{idVendor}=="04b4", ATTR{idProduct}=="1004", MODE="0666"
  ```

The public API is identical on both. Only the transport differs.

Nothing is installed - put the `dbk2jp/` folder next to your script.

```python
from dbk2jp import Job

with Job() as j:                    # opens, unlocks, leaves the board armed
    j.laser(freq_khz=20, power_pct=50)
    j.jump(0x4000, 0x8000)
    j.pwm_burst(seconds=5)
```

---

## The rule that matters

**Session/control commands go on EP 0x06. Everything that configures a job goes
inline in the EP 0x02 batch, ahead of the vectors.**

The board ACKs parameter commands sent on EP 0x06 and then silently ignores
them - no error, no status change, just no effect. Two separate debugging dead
ends came from this. `Job` already routes each command correctly; it only
matters if you build commands yourself.

---

## Layout

| Module | What it holds |
|---|---|
| `dbk2jp/usb.py` | `Board`: discovery, command framing, backend selection |
| `dbk2jp/_cyusb.py` | Windows backend -- CYUSB3.sys IOCTLs |
| `dbk2jp/_libusb.py` | Linux/macOS backend -- pyusb bulk transfers (untested) |
| `dbk2jp/protocol.py` | the 12-byte `tagSeaCMD` wire format, opcode constants, parameter packing |
| `dbk2jp/unlock.py` | the 3-frame ATSHA204 replay that turns the 加密 LED green |
| `dbk2jp/field.py` | millimetres to galvo counts (`Field`), `markcfg0` reader |
| `dbk2jp/cor.py` | `.cor` optical correction (scaffold, format not recovered) |
| `dbk2jp/job.py` | the high-level API (`Job`) |
| `dbk2jp/__main__.py` | `python -m dbk2jp …` |

---

## Command line

```bash
python -m dbk2jp devices           # list DBK2JP interface paths
python -m dbk2jp status            # unlock state, inputs, free cache
python -m dbk2jp unlock [set]      # replay the auth frames
python -m dbk2jp inputs [secs]     # live input/SGIN view with edge timings
python -m dbk2jp out <port> <0|1>  # set an output port
python -m dbk2jp jump <x> <y>      # move the galvos
python -m dbk2jp field [path] [key=value ...]   # scan field, creates if missing
```

---

## Device discovery

```python
from dbk2jp import find_devices, Board

find_devices()            # -> ['\\\\?\\usb#vid_04b4&pid_1004#...']
Board()                   # first board
Board(index=1)            # second board
Board(path=r"\\\\?\\usb#...")
```

`usb.BACKEND` says which transport is active (`_cyusb` or `_libusb`), and
`Board(backend=...)` forces one.

### Windows only: the interface GUID

**The interface GUID is not portable.** It comes from whichever `.inf` bound the
board, not from the hardware. On the development machine one instance of
`VID_04B4&PID_1004` carried Cypress's stock
`{AE18AA60-7F6A-11D4-97DD-00010229B959}` while a second carried
`{090DE41C-61CA-48A8-AAA8-BBBB057F58A1}` from a different OEM package. So
`find_devices()`:

1. reads the interface GUIDs out of
   `HKLM\SYSTEM\CurrentControlSet\Enum\USB\VID_04B4&PID_1004\*\Device Parameters`,
   putting CYUSB3-serviced instances first;
2. falls back to the stock Cypress GUID;
3. keeps only paths carrying VID `04B4` / PID `1004`.

VID and PID are fixed for this board; the GUID is not. Earlier revisions
hardcoded the GUID and took the first path enumerated - that worked on exactly
one machine.

---

## `Board` - transport

| Method | Purpose |
|---|---|
| `write_cmd(cmd)` | one 12-byte command on EP 0x06 |
| `write_data(blob)` | batched 12-byte commands on EP 0x02 |
| `read_status(ep, timeout_ms)` | raw 12-byte read |
| `ask(cmd, ...)` | send on EP 0x06 and return **its** reply |
| `recover()` | abort + reset all four endpoints |
| `close()` | close the handle |

Use `ask()`, not `write_cmd` + `read_status`. EP 0x88 runs one reply behind
after a burst of mixed opcodes, so the naive pairing hands back the *previous*
command's answer. `ask()` matches the echoed opcode and discards stale replies.
This produced phantom `0x0000` status words before it was found.

---

## Unlocking

The 加密 LED must go green before the board will emit anything.

```python
from dbk2jp import Board, unlock, encrypt_state

with Board() as b:
    unlock(b)                     # 3 frames, ~0.2 s
    encrypt_state(b)              # 2 = authenticated, 0 = not
```

`Job()` calls this automatically unless you pass `unlock_now=False`.

Three frames are enough: the host-computed SHA-256 digest on `0x0C5D`, the MAC
command on `0x0C5C`, and the transmit token. The inter-frame gaps are
load-bearing - the ATSHA204 needs 40-120 ms per command, and replaying
back-to-back reads stale registers and fails silently.

Larger sets are kept for debugging: `SETS["bare"|"rb"|"wake"|"core"|"auth"|"min"|"full"]`.

**Read the unlock state from `0x0102` byte 7, never `0x0101` bit 5.** Bit 5 is a
ready/arm flag; the reset tail sets it with the LED still red.

---

## `Job` - laser and marking

```python
Job(laser=CO2, board=None, index=0, unlock_now=True)
```

### Laser selection

```python
from dbk2jp import Job, CO2, FIBER, UV, GREEN, MOPA, YAG, LASERS

j.select(FIBER)                              # name, Laser object, or raw code
j.configure(freq_khz=30, power_byte=0xC0)
j.settings()                                 # what will go on the wire
```

| Type | Code | Power | Tickle | Pulse width | Verified |
|---|---|---|---|---|---|
| `CO2` | `0x22` | PWM duty | yes, on by default | no | yes |
| `FIBER` | `0x11` | byte on P0-P7 | no | no | yes |
| `UV` | `0x33` | PWM duty | no | no | no |
| `GREEN` | `0x44` | PWM duty | no | no | no |
| `MOPA` | `0x55` | byte on P0-P7 | no | yes | no |
| `YAG` | `0x00` | PWM duty | no | no | no, code is a guess |

The code is the high byte of `0x0211` Param0. `LASERS` holds the table;
`Laser` is the record type if you want to define your own.

`configure(freq_khz, power_pct, power_byte, mopa_pulse, tickle, tick_khz, tick_us)` stores the
settings and emits nothing: they go into each job's EP 0x02 header. Pass
`power_pct` or `power_byte`, whichever suits the laser, and the other is
derived. It raises on a frequency outside the type's range, a tickle on a laser
without one, and a pulse width on a laser that takes none.

The tickle has its own frequency and width, set with `tick_khz` / `tick_us` or
`tick()`. Range is 0.74 to 100 kHz and the width must be shorter than the
period; both are checked.

### MOPA pulse width

`0x0206`, `Param0 = 0xA501`, `Param1 = pulse`, on EP 0x02. Set it through
`configure(mopa_pulse=...)` for the next job, or `mopa_pulse(value)` to send it
now. **Untested** - no MOPA laser here.

### Millimetres

```python
Job(CO2, field=Field.load_or_create("markcfg0"))
```

| Field method | Notes |
|---|---|
| `Field(size_mm=..., offset_mm=..., ...)` | state the field inline |
| `Field.from_markcfg(path)` | load an existing config, raises if missing |
| `Field.load_or_create(path, **defaults)` | load, writing a default config first if there is none. `field.created` says which happened |
| `field.set(**factors)` | adjust in place, validated |
| `field.save(path=None)` | write back, preserving keys this library does not use |
| `field.as_markcfg()` | the factors as config key/value strings |

Also `python -m dbk2jp field [path] [key=value ...]`.

| Method | Notes |
|---|---|
| `mm(x, y, clamp=False)` | mm to galvo counts |
| `where_mm(cx, cy)` | counts back to mm |
| `jump_mm(x, y)` / `begin_mm(start)` / `lines_mm(points)` | mm equivalents |

Out-of-field coordinates raise unless `clamp=True`. `Field` carries field size,
offsets, per-axis aspect, mirror and axis swap (all exact) plus the barrel,
horizontal-vertical and trapezoid terms (conventional model, unverified: every
one is `1.0` in the available config). `Field.correction` takes a `Correction` from `cor.py`. Reading a `.cor` file is
an **unfinished feature**; correcting a field from measured points works today.
See EXAMPLES.md.

### Marking

| Method | Notes |
|---|---|
| `configure(...)` / `select(...)` / `settings()` | laser setup, above |
| `power_byte(value, freq_khz)` | write the fiber P0-P7 word immediately, outside a job |
| `tick(freq_khz, width_us, enable)` / `tick_off()` | CO2 tickle shape. Either argument may be omitted to keep the current value. Returns `(actual_hz, width_ticks, duty_pct)`. **Free-running** - survives job end *and host exit* |
| `begin(start, speed)` / `lines(points, speed)` | lit vector streaming |
| `jump(x, y, speed, delay)` | unlit move. `0x8000` is centre, span `0x0000`-`0xFFFF` |
| `pwm_burst(seconds, ...)` | sustained PWM for scope work, paced off `free_cache()` |
| `red_light(on)` | pilot pointer, CON3 pin 22 |
| `mo(on)` | enable MO and PA, `0x0211` Param1 bit 8 |
| `mopa_pulse(ns)` | MOPA pulse width in nanoseconds, SPI frame on P1 and P2 |
| `laser_port_switch(...)` | port switch, purpose unknown |
| `running()` | engine started (`0x0101` byte 2 bit 3). **Not** "still marking" |
| `free_cache()` | free queue slots, **0 to 256** |
| `laser_off()` | silence every laser output: marking PWM, tickle, gate |
| `stop()` / `close(quiet=True)` | `close` runs `laser_off()` for any job that programmed an output |

There is **no job-complete indicator**. `running()` is set by `0x0104` and stays
set until a reset, and `free_cache()` reads idle even while vectors are
executing. Time your own waits; `axis_move()` returns its expected duration for
exactly this reason.

**Laser outputs latch.** A power level stays on the laser control pin until it
is cleared; a plain reset does not clear it, the generator has to be zeroed
through the EP 0x02 header. `laser_off()` does that, `close()` calls it, and an
exit hook catches a script that exits without either. Order matters inside
`laser_off()`: arming before zeroing restarts the engine with the old values
loaded and emits a burst.

MO and PA (CON3 pins 18 and 19) are enabled by `0x0211` Param1 bit 8, exposed
as `mo(True)` or `configure(mo=True)`. With the bit clear both stay low however
long the engine runs, which is why the `0x0281` / `0x0280` command pair appears
to do nothing: it is not what drives them. With it set, both come up as the job
starts and drop when it ends. They are amplifier enables on the laser side, so
the bit is off by default and `laser_off()` clears it. Set it before `begin()`;
it takes effect with the next job header.

MO leads PA. MO asserts on the header, PA only once the first lit vector
executes, so the gap is however long the host takes to deliver that vector:
41 ms with the header and vectors in a single write, 542 ms with a deliberate
500 ms wait between them. The floor is engine start latency and it quantises in
steps of about 41 ms, landing on 41 or 82 depending on the run, so treat single
measurements as approximate. Adding unrelated parameter commands to the header
does not change it (0, 1, 2 and 3 copies all measured the same), but `0x0206`
adds a repeatable 60 ms. On the way
down PA drops when the vectors stop and MO follows at the reset, about 39 ms
later, and that gap is constant. `0x0211` Param2 is documented as an MO delay
but changing it does not move any of this.

`pwm_burst` is closed-loop against the board's own counter. Open-loop pacing
drains the queue between chunks and the output visibly drops to tickle-only
about once a second.

---

## Inputs

All input state rides in the `0x0101` status reply - there is no separate read
command.

```python
j.status_word()     # 16-bit word at bytes 5..6
j.inputs()          # IN0 in bit 0
j.input_pin(0)      # True = idle/high, False = driven
j.remark()          # REMARK trigger
j.free_cache()      # low byte of the same word
```

| Bit | Signal |
|---|---|
| 0 to 7 | free cache count (256-slot queue, 189 free at idle) |
| 8 | `IN0` - also the **X axis origin/home switch** on some setups |
| 9 | `IN1` |
| 10 | `IN2` |
| 11 | `REMARK` (mark-repeat trigger; there is no IN3 on the connector) |
| 12-15 | always 0 |

**Idle reads 1, driven reads 0** - opto inputs with pull-ups.

`free_cache()` masks bits 8-15 off. Reading the raw 16-bit word makes the count
jump by 256 on every input edge; earlier revisions did exactly that and reported
4029 free slots instead of 189.

---

## Laser status and safety

```python
j.sgin()                    # True = OK, False = fault asserted
j.abort()                   # gate off -> clear cache -> reset
j.guard(seconds)            # poll SGIN, abort on assertion; False if it fired
```

SGIN carries laser fault lines - overheat, back-reflection, ready - which vary
by laser model. Any assertion must stop marking and laser output.

**SGIN0, SGIN1 and SGIN2 are OR'd into one bit** (byte 2 bit 1). The board tells
you that *some* SGIN asserted, never which. Per-fault handling requires reading
the lines outside this board. SGIN3 does not appear in the status reply at all.

**`guard()` is not an interlock.** It is a USB poll: ~4 to 8 ms per round trip, so
worst-case reaction is tens of milliseconds, and it dies with the host process.
E-stop belongs in hardware.

---

## Outputs

```python
j.out(port, value)              # verified on OUT0 and OUT1
j.out_pulse(port, value, ms)    # UNTESTED
j.out_state()                   # 0x0112
```

`0x0111` takes the **port index in the high byte of Param0** and the level in
Param1 - it is not a bitmask. Writing `0x0001` to Param0 addresses port 0 with
level 0 and does nothing, which is what the first attempts did.

**OUT2 and OUT3 are the stepper `DIR` and `PULSE` pins**, owned by
`axis_move()`. Do not drive them with `out()` while an axis move is running.

---

## Stepper axis

```python
j.axis_move(pulses, pps, direction=0, min_pps=None, acctime=100, p232=175)
```

| Field | Where |
|---|---|
| pulse count | `0x0230` Param0/Param1 (32-bit) |
| direction | `0x0230` Param4 bit `0x100` (REVROT) |
| start / target rate | `0x0231` Param0 / Param1 |
| acceleration time | `0x0231` Param2 **high byte** |

Accel and decel are symmetric. They only look asymmetric over a wide rate span
(200→5000 pps) because the ramp compresses into a fraction of a second; at
50→500 pps both are equally visible.

---

## Missing pieces

Documented so nobody re-runs these experiments.

### FPS - first pulse suppression (CON3 pin 6) - not reachable

Never observed to move. Exhausted:

- 48 triggered mark-starts with the real config values (`ENFPK=1`, `FPK=40`,
  `OPC_FPKTIME=20`), tick flags `0x0200` / `0x0300`, laser types `0x33`, `0x44`.
- Output-port sweep: every `0x0111` index 0-15 toggled together. Not a GPIO.
- `0x0218`, the Q-switch FPK branch of `SendPenPara`, which the first round never
  sent: `Param0 = (qs<<8) | (FPKTime>>8)`, `Param1 = (FPKTime&0xFF)<<8`, `qs` 3
  or 7. Across laser types `0x00`/`0x22`/`0x33`/`0x66`, FPKTime 20 and 40.
- No `FPS`, `FirstPulse` or `PulseSuppress` string in any shipped DLL.

Most likely a board-variant pin this firmware never drives.

### DA1 - analog CO2 power (CON3 pin 15) - not reachable

`j.dac()` produces no voltage. `ENPOWERANALOGOUT=0` in `markcfg0`, and
`SendPenPara` only emits the analog command (`0x0207`) for `iLsrType` 0 or 6.
**No other software drives it here either**, so this is a machine-config issue, not a
protocol gap.

### EMSTOP - not readable or drivable

The pin sits at 5 V and never changed across any test. No command in the set
moves it, it appears in no status field, and nothing here can assert it. It behaves as a hardware interlock line that the host is simply
not part of.

This matters for safety design: you cannot read emergency-stop state over USB,
so an E-stop has to break the circuit in hardware. See the note on `guard()`
above, which has the same limitation for a different reason.

### Smaller unknowns

- `0x0232` Param0 = 175 in every LightBurn jog. Purpose unknown; moves nothing.
- `0x1667` appears 269× in the ramp capture and in no opcode table.
- `0x0211` Param3/Param4 carry a 32-bit float lost to `_ftol2_sse`; always sent as 0.
- `laser_port_switch()` (`0x2F84`) and `out_pulse()` (`0x2F82`) are untested.
- SGIN3 is on the connector but in no status field.

### Closed as not protocol issues

Tickle-during-mark: the board multiplexes the tickle out while marking, and the
other software behaves identically. Hardware, not a gap in this API.

---

## Gotchas

- **The tickle generator is free-running.** It keeps pulsing after the job ends
  *and after the host process exits*. `close()` handles it; a killed process
  does not.
- **The unlock latch is sticky.** Once green it survives everything short of a
  power cycle - a deliberately corrupted digest is ignored silently. Testing the
  unlock path costs one power cycle per experiment.
- **EP 0x02 overrun clears the ready bit.** `ucPara0` drops to `0x0e` and the
  board needs re-arming. Pace against `free_cache()`.
- **Pausing with `0x0125`** also takes `ucPara0` to `0x0e`.

## MOPA pulse width

`0x0206` carries a four byte SPI frame, not a parameter: `A5 01` then the
width big-endian, clocked out on **P1 (data)** and **P2 (clock)**. The unit is
**nanoseconds**, so `mopa_pulse(100)` puts `A5 01 00 64` on the wire.
Confirmed at 100, 150 and 200 ns.

P1 and P2 are also bits 1 and 2 of the parallel power word, so a power byte
with either set holds them high after the frame and the clock never returns to
idle, mangling the next frame's first byte. `0x7F` corrupts every frame after
the first, `0x00` gives clean ones. `mopa_pulse()` warns on a colliding power
byte rather than silently emitting a bad frame.

Laser type `0x55` mutes every output on the board tested, so drive a MOPA
source as `FIBER` and set the pulse width. The optical result is unverified.

The board drives **all eight** power bits, including P1 and P2: a power byte of
`0x06`, bits 1 and 2 only, raises both pins with no frame sent at all. It
asserts them from the job header and holds them for the whole job, then shifts
the pulse frame out on top of two of them. So MOPA power is not six bits by
design, it is eight with two that collide, and keeping them clear is on you.
That leaves bits 0, 3, 4, 5, 6 and 7, so 64 usable levels.


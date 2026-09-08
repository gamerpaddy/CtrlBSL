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
- **Linux**: `pip install pyusb` plus libusb. Verified on hardware:
  enumeration, unlock, status and marking. macOS uses the same backend and is
  still to be tried. Linux needs usbfs access, so either root or a udev rule:

  ```
  # /etc/udev/rules.d/99-dbk2jp.rules
  SUBSYSTEM=="usb", ATTR{idVendor}=="04b4", ATTR{idProduct}=="1004", MODE="0666"
  ```

The public API is identical on both. Only the transport differs.

On Linux a stalled endpoint survives a close, so a finished job could leave the
pipes halted and the next process would enumerate, unlock and report status
while every transfer that mattered went nowhere: the symptom is a first run that
marks and a second that stays silent. The backend clears the halts on
open and on close, the open side also covering a process that was killed before
it could clean up.

Zero install: put the `dbk2jp/` folder next to your script.

```python
from dbk2jp import Job

with Job() as j:                    # opens, unlocks, leaves the board armed
    j.configure(freq_khz=20, power_pct=50)
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
| `dbk2jp/_libusb.py` | Linux/macOS backend -- pyusb bulk transfers, verified on Linux |
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

**The interface GUID varies per machine.** It comes from whichever `.inf` bound
the board, so it is a property of the driver package rather than the hardware. On the development machine one instance of
`VID_04B4&PID_1004` carried Cypress's stock
`{AE18AA60-7F6A-11D4-97DD-00010229B959}` while a second carried
`{090DE41C-61CA-48A8-AAA8-BBBB057F58A1}` from a different OEM package. So
`find_devices()`:

1. reads the interface GUIDs out of
   `HKLM\SYSTEM\CurrentControlSet\Enum\USB\VID_04B4&PID_1004\*\Device Parameters`,
   putting CYUSB3-serviced instances first;
2. falls back to the stock Cypress GUID;
3. keeps only paths carrying VID `04B4` / PID `1004`.

VID and PID are fixed for this board, while the GUID varies. Earlier revisions
hardcoded the GUID and took the first path enumerated, which worked on exactly
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

### Failures

Transfers that the driver rejects raise `BoardError` instead of returning
quietly. A write on a stalled pipe otherwise looks exactly like a write that
worked, which is the failure mode that made the Linux endpoint bug so hard to
see. The status helpers keep their old contract and answer `None` when the board
does not, so only the data path raises: geometry that never landed must not look
like geometry that did. `recover()` clears and resets all four endpoints, and
`ask()` now runs it once by itself when the reply pipe stops answering, then
retries.

`laser_off()` treats its own failure as serious: if the silencing write fails it
recovers the endpoints, writes once more, and warns loudly if that also fails,
since the alternative is returning as though the board were quiet while an
output is still driving.

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
`ensure_unlocked(attempts=2)` replays until the board reports authenticated: the
ATSHA204 answers from stale registers when it is rushed, the latch is sticky, and
a repeat costs nothing. It warns rather than failing silently if the board stays
locked, which otherwise shows up as a job that runs and emits nothing.
`encrypt_state()` returns `None` when the reply is missing or short, so an
unreadable board is distinguishable from a locked one.

Three frames are enough: the host-computed SHA-256 digest on `0x0C5D`, the MAC
command on `0x0C5C`, and the transmit token. The inter-frame gaps are
load-bearing - the ATSHA204 needs 40-120 ms per command, and replaying
back-to-back reads stale registers and fails silently.

Larger sets are kept for debugging: `SETS["bare"|"rb"|"wake"|"core"|"auth"|"min"|"full"]`.

**Read the unlock state from `0x0102` byte 7, and treat `0x0101` bit 5 as
unrelated.** Bit 5 is a
ready/arm flag; the reset tail sets it with the LED still red. Vendor captures
put a second meaning on it: it also clears while the vector queue executes, see
Laser status and safety.

USB captures of both BslApp and LightBurn driving the same board run the
identical exchange, each with its own digest and challenge bytes, so the
handshake works with any host-drawn challenge rather than only the captured one.
BslApp matches the frame list here command for command, including the licence
tail and the `0106 0105 0104 [jump] 0105 0118 0105` arm sequence, which is where
that list came from. Its preamble differs: it opens with `0x0123`
(replies `FFFF`), `0x0140` with Param4 = 1, and `0x0110` (replies `05 12 08 0D`,
a version word that also leads the `0x0102` reply), then reads the ATSHA data
zone at word address `0x0018` twice before the config read this library starts
from. None of that is needed for the gate. It also skips the long
`0x2000` / `0x3000` / `0x3800` read tail entirely and goes straight to `0x0102`,
`0x0105`, `0x010D`, `0x0104`.

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
| `FIBER` | `0x11` | byte on P0-P7 | no | yes | yes |
| `UV` | `0x33` | PWM duty | no | no | no |
| `GREEN` | `0x44` | PWM duty | no | no | no |
| `MOPA` | `0x55` | byte on P0-P7 | no | yes | no, **0x55 mutes every output on the board tested, use `FIBER`** |
| `YAG` | `0x00` | PWM duty | no | no | no, code is a guess |

The code is the high byte of `0x0211` Param0. `LASERS` holds the table;
`Laser` is the record type if you want to define your own.

`configure(freq_khz, power_pct, power_byte, mopa_pulse, tickle, tick_khz,
tick_us, mo)` stores the
settings and stays silent on the wire: they go into each job's EP 0x02 header. Pass
`power_pct` or `power_byte`, whichever suits the laser, and the other is
derived. It raises on a frequency outside the type's range, a tickle on a laser
without one, and a pulse width on any laser outside the parallel-power family,
so `FIBER` and `MOPA` accept one while the PWM types reject it.

The tickle has its own frequency and width, set with `tick_khz` / `tick_us` or
`tick()`. Range is 0.74 to 100 kHz and the width must be shorter than the
period; both are checked.

### MOPA pulse width

In nanoseconds, as an SPI frame on P1 and P2. Set it with
`configure(mopa_pulse=...)` for the next job, or `mopa_pulse(ns)` to send it
now. Full details below under [MOPA pulse width](#mopa-pulse-width-1).

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
| `field.save(path=None)` | write back, preserving keys beyond the ones this library reads |
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
| `begin(start, speed)` / `lines(points, speed)` | lit vector streaming. `speed` is the per-segment **duration in microseconds**, see below |
| `path(points, lit=..., mm_s=...)` | one position stream, laser gated per segment, with run-up, wiggle and dwell placement |
| `segments(segs, ...)` | disjoint segments in one batch |
| `dots(points, dwell_us)` | point marking. **UNTESTED** |
| `path_mm` / `segments_mm` / `dots_mm` | the same in millimetres |
| `jump(x, y, speed, delay)` | unlit move. `0x8000` is centre, span `0x0000`-`0xFFFF`. `speed` is a duration, `delay` a settle time, both microseconds |
| `pwm_burst(seconds, ...)` | sustained PWM for scope work, paced off `free_cache()` |
| `red_light(on)` | pilot pointer, CON3 pin 22 |
| `mo(on)` | enable MO and PA, `0x0211` Param1 bit 8 |
| `mopa_pulse(ns)` | MOPA pulse width in nanoseconds, SPI frame on P1 and P2 |
| `laser_port_switch(...)` | port switch, purpose unknown |
| `running()` | engine started (`0x0101` byte 2 bit 3). **Not** "still marking" |
| `wiggle_load(r_mm, pitch_mm, mm_s)` | what a wiggle demands of the mirrors, before cutting |
| `set_limits(...)` / `runup_mm(mm_s)` | state the machine kinematics; size a run-up |
| `busy()` / `wait_idle(timeout)` | queue still executing, from byte 2 bit 5. Read from vendor captures, unverified here |
| `free_cache()` | free queue slots, **0 to 256** |
| `laser_off()` | silence every laser output: marking PWM, tickle, gate |
| `stop()` / `close(quiet=True)` | `close` runs `laser_off()` for any job that programmed an output |

There is **no job-complete indicator exposed here**. `running()` is set by
`0x0104` and stays set until a reset, and `free_cache()` reads idle even while
vectors are executing. Time your own waits; `axis_move()` returns its expected
duration for exactly this reason.

Vendor captures say one exists: `0x0101` byte 2 bit 5 (`0x20`) clears while the
queue executes and sets again when it drains, with no host action, in both
LightBurn and BslApp. LightBurn gave a 34 ms window against about 25 ms of summed
vector durations; BslApp gave a clean 2.475 s window on a 1473-vector fill.

```
4.263  byte2 = 0x2E   idle, armed
4.272  byte2 = 0x0E   executing
4.306  byte2 = 0x2E   drained
```

Untested from this library, and it cuts against the reading of `0x0e` as a lost
unlock bit below, where it is the ordinary executing state and recovers on its
own. Confirm on the bench before relying on either.

`busy()` and `wait_idle(timeout, poll)` expose it. `wait_idle` returns False on
timeout rather than raising, so a board that never sets the bit costs a wait and
nothing else, and `armed()` reads the same bit: it returns False mid-mark on a
perfectly healthy board.

**Laser outputs latch.** A power level stays on the laser control pin until it
is cleared. A plain reset leaves it running: the generator has to be zeroed
through the EP 0x02 header. `laser_off()` does that, `close()` calls it, and an
exit hook catches a script that exits without either. Order matters inside
`laser_off()`: arming before zeroing restarts the engine with the old values
loaded and emits a burst.

MO and PA (CON3 pins 18 and 19) are enabled by `0x0211` Param1 bit 8, exposed
as `mo(True)` or `configure(mo=True)`. With the bit clear both stay low however
long the engine runs, which is why the `0x0281` / `0x0280` command pair appears
to be inert: something else drives these pins. With it set, both come up as the job
starts and drop when it ends. They are amplifier enables on the laser side, so
the bit is off by default and `laser_off()` clears it. Set it before `begin()`;
it takes effect with the next job header.

MO leads PA. MO asserts on the header, PA only once the first lit vector
executes, so the gap is however long the host takes to deliver that vector:
41 ms with the header and vectors in a single write, 542 ms with a deliberate
500 ms wait between them. The floor is engine start latency and it quantises in
steps of about 41 ms, landing on 41 or 82 depending on the run, so treat single
measurements as approximate. Adding unrelated parameter commands to the header
leaves it unchanged (0, 1, 2 and 3 copies all measured the same), while `0x0206`
adds a repeatable 60 ms. On the way
down PA drops when the vectors stop and MO follows at the reset, about 39 ms
later, and that gap is constant. `0x0211` Param2 is documented as an MO delay
but changing it leaves all of this where it is.

`pwm_burst` is closed-loop against the board's own counter. Open-loop pacing
drains the queue between chunks and the output visibly drops to tickle-only
about once a second.

### What the vendor host sends that this library does not

From the same LightBurn capture, for anyone chasing a behaviour difference:

| command | LightBurn | BslApp | here |
|---|---|---|---|
| `0x0123` | opens the session | absent | absent |
| `0x0140`, `0x0110` | before unlock | before unlock | absent |
| `0x010D` | once at init | absent | absent |
| `0x0205` | absent | once, ahead of the first vector | absent |
| `0x0212` | `0 0 FFFF FFFF 0` | same | absent |
| `0x0213` | all zeros | P2 = `0x00FF` | absent |
| `0x0106` | never | before every job and between framing chunks | before every job |
| `0x0118` | never | once in the init tail | in the unlock tail |

`0x0110` is a firmware version read: it replied `05 12 08 0D` while BslApp's own
log printed `FPGA:5.18.8.13`. The same four bytes lead the `0x0102` reply.

Header fields left at zero here that a vendor host sets: `0x0208` Param1 carries
laser on TC (LightBurn), `0x0211` Param1 carries an extra bit beside
`MO_ENABLE` (`0x0900` in LightBurn, `0x4100` in BslApp), and `0x0211` Param2 is
8000 in every capture from both.

`0x0211` Param0 behaves exactly as `red_light()` describes it: high byte laser
type, low byte red-light flag. BslApp sent `0x0022` on the header that preceded
its jump-only framing pass and `0x0000` on the one that preceded the marks, so
that capture is an independent confirmation of the red-light encoding rather
than a laser type of `0x22`. What is still open is the type value itself:
LightBurn sent `0x1100` in one session and `0x0000` in another, BslApp `0x0000`,
all marking correctly. None of this has been tested here in isolation.

### Streaming positions

`lines()` marks everything it is given. These three carry the laser state with
the geometry instead, and batch the whole run into `MAX_SEGS`-sized writes:

```python
j.path(pts, lit=[True, False, True], mm_s=800)     # gate per segment
j.segments([(a, b), (c, d)], mm_s=800)             # disjoint segments, one batch
j.dots(pts, dwell_us=250)                          # UNTESTED
```

`path(points, lit)` takes one boolean per segment, so segment *i* runs
`points[i] -> points[i+1]` as `0x0243` when lit and `0x0241` when not. That is
the way to turn the laser off partway through a run of points without a call per
piece. `segments()` builds the same stream from disjoint pairs, adding the
connecting jumps itself: *n* segments cost one write rather than 2*n* calls.

Speed comes either as `speed=` (raw Param0, a duration) or `mm_s=` (a feed rate,
converted per segment through the field, which is the only way to paint unequal
segments evenly). Exactly one of the two, or it raises.

**Run-up (`overshoot`)** adds a laser-off lead-in before the first segment of
each lit run and a lead-out after the last, along that segment's own direction,
so the mirrors are already at speed when the laser strikes and still moving when
it stops. Both move at the segment's own rate. In counts for `path()` and
`segments()`, in millimetres for the `_mm` wrappers.

Run-up is trimmed, not refused: a lead-in that would leave the 0..0xFFFF travel
limits is shortened to the longest one that fits, and the granted length comes
back in the return value, so marking near an edge still happens with whatever
acceleration distance is available.

**Wiggle** orbits the line while walking it, so the beam covers the same cut
several times per millimetre of advance. It is there to concentrate exposure for
cutting rather than to draw a wider line, and the widening is a side effect of
the orbit:

```python
r = j.path(pts, mm_s=600, wiggle=60, wiggle_pitch=120)
r["exposure"]      # 3.20 -> the beam traces 3.2 mm of path per mm of cut
```

`wiggle` is the circle radius, `wiggle_pitch` how far along the line one full
circle advances, `wiggle_steps` how many points make up a circle (16 by
default). Pitch is the dose control: at radius 60 counts, a pitch of 400 gives
1.21x exposure and a pitch of 120 gives 3.20x, on the same geometry at the same
feed rate. Tightening the pitch multiplies dwell without touching power or
speed. The kerf comes out about `2 * wiggle` wide.

`exposure` in the return value is the traced lit length over the straight lit
length, so it is the multiplier on both dwell and job time. Only lit segments
are wiggled; unlit ones stay traverses.

Points are spaced by arc length rather than by angle. Sampling the loop evenly
in angle bunches them where the curve doubles back, which quantises badly once a
chord drops near a single count, and it concentrates the dose by accident
instead of by the pitch you set.

Timing follows the traced path: at a fixed `mm_s` a wiggled segment takes
`exposure` times longer, which is the point. With `speed=` the duration you gave
is split across the traced path instead, so the segment still takes what you
asked and the extra coverage comes out of dwell per point rather than total
time.

Bounds apply to the widened path too: a circle that would leave the field raises
rather than being flattened against the edge, and the message names the offending
wiggle point. A radius or pitch given in millimetres that rounds to less than one
galvo count warns rather than quietly marking a plain line.

**The path is kinematically ideal, and the mirrors are not.** Param0 is a
duration the board interpolates; nothing in the protocol reports whether the
galvos kept up. A wiggle is where that matters, because a circle of radius *r*
walked at *v* needs `v^2 / r` of lateral acceleration continuously:

| radius | pitch | feed | loops/s | lateral | exposure |
|---|---|---|---|---|---|
| 0.1 mm | 0.6 mm | 600 mm/s | 1000 | 367 g | 1.45x |
| 0.1 mm | 0.6 mm | 150 mm/s | 250 | 23 g | 1.45x |
| 0.3 mm | 1.0 mm | 300 mm/s | 300 | 31 g | 2.13x |

Ask for more than the mirrors will follow and they round the loops off into
smaller, smoothed ovals, and the dwell piles up wherever the servo reverses. The
exposure then bunches at the turns rather than spreading along the cut, which is
the opposite of what the wiggle was for. Feed rate and radius are the two ways
out, and the first row above shows the trap: dropping the feed by 4x leaves the
exposure multiplier untouched while cutting the acceleration demand by 16.

```python
j.wiggle_load(0.1, 0.6, 600)
# loop_hz 1000, accel_g 367, exposure 1.45, vectors_per_s 16000, exceeded [...]

j.set_limits(max_mm_s=3000, max_accel_mm_s2=200000, max_loop_hz=250)
j.runup_mm(600)        # 0.900 mm to reach 600 mm/s at that acceleration
```

`wiggle_load()` needs no limits to report the physics. `set_limits()` states what
your machine will do, and `path()` then warns when a wiggle exceeds it. Nothing
is assumed: with the limits unset, only the vector rate is checked, against the
board's own measured ~33 000 vectors per second. Measure the other two by cutting
test loops and watching where the corners start rounding.

`runup_mm(mm_s)` sizes the `overshoot` argument as `v^2 / 2a`. The overshoot
arguments themselves take whatever number you give and make no claim that the
mirrors are up to speed by the end of it.

```python
r = j.segments_mm([((-10, 0), (10, 0))], mm_s=1500, overshoot_mm=0.5)
# {'commands': 4, 'us': 24000, 'overshoot': 298}   <- 298 counts = 0.5 mm granted
```

The geometry itself is bounds-checked before anything is written, so a point
outside the travel limits raises rather than clipping mid-stream. All three
return `{"commands", "us", "overshoot", "exposure"}`, where `us` is the summed
duration the board should take, which is also what the host paces against.

Unlit segments travel at `jump_speed`. Timing them at the marking rate instead
spends every traverse at cutting speed, which is 33 ms of darkness for a 20 mm
gap at 600 mm/s; pass `unlit_at_feed=True` where the slow dark move is
deliberate.

**Dwell placement.** Param4 is a dwell at the end point, and it goes only on the
vector that ends a lit run, which is where both vendor hosts put it. Interior
vertices take `corner_delay`, 0 by default. Spending the laser-off delay at
every point instead costs half a millisecond per vector: on one wiggled segment
of 328 vectors that is 164 ms of standing still, most of the job. `lines()`
keeps its historical 500 on every point for compatibility and now takes
`delay=` so callers can set it.

### EP 0x84 acknowledges every EP 0x02 write

Each write to EP 0x02 is answered on EP 0x84 with a 12-byte record: opcode
`0x0003`, a 16-bit running counter that advances 6 per 12-byte command accepted,
a state byte, then the same free-slot word as `0x0101` bytes 5 and 6. The
vendor capture holds 3631 writes and 3631 acks, one to one, so pacing can ride
the acks with no status polling at all. This library polls `0x0101` instead and
never reads EP 0x84.

The free-slot word read `0x0FBD` idle and walked down to `0x0F96` under sustained
streaming, with the high byte fixed at `0x0F` throughout. That is the low byte
moving from 189 to 150 of 256, so it confirms the masking `free_cache()` already
does rather than contradicting it.

### Vector timing: `speed` is a duration in microseconds

Param0 of `0x0241` and `0x0243` is the time the board takes over that one
segment, in microseconds. The board interpolates the whole move itself, so a
fixed Param0 gives a different feed rate on every segment length:

```
mm_per_s   = length_mm / (Param0 * 1e-6)
Param0_us  = length_mm / mm_per_s * 1e6
```

Established from vendor USB captures of LightBurn marking known rectangles.
A 4 x 3 mm rectangle at 2000 mm/s on a 90.1 mm lens sent Param0 = 1000 for its
2.000 mm halves and 750 for its 1.500 mm halves, so both resolve to 2000 mm/s.
The same rectangle at 1337 mm/s on a 110 mm lens sent 1495 for 2.001 mm and
1121 for 1.500 mm, which resolve to 1338 and 1337 mm/s. Jumps follow the identical rule: framing moves came out at
100 mm/s, positioning jumps at 8035 mm/s against a jump speed setting of 8000.

The `speed=` arguments on `begin()`, `lines()`, `jump()` and their `_mm`
variants are this raw Param0, so they are durations, not rates. Passing one
number for a run of unequal segments paints them at unequal speeds.

`lines()` paces on the larger of the commanded duration and the measured host
cost per segment. Pacing on the host cost alone, as it did before this was
understood, under-sleeps by the whole ratio on a slow mark and overruns the
queue.

Param4 of the same commands is a delay in microseconds applied at the end point,
and the vendor software uses it for its four timing controls:

| Param4 site | LightBurn control |
|---|---|
| every polygon corner of a lit run | polygon TC |
| last vector of a shape | laser off TC |
| a zero-length `0x0241` placed before the first mark | jump delay |

Laser on TC is separate: it rides in `0x0208` Param1, once per job header.
BslApp names the same four controls Opening Delay, End Delay, Corner Delay and
Jump Position Delay.

BslApp's own numbers came out of a capture the same way: corner 80 us, end
100 us, opening 0 us, jump position delay 500 us, and its marks resolved to one
speed across two segment lengths to 0.01%, a third host obeying the duration
rule.
End TC, max jump delay and jump distance limit never appear on the wire at all.
The vendor host folds them into the Param4 numbers it emits, so a driver that
wants that behaviour has to compute it the same way.

`0x0211` Param2 sat at 8000 across captures with different speeds and lenses,
so it carries no per-job speed.

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
you that *some* SGIN asserted, while which one stays hidden. Per-fault handling
requires reading the lines outside this board. SGIN3 is absent from the status
reply entirely.

**`guard()` is a status poll, and hardware owns the interlock.** It is a USB
poll: ~4 to 8 ms per round trip, so
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
Param1, so it behaves as an index rather than a bitmask. Writing `0x0001` to
Param0 addresses port 0 with level 0 and leaves the pin where it was, which is
what the first attempts did.

**OUT2 and OUT3 are the stepper `DIR` and `PULSE` pins**, owned by
`axis_move()`. Leave them to `axis_move()` while an axis move is running.

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

### FPS - first pulse suppression (CON3 pin 6) - out of reach

Holds at its idle level in every test so far. Exhausted:

- 48 triggered mark-starts with the real config values (`ENFPK=1`, `FPK=40`,
  `OPC_FPKTIME=20`), tick flags `0x0200` / `0x0300`, laser types `0x33`, `0x44`.
- Output-port sweep: every `0x0111` index 0-15 toggled together, so it lives outside the GPIO block.
- `0x0218`, the Q-switch FPK branch of `SendPenPara`, which the first round
  omitted: `Param0 = (qs<<8) | (FPKTime>>8)`, `Param1 = (FPKTime&0xFF)<<8`, `qs` 3
  or 7. Across laser types `0x00`/`0x22`/`0x33`/`0x66`, FPKTime 20 and 40.
- The strings `FPS`, `FirstPulse` and `PulseSuppress` are absent from every
  shipped DLL.

Most likely a board-variant pin that this firmware leaves alone.

### DA1 - analog CO2 power (CON3 pin 15) - out of reach

`j.dac()` leaves the pin at 0 V. `ENPOWERANALOGOUT=0` in `markcfg0`, and
`SendPenPara` only emits the analog command (`0x0207`) for `iLsrType` 0 or 6.
**Every other software on this machine leaves the pin alone too**, so this looks
like a machine-config issue rather than a protocol gap.

### EMSTOP - hardware only

The pin sits at 5 V throughout every test. The command set leaves it alone, it
is absent from every status field, and the host lacks any way to assert it. It
behaves as a hardware interlock line that the host sits outside of.

This matters for safety design: emergency-stop state is readable only in
hardware, so an E-stop has to break the circuit there. See the note on `guard()`
above, which has the same limitation for a different reason.

### Smaller unknowns

- `0x0232` Param0 = 175 in every captured jog. Purpose unknown; it leaves every observed pin where it was.
- `0x1667` appears 269× in the ramp capture and in no opcode table.
- `0x0211` Param3/Param4 carry a 32-bit float lost to `_ftol2_sse`; always sent as 0.
- `laser_port_switch()` (`0x2F84`) and `out_pulse()` (`0x2F82`) are untested.
- SGIN3 is on the connector but in no status field.

### Closed as hardware behaviour

Tickle-during-mark: the board multiplexes the tickle out while marking, and the
other software behaves identically. This is hardware, and it matches vendor
behaviour.

---

## Gotchas

- **The tickle generator is free-running.** It keeps pulsing after the job ends
  *and after the host process exits*. `close()` handles it; a killed process
  leaves it running.
- **The unlock latch is sticky.** Once green it survives everything short of a
  power cycle - a deliberately corrupted digest is ignored silently. Testing the
  unlock path costs one power cycle per experiment.
- **EP 0x02 overrun clears the ready bit.** `ucPara0` drops to `0x0e` and the
  board needs re-arming. Pace against `free_cache()`. Note that `0x0e` also
  appears as the normal executing state in vendor captures, so the overrun case
  is a `0x0e` that stays stuck rather than the value itself.
- **Pausing with `0x0125`** also takes `ucPara0` to `0x0e`.
- **`speed=` is a duration in microseconds**, so one value across segments of
  different length paints them at different feed rates. See Vector timing.

## MOPA pulse width

`0x0206` carries a four byte SPI frame rather than a parameter: `A5 01` then the
width big-endian, clocked out on **P1 (data)** and **P2 (clock)**. The unit is
**nanoseconds**, so `mopa_pulse(100)` puts `A5 01 00 64` on the wire.
Confirmed at 100, 150 and 200 ns.

P1 and P2 are also bits 1 and 2 of the parallel power word, so a power byte
with either set holds them high after the frame, so the clock stays high instead
of returning to idle and the next frame's first byte is mangled. `0x7F` corrupts every frame after
the first, `0x00` gives clean ones. `mopa_pulse()` warns on a colliding power
byte rather than silently emitting a bad frame.

Laser type `0x55` mutes every output on the board tested, so drive a MOPA
source as `FIBER` and set the pulse width. The optical result is unverified.

The board drives **all eight** power bits, including P1 and P2: a power byte of
`0x06`, bits 1 and 2 only, raises both pins with no frame sent at all. It
asserts them from the job header and holds them for the whole job, then shifts
the pulse frame out on top of two of them. So MOPA power is eight bits with two
that collide, and keeping those two clear is on you. That leaves bits 0, 3, 4,
5, 6 and 7, so 64 usable levels.


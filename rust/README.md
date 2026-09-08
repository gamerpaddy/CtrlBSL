# `dbk2jp` in Rust

The same driver as the Python package one directory up, ported to Rust, with
Python bindings so existing scripts keep working. The protocol facts, the
measurements behind them and the safety behaviour are unchanged: see
[API.md](../API.md).

## Why

The bug class this library keeps hitting is a units bug. Param0 is a duration
that reads like a speed, counts against millimetres, microseconds against
48 MHz ticks. Rust lets those be types rather than comments, `Speed::Micros`
against `Speed::MmPerSec`, so the mistake stops compiling. `Drop` also silences
the laser on every path out of a scope, including a panic unwind, which the
Python `atexit` hook could only approximate.

## Build

```bash
cargo build --release          # core library, no dependencies at all
cargo test                     # 22 offline checks, no hardware needed
```

The Windows transport talks to the stock CYUSB3 driver through
`DeviceIoControl`, declared inline, so the default build pulls in nothing.
Linux and macOS use libusb:

```bash
cargo build --release --features libusb
```

## Python

```bash
pip install maturin
maturin develop --release      # builds and installs into the active venv
```

Then:

```python
import dbk2jp_rs as d

with d.Job(laser="co2", field=d.Field(110.0)) as j:
    j.configure(freq_khz=20, power_pct=50)
    j.begin(start=(0x4000, 0x8000), speed=200)
    r = j.path([(0x4000, 0x8000), (0xC000, 0x8000)], mm_s=600, wiggle=60,
               wiggle_pitch=400)
    print(r["exposure"], j.warnings())
```

Long calls detach from the interpreter, so a marking run that sleeps against its
own vector durations leaves other Python threads running. Errors arrive as
`RuntimeError` or `ValueError`, and `warnings()` returns whatever the driver
wanted to tell you since the last call, then clears it.

## Layout

| File | What |
|---|---|
| `src/protocol.rs` | the 12-byte tagSeaCMD frame and power packing |
| `src/usb.rs` | `Board`: framing, the stale-reply fix, endpoint recovery |
| `src/backend_win.rs` | CYUSB3 IOCTLs, registry and SetupAPI enumeration |
| `src/backend_libusb.rs` | bulk transfers everywhere else |
| `src/unlock.rs` | the ATSHA204 replay, three frames of it |
| `src/laser.rs` | laser types and how each is driven |
| `src/field.rs` | millimetres to counts, markcfg0 |
| `src/job.rs` | the high-level API, streaming, wiggle, safety |
| `src/python.rs` | the PyO3 wrapper, and the only file that knows about Python |
| `tests/streaming.rs` | geometry and timing against a fake transport |

## Status

Every offline check passes and the frames match the Python implementation
byte for byte, including field conversions and device enumeration against the
real board's interface path. Nothing in this port has driven a laser yet, so
treat the first run as a bench test: low power, short vectors, hand on the
supply.

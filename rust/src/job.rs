//! High-level job API.
//!
//! THE RULE THAT MATTERS: session/control commands go on EP 0x06; everything
//! that configures a job goes inline in the EP 0x02 batch ahead of the vectors.
//! The board ACKs parameter commands on EP 0x06 and then silently ignores them.

use crate::field::Field;
use crate::laser::{self, Laser, Power};
use crate::protocol as s;
use crate::unlock;
use crate::usb::{Board, Result};
use std::thread::sleep;
use std::time::{Duration, Instant};

/// Conservative per-segment cost, seconds. Used by `lines()`.
pub const SEG_TIME: f64 = 0.00083;
/// Measured floor: ~32 800 segments/s with correct pacing.
pub const SEG_MIN: f64 = 0.00003;
/// Commands per EP 0x02 transfer (the board's flush limit is 1920).
pub const MAX_SEGS: usize = 1400;
pub const CENTRE: u16 = 0x8000;
pub const FULL: i64 = 0xFFFF;

/// Speed for a vector: either the raw Param0 duration or a feed rate.
///
/// Param0 is the time the board takes over one segment, in microseconds, so a
/// fixed value across mixed lengths gives mixed speeds. A feed rate is the
/// honest way to paint unequal segments evenly.
#[derive(Clone, Copy, Debug)]
pub enum Speed {
    /// Raw Param0, microseconds per vector.
    Micros(u16),
    /// Millimetres per second, converted per segment through the field.
    MmPerSec(f64),
}

/// What a batch cost and what it bought.
#[derive(Clone, Copy, Debug, Default)]
pub struct Emitted {
    pub commands: usize,
    pub us: u64,
    /// Run-up actually granted, in counts, after trimming to the travel limits.
    pub overshoot: i64,
    /// Traced lit length over straight lit length: how many times the beam
    /// covers the cut line, which is what a wiggle is bought for.
    pub exposure: f64,
}

/// Machine kinematics, if you know them. Nothing is assumed.
#[derive(Clone, Copy, Debug, Default)]
pub struct Limits {
    pub max_mm_s: Option<f64>,
    pub max_accel_mm_s2: Option<f64>,
    pub max_loop_hz: Option<f64>,
}

/// What a wiggle asks of the mirrors.
#[derive(Clone, Debug)]
pub struct WiggleLoad {
    pub loop_hz: f64,
    pub accel_mm_s2: f64,
    pub accel_g: f64,
    pub exposure: f64,
    pub chord_mm: f64,
    pub vectors_per_s: f64,
    pub exceeded: Vec<String>,
}

/// Options for the streaming methods. Defaults match the vendor hosts.
#[derive(Clone, Copy, Debug)]
pub struct PathOpts {
    pub jump_speed: u16,
    pub jump_delay: u16,
    /// Laser-off dwell, microseconds, on the vector that ends a lit run.
    pub delay: u16,
    /// Dwell at interior vertices. The vendors use tens of microseconds.
    pub corner_delay: u16,
    /// Laser-off run-up in counts, before each lit run and after it.
    pub overshoot: i64,
    /// Wiggle radius in counts. 0 is off.
    pub wiggle: i64,
    /// Counts of travel per full wiggle circle.
    pub wiggle_pitch: i64,
    /// Points per wiggle circle.
    pub wiggle_steps: u32,
    /// Run unlit segments at the marking rate instead of at jump speed.
    pub unlit_at_feed: bool,
}

impl Default for PathOpts {
    fn default() -> Self {
        PathOpts {
            jump_speed: 0x2710,
            jump_delay: 0x01F4,
            delay: 500,
            corner_delay: 0,
            overshoot: 0,
            wiggle: 0,
            wiggle_pitch: 0,
            wiggle_steps: 16,
            unlit_at_feed: false,
        }
    }
}

/// What a job would put on the wire.
#[derive(Clone, Debug)]
pub struct Settings {
    pub laser: String,
    pub code: u8,
    /// The frequency the board actually produces: the divider is an N+1 counter.
    pub freq_khz: f64,
    pub power_pct: f64,
    pub power_byte: u8,
    pub mopa_pulse: Option<u32>,
    pub mo: bool,
    pub tickle: bool,
    pub tick_khz: Option<f64>,
    pub tick_us: Option<f64>,
    pub verified: bool,
}

pub struct Job {
    pub board: Board,
    pub field: Field,
    pub laser: Laser,
    pub limits: Limits,
    freq_khz: f64,
    power_pct: f64,
    power_byte: u8,
    mopa_pulse: Option<u32>,
    mo: bool,
    tickle: bool,
    tick_khz: f64,
    tick_us: f64,
    /// Has this job programmed a laser output?
    live: bool,
    closed: bool,
    /// Warnings the caller should see, since Rust has no warnings module.
    pub warnings: Vec<String>,
}

impl Job {
    pub fn new(board: Board, laser_kind: &str, field: Option<Field>) -> std::result::Result<Self, String> {
        let l = laser::get(laser_kind)?;
        Ok(Job {
            board,
            field: field.unwrap_or_default(),
            freq_khz: l.freq_khz,
            power_pct: 50.0,
            power_byte: 0x80,
            mopa_pulse: None,
            mo: false,
            tickle: l.tickle,
            tick_khz: l.tick_khz,
            tick_us: l.tick_us,
            laser: l,
            limits: Limits::default(),
            live: false,
            closed: false,
            warnings: Vec::new(),
        })
    }

    fn warn(&mut self, msg: String) {
        self.warnings.push(msg);
    }

    // ---- configuration --------------------------------------------------

    pub fn select(&mut self, kind: &str) -> std::result::Result<(), String> {
        let l = laser::get(kind)?;
        self.freq_khz = l.freq_khz;
        self.power_pct = 50.0;
        self.power_byte = 0x80;
        self.mopa_pulse = None;
        self.mo = false;
        self.tickle = l.tickle;
        self.tick_khz = l.tick_khz;
        self.tick_us = l.tick_us;
        self.laser = l;
        Ok(())
    }

    pub fn configure(
        &mut self,
        freq_khz: Option<f64>,
        power_pct: Option<f64>,
        power_byte: Option<u8>,
        mopa_pulse: Option<u32>,
        mo: Option<bool>,
        tickle: Option<bool>,
    ) -> std::result::Result<(), String> {
        if let Some(f) = freq_khz {
            let (lo, hi) = self.laser.freq_range;
            if f < lo || f > hi {
                return Err(format!(
                    "{} kHz outside the {} range {}..{} kHz",
                    f, self.laser.name, lo, hi
                ));
            }
            self.freq_khz = f;
        }
        if let Some(p) = power_pct {
            if !(0.0..=100.0).contains(&p) {
                return Err(format!("power {}% outside 0..100", p));
            }
            self.power_pct = p;
        }
        if let Some(b) = power_byte {
            if self.laser.power != Power::Byte {
                return Err(format!("{} has no parallel power word", self.laser.name));
            }
            self.power_byte = b;
        }
        if let Some(ns) = mopa_pulse {
            if self.laser.power != Power::Byte {
                return Err(format!(
                    "{} has no pulse-width setting: it is not a parallel-power laser",
                    self.laser.name
                ));
            }
            self.mopa_pulse = Some(ns);
        }
        if let Some(v) = mo {
            self.mo = v;
        }
        if let Some(v) = tickle {
            if v && !self.laser.tickle {
                return Err(format!("{} has no tickle", self.laser.name));
            }
            self.tickle = v;
        }
        Ok(())
    }

    /// State what the machine can actually do, for the wiggle checks.
    pub fn set_limits(&mut self, l: Limits) {
        if l.max_mm_s.is_some() {
            self.limits.max_mm_s = l.max_mm_s;
        }
        if l.max_accel_mm_s2.is_some() {
            self.limits.max_accel_mm_s2 = l.max_accel_mm_s2;
        }
        if l.max_loop_hz.is_some() {
            self.limits.max_loop_hz = l.max_loop_hz;
        }
    }

    // ---- plumbing -------------------------------------------------------

    fn ask(&mut self, c: [u8; 12]) -> Option<Vec<u8>> {
        self.board.ask(&c, 1200, 3, 0.04)
    }

    pub fn status(&mut self) -> Option<Vec<u8>> {
        self.ask(s::cmd(s::CMD_STATUS, 0, 0, 0, 0, 0))
    }

    /// True once the board is authenticated. Read from 0x0102 byte 7, never
    /// from 0x0101 bit 5, which is a ready flag the reset tail sets with the
    /// LED still red.
    pub fn unlocked(&mut self) -> bool {
        matches!(self.ask(s::cmd(s::CMD_ENCRYPT_STATE, 0, 0, 0, 0, 0)), Some(st) if st.len() > 7 && st[7] == 2)
    }

    /// Reset, then replay the unlock until the board reports authenticated.
    pub fn ensure_unlocked(&mut self, attempts: u32) -> bool {
        for c in [s::CMD_RESET, s::CMD_CLEAR_CACHE, s::CMD_RESET] {
            self.ask(s::cmd(c, 0, 0, 0, 0, 0));
        }
        for _ in 0..attempts.max(1) {
            if self.unlocked() {
                return true;
            }
            unlock::unlock(&mut self.board, None, true);
        }
        if self.unlocked() {
            return true;
        }
        self.warn(format!(
            "board still reports locked after {} unlock attempts: it will accept \
             commands and emit nothing",
            attempts.max(1)
        ));
        false
    }

    const IN_SHIFT: u32 = 8;
    const IN_MASK: u16 = 0xFF00;
    const REMARK_BIT: u16 = 0x0800;
    const RUNNING_BIT: u8 = 0x08;
    const READY_BIT: u8 = 0x20;
    const SGIN_BIT: u8 = 0x02;

    pub fn status_word(&mut self) -> Option<u16> {
        self.status()
            .filter(|st| st.len() > 6)
            .map(|st| ((st[5] as u16) << 8) | st[6] as u16)
    }

    /// Raw input bits, IN0 in bit 0. A bit reads 1 when the pin is idle.
    pub fn inputs(&mut self) -> Option<u8> {
        self.status_word()
            .map(|w| ((w & Self::IN_MASK) >> Self::IN_SHIFT) as u8)
    }

    pub fn input_pin(&mut self, n: u8) -> Option<bool> {
        self.inputs().map(|v| (v >> n) & 1 == 1)
    }

    /// REMARK trigger input, bit 11. True = idle/high.
    pub fn remark(&mut self) -> Option<bool> {
        self.status_word().map(|w| w & Self::REMARK_BIT != 0)
    }

    /// Free queue slots, from the LOW byte of the 0x0101 word: bits 8..15 are
    /// the opto inputs, so the count is 0..256 and reads 189 idle.
    pub fn free_cache(&mut self) -> u16 {
        self.status_word().map(|w| w & !Self::IN_MASK).unwrap_or(0)
    }

    /// Engine started by 0x0104. NOT "still marking": it stays set until reset.
    pub fn running(&mut self) -> Option<bool> {
        self.status()
            .filter(|st| st.len() > 2)
            .map(|st| st[2] & Self::RUNNING_BIT != 0)
    }

    /// Board reset and online. Reads false mid-mark: the same bit clears while
    /// the queue executes. See `busy`.
    pub fn armed(&mut self) -> Option<bool> {
        self.status()
            .filter(|st| st.len() > 2)
            .map(|st| st[2] & Self::READY_BIT != 0)
    }

    /// True while the vector queue is still executing.
    ///
    /// Both vendor hosts clear 0x0101 byte 2 bit 5 for exactly the duration of
    /// a mark and set it again when the queue drains, over jobs from 34 ms to
    /// 2.5 s. Read from captures rather than measured here.
    pub fn busy(&mut self) -> Option<bool> {
        self.status()
            .filter(|st| st.len() > 2)
            .map(|st| st[2] & Self::READY_BIT == 0)
    }

    /// Block until `busy()` goes false. False on timeout, never an error: on a
    /// board that keeps the bit clear this costs a wait and nothing else.
    pub fn wait_idle(&mut self, timeout_s: f64, poll_s: f64) -> bool {
        let t0 = Instant::now();
        while t0.elapsed().as_secs_f64() < timeout_s {
            if self.busy() == Some(false) {
                return true;
            }
            sleep(Duration::from_secs_f64(poll_s));
        }
        false
    }

    /// Laser status input (SGIN). True = OK, False = fault asserted.
    /// SGIN0..2 all drive the same bit, so treat any assertion as a stop.
    pub fn sgin(&mut self) -> Option<bool> {
        self.status()
            .filter(|st| st.len() > 2)
            .map(|st| st[2] & Self::SGIN_BIT != 0)
    }

    // ---- outputs --------------------------------------------------------

    /// Set output port `port` to `value`. 0x0111 takes the index in the HIGH
    /// byte of Param0 and the level in Param1, and goes on EP 0x06.
    pub fn out(&mut self, port: u8, value: u16) -> Option<Vec<u8>> {
        self.ask(s::cmd(s::CMD_PORT_OUT, (port as u16) << 8, value, 0, 0, 0))
    }

    pub fn out_state(&mut self) -> Option<Vec<u8>> {
        self.ask(s::cmd(s::CMD_PORT_OUT_STATE, 0, 0, 0, 0, 0))
    }

    // ---- job header -----------------------------------------------------

    fn header(&mut self) -> Vec<u8> {
        let pwr = if self.laser.power == Power::Byte {
            s::set_power_raw(self.freq_khz, self.power_byte, 0, self.power_pct)
        } else {
            s::set_power_0210(self.freq_khz, self.power_pct, 0).0
        };
        let tperiod = (s::FPGA_CLK_KHZ / self.tick_khz).round() as u16;
        let twidth = (self.tick_us * 48.0).round() as u16;
        let mut h = Vec::new();
        h.extend_from_slice(&s::cmd(
            s::CMD_LASER_TYPE,
            (self.laser.code as u16) << 8,
            if self.mo { s::MO_ENABLE } else { 0 },
            0,
            0,
            0,
        ));
        h.extend_from_slice(&pwr);
        if let Some(ns) = self.mopa_pulse {
            if self.power_byte & s::MOPA_SPI_MASK != 0 {
                self.warn(format!(
                    "power byte 0x{:02X} has bit 1 or 2 set, and those are the MOPA SPI \
                     lines: every frame after the first will be mangled",
                    self.power_byte
                ));
            }
            h.extend_from_slice(&s::mopa_pulse_ns(ns));
        }
        h.extend_from_slice(&s::cmd(
            s::CMD_TICK,
            if self.tickle { 0x0100 } else { 0 },
            tperiod,
            twidth,
            0,
            0,
        ));
        h.extend_from_slice(&s::cmd(s::CMD_LASER_GATE, 0, 0, 0, 0, 0));
        self.live = true;
        h
    }

    fn arm(&mut self) {
        for c in [s::CMD_CLEAR_CACHE, s::CMD_RESET, s::CMD_RUN] {
            self.ask(s::cmd(c, 0, 0, 0, 0, 0));
        }
    }

    /// Reset, arm, and push the parameter header plus the opening jump.
    pub fn begin(&mut self, start: (u16, u16), speed: u16) -> Result<()> {
        self.arm();
        let mut blob = self.header();
        blob.extend_from_slice(&s::cmd(s::CMD_JUMP, speed, start.0, start.1, 0, 500));
        self.live = true;
        self.board.write_data(&blob)?;
        Ok(())
    }

    /// Emit lit vectors through `points`, paced to the board.
    ///
    /// `speed` is the per-segment duration in microseconds, so the pace has to
    /// follow it: pacing on a fixed per-segment cost alone under-sleeps on a
    /// slow mark and overruns the queue.
    pub fn lines(&mut self, points: &[(u16, u16)], speed: u16, delay: u16) -> Result<()> {
        let per_seg = SEG_TIME.max(speed as f64 * 1e-6);
        for chunk in points.chunks(MAX_SEGS) {
            let mut blob = Vec::with_capacity(chunk.len() * 12);
            for (x, y) in chunk {
                blob.extend_from_slice(&s::cmd(s::CMD_MARK, speed, *x, *y, 0, delay));
            }
            self.board.write_data(&blob)?;
            sleep(Duration::from_secs_f64(chunk.len() as f64 * per_seg * 0.92));
        }
        self.live = true;
        Ok(())
    }

    /// Unlit move. 0x8000 is centre, full span 0x0000..0xFFFF.
    pub fn jump(&mut self, x: u16, y: u16, speed: u16, delay: u16) -> Result<()> {
        self.board.write_data(&s::cmd(s::CMD_JUMP, speed, x, y, 0, delay))?;
        Ok(())
    }

    // ---- millimetres ----------------------------------------------------

    pub fn mm(&self, x_mm: f64, y_mm: f64, clamp: bool) -> std::result::Result<(u16, u16), String> {
        self.field.to_counts(x_mm, y_mm, clamp)
    }

    pub fn where_mm(&self, x: u16, y: u16) -> (f64, f64) {
        self.field.to_mm(x as f64, y as f64)
    }

    /// Millimetres to a count distance, for radii and spacings.
    pub fn counts(&mut self, mm: f64, what: &str) -> i64 {
        if mm == 0.0 {
            return 0;
        }
        let n = (mm / self.field.mm_per_count()).round() as i64;
        if n == 0 && !what.is_empty() {
            self.warn(format!(
                "{} of {} mm is under one galvo count on a {} mm field, so it is ignored",
                what, mm, self.field.size_mm
            ));
        }
        n
    }

    // ---- position streaming ---------------------------------------------
    //
    // One write per batch instead of one per call, and the laser state carried
    // per segment rather than per method. Geometry only: no shapes, no fills.

    fn len_mm(&self, p0: (f64, f64), p1: (f64, f64)) -> f64 {
        let a = self.field.to_mm(p0.0, p0.1);
        let b = self.field.to_mm(p1.0, p1.1);
        ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt()
    }

    fn duration(&self, p0: (f64, f64), p1: (f64, f64), speed: Speed) -> u16 {
        match speed {
            Speed::Micros(us) => us.max(1),
            Speed::MmPerSec(v) => {
                let us = (self.len_mm(p0, p1) / v * 1e6).round();
                us.clamp(1.0, 65535.0) as u16
            }
        }
    }

    fn in_field(pt: (i64, i64)) -> bool {
        (0..=FULL).contains(&pt.0) && (0..=FULL).contains(&pt.1)
    }

    fn require_in_field(pts: &[(i64, i64)], what: &str) -> std::result::Result<(), String> {
        for p in pts {
            if !Self::in_field(*p) {
                return Err(format!(
                    "{} ({}, {}) is outside the galvo travel limits 0..{}",
                    what, p.0, p.1, FULL
                ));
            }
        }
        Ok(())
    }

    /// A point `counts` beyond p0, or past p1, along the p0->p1 line.
    fn lead(p0: (i64, i64), p1: (i64, i64), counts: i64, at_start: bool) -> (i64, i64) {
        let (dx, dy) = ((p1.0 - p0.0) as f64, (p1.1 - p0.1) as f64);
        let d = (dx * dx + dy * dy).sqrt();
        if d == 0.0 || counts <= 0 {
            return if at_start { p0 } else { p1 };
        }
        let (ux, uy) = (dx / d, dy / d);
        let c = counts as f64;
        if at_start {
            (
                (p0.0 as f64 - ux * c).round() as i64,
                (p0.1 as f64 - uy * c).round() as i64,
            )
        } else {
            (
                (p1.0 as f64 + ux * c).round() as i64,
                (p1.1 as f64 + uy * c).round() as i64,
            )
        }
    }

    /// Longest run-up up to `counts` that stays inside the travel limits.
    /// Shrinking beats refusing: a shorter run-up still marks the requested
    /// geometry, while a refusal turns a reachable job into no job.
    fn fit_overshoot(p0: (i64, i64), p1: (i64, i64), counts: i64, at_start: bool) -> ((i64, i64), i64) {
        let (mut lo, mut hi) = (0i64, counts);
        let mut best = (if at_start { p0 } else { p1 }, 0);
        while lo <= hi {
            let mid = (lo + hi) / 2;
            let pt = Self::lead(p0, p1, mid, at_start);
            if Self::in_field(pt) {
                best = (pt, mid);
                lo = mid + 1;
            } else {
                hi = mid - 1;
            }
        }
        best
    }

    /// p0 -> p1 traced as circles around the line, to widen and deepen it.
    ///
    /// The wiggle belongs to a whole lit run, not to one segment: `phase` is
    /// where the previous segment left the circle and `travelled` how far into
    /// the run this segment starts, so a run split into many short segments
    /// gets one continuous spiral rather than a full loop per segment. The
    /// amplitude ramps from zero over the first turn and back over the last, so
    /// the ends of the cut sit on the line instead of starting with a radial
    /// dart of one radius.
    fn wiggle_points(
        p0: (i64, i64),
        p1: (i64, i64),
        radius: f64,
        pitch: f64,
        steps: u32,
        phase: f64,
        travelled: f64,
        run_len: f64,
    ) -> (Vec<(f64, f64)>, f64) {
        let (dx, dy) = ((p1.0 - p0.0) as f64, (p1.1 - p0.1) as f64);
        let length = (dx * dx + dy * dy).sqrt();
        if length == 0.0 || radius <= 0.0 {
            return (vec![(p1.0 as f64, p1.1 as f64)], phase);
        }
        let (ux, uy) = (dx / length, dy / length);
        let turns = length / pitch;
        let n = ((turns * steps as f64).round() as usize).max(2);

        let amp = |d: f64| -> f64 {
            if run_len <= 0.0 {
                return radius;
            }
            let ramp = pitch.min(run_len / 2.0);
            if ramp <= 0.0 {
                return radius;
            }
            let head = (d / ramp).min(1.0);
            let tail = ((run_len - d) / ramp).clamp(0.0, 1.0);
            radius * head.min(tail)
        };
        let at = |f: f64| -> (f64, f64) {
            let d = travelled + length * f;
            let a = phase + 2.0 * std::f64::consts::PI * turns * f;
            let r = amp(d);
            (
                p0.0 as f64 + ux * length * f + r * a.cos(),
                p0.1 as f64 + uy * length * f + r * a.sin(),
            )
        };

        // Sampling the loop at even parameter steps bunches the points where the
        // curve doubles back: one step covers a couple of counts and the next
        // covers fifty, which quantises badly and sets the dose by accident
        // rather than by the pitch. Resample by arc length instead.
        let dense = (n * 8).max(64);
        let pts: Vec<(f64, f64)> = (0..=dense).map(|i| at(i as f64 / dense as f64)).collect();
        let mut cum = Vec::with_capacity(pts.len());
        let mut acc = 0.0;
        cum.push(0.0);
        for w in pts.windows(2) {
            acc += ((w[1].0 - w[0].0).powi(2) + (w[1].1 - w[0].1).powi(2)).sqrt();
            cum.push(acc);
        }
        let total = if acc == 0.0 { 1.0 } else { acc };

        let mut out = Vec::with_capacity(n);
        let mut j = 0usize;
        for i in 1..=n {
            let want = total * i as f64 / n as f64;
            while j + 2 < cum.len() && cum[j + 1] < want {
                j += 1;
            }
            let span = cum[j + 1] - cum[j];
            let t = if span <= 0.0 { 0.0 } else { (want - cum[j]) / span };
            let (ax, ay) = pts[j];
            let (bx, by) = pts[j + 1];
            out.push((ax + (bx - ax) * t, ay + (by - ay) * t));
        }
        let last = out.len() - 1;
        out[last] = at(1.0);
        (out, phase + 2.0 * std::f64::consts::PI * turns)
    }

    /// What a wiggle asks of the mirrors, before you cut anything.
    ///
    /// A circle of radius r walked at v needs v^2 / r sideways, continuously.
    /// 0.1 mm at 600 mm/s is 3.6e6 mm/s^2, some 367 g, which no galvo follows:
    /// what comes out instead is a smoothed, smaller loop with the dwell piling
    /// up wherever the servo reverses.
    pub fn wiggle_load(
        &self,
        radius_mm: f64,
        pitch_mm: f64,
        mm_s: f64,
        steps: u32,
    ) -> std::result::Result<WiggleLoad, String> {
        if radius_mm <= 0.0 || pitch_mm <= 0.0 || mm_s <= 0.0 {
            return Err("radius, pitch and mm_s must all be positive".into());
        }
        let steps = steps.max(4) as f64;
        let traced_per_turn =
            ((2.0 * std::f64::consts::PI * radius_mm).powi(2) + pitch_mm * pitch_mm).sqrt();
        let mut out = WiggleLoad {
            loop_hz: mm_s / pitch_mm,
            accel_mm_s2: mm_s * mm_s / radius_mm,
            accel_g: mm_s * mm_s / radius_mm / 9810.0,
            exposure: traced_per_turn / pitch_mm,
            chord_mm: traced_per_turn / steps,
            vectors_per_s: mm_s * steps / pitch_mm,
            exceeded: Vec::new(),
        };
        if let Some(v) = self.limits.max_mm_s {
            if mm_s > v {
                out.exceeded
                    .push(format!("feed {} mm/s over the {} limit", mm_s, v));
            }
        }
        if let Some(a) = self.limits.max_accel_mm_s2 {
            if out.accel_mm_s2 > a {
                out.exceeded.push(format!(
                    "needs {:.3e} mm/s^2 lateral, limit {:.3e}",
                    out.accel_mm_s2, a
                ));
            }
        }
        if let Some(hz) = self.limits.max_loop_hz {
            if out.loop_hz > hz {
                out.exceeded
                    .push(format!("{:.0} loops/s over the {:.0} limit", out.loop_hz, hz));
            }
        }
        if out.vectors_per_s > 1.0 / SEG_MIN {
            out.exceeded.push(format!(
                "{:.0} vectors/s over the board's measured {:.0}",
                out.vectors_per_s,
                1.0 / SEG_MIN
            ));
        }
        Ok(out)
    }

    /// Run-up length that actually reaches `mm_s`, as v^2 / 2a.
    pub fn runup_mm(&self, mm_s: f64, accel_mm_s2: Option<f64>) -> std::result::Result<f64, String> {
        let a = accel_mm_s2
            .or(self.limits.max_accel_mm_s2)
            .ok_or("no acceleration known: pass one or set limits")?;
        Ok(mm_s * mm_s / (2.0 * a))
    }

    fn warn_wiggle_load(&mut self, wig: i64, pitch: i64, steps: u32, mm_s: f64) {
        let mpc = self.field.mm_per_count();
        if let Ok(load) = self.wiggle_load(wig as f64 * mpc, pitch as f64 * mpc, mm_s, steps) {
            if !load.exceeded.is_empty() {
                self.warn(format!(
                    "wiggle asks for {:.0} loops/s and {:.3e} mm/s^2 ({:.0} g) of lateral \
                     acceleration at {:.0} mm/s: {}. The mirrors will round the loops off and \
                     the exposure will bunch at the turns instead of spreading along the cut.",
                    load.loop_hz,
                    load.accel_mm_s2,
                    load.accel_g,
                    mm_s,
                    load.exceeded.join("; ")
                ));
            }
        }
    }

    fn emit(&mut self, cmds: &[([u8; 12], u32)]) -> Result<u64> {
        // The floor here is the measured host throughput, not the conservative
        // 0.83 ms `lines()` uses: these callers built every Param0, so the sum
        // is the real pace.
        let mut total: u64 = 0;
        for chunk in cmds.chunks(MAX_SEGS) {
            let us: u64 = chunk.iter().map(|(_, d)| *d as u64).sum();
            total += us;
            let mut blob = Vec::with_capacity(chunk.len() * 12);
            for (c, _) in chunk {
                blob.extend_from_slice(c);
            }
            self.board.write_data(&blob)?;
            let secs = (chunk.len() as f64 * SEG_MIN).max(us as f64 * 1e-6) * 0.92;
            sleep(Duration::from_secs_f64(secs));
        }
        self.live = true;
        Ok(total)
    }

    /// Stream a run of points with the laser on or off per segment.
    ///
    /// `lit` is one boolean per segment, so segment i runs points[i] ->
    /// points[i+1] as 0x0243 when lit and 0x0241 when not. Unlit segments
    /// travel at `jump_speed`, since they are traverses between pieces of work.
    ///
    /// The path is kinematically ideal: Param0 is a duration the board
    /// interpolates, and nothing here knows whether the mirrors kept up. See
    /// `wiggle_load`.
    pub fn path(
        &mut self,
        points: &[(u16, u16)],
        lit: Option<&[bool]>,
        speed: Speed,
        o: PathOpts,
    ) -> std::result::Result<Emitted, String> {
        let pts: Vec<(i64, i64)> = points.iter().map(|p| (p.0 as i64, p.1 as i64)).collect();
        if pts.len() < 2 {
            return Err("path needs at least two points".into());
        }
        let flags: Vec<bool> = match lit {
            Some(v) => v.to_vec(),
            None => vec![true; pts.len() - 1],
        };
        if flags.len() != pts.len() - 1 {
            return Err(format!(
                "lit needs one flag per segment: {} for {} points",
                pts.len() - 1,
                pts.len()
            ));
        }
        Self::require_in_field(&pts, "point")?;

        let over = o.overshoot.max(0);
        let wig = o.wiggle.max(0);
        if wig > 0 {
            if o.wiggle_pitch <= 0 {
                return Err("wiggle needs a positive wiggle_pitch (counts per circle)".into());
            }
            if o.wiggle_steps < 4 {
                return Err("wiggle_steps must be at least 4".into());
            }
            if let Speed::MmPerSec(v) = speed {
                self.warn_wiggle_load(wig, o.wiggle_pitch, o.wiggle_steps, v);
            }
        }

        // A wiggle belongs to a lit run, so measure the runs first: their total
        // length drives the amplitude ramp and the phase carries across the
        // segments inside one.
        let n = flags.len();
        let (mut run_len, mut run_at) = (vec![0.0; n], vec![0.0; n]);
        let mut i = 0;
        while i < n {
            if !flags[i] {
                i += 1;
                continue;
            }
            let mut k = i;
            let mut acc = Vec::new();
            while k < n && flags[k] {
                let d = (((pts[k + 1].0 - pts[k].0) as f64).powi(2)
                    + ((pts[k + 1].1 - pts[k].1) as f64).powi(2))
                .sqrt();
                acc.push(d);
                k += 1;
            }
            let total: f64 = acc.iter().sum();
            let mut walked = 0.0;
            for (m, d) in acc.iter().enumerate() {
                run_len[i + m] = total;
                run_at[i + m] = walked;
                walked += d;
            }
            i = k;
        }

        let f = |p: (i64, i64)| (p.0 as f64, p.1 as f64);
        let mut granted = over;
        let mut cmds: Vec<([u8; 12], u32)> = Vec::new();
        let mut at: Option<(i64, i64)> = None;
        let mut phase = 0.0f64;
        let mut warned = false;
        let (mut straight, mut lit_path) = (0.0f64, 0.0f64);

        for i in 0..n {
            let on = flags[i];
            let (p0, p1) = (pts[i], pts[i + 1]);
            let starts_run = on && (i == 0 || !flags[i - 1]);
            let ends_run = on && (i == n - 1 || !flags[i + 1]);

            if starts_run && over > 0 {
                let (lead, got) = Self::fit_overshoot(p0, p1, over, true);
                granted = granted.min(got);
                cmds.push((
                    s::cmd(s::CMD_JUMP, o.jump_speed, lead.0 as u16, lead.1 as u16, 0, o.jump_delay),
                    o.jump_speed as u32,
                ));
                if got > 0 {
                    let us = self.duration(f(lead), f(p0), speed);
                    cmds.push((s::cmd(s::CMD_JUMP, us, p0.0 as u16, p0.1 as u16, 0, 0), us as u32));
                }
                // Every branch below sets `at` to the segment end, so there is
                // nothing to record here.
            } else if at != Some(p0) {
                cmds.push((
                    s::cmd(s::CMD_JUMP, o.jump_speed, p0.0 as u16, p0.1 as u16, 0, o.jump_delay),
                    o.jump_speed as u32,
                ));
            }

            if on && wig > 0 {
                if starts_run {
                    phase = 0.0;
                }
                let (mut curve, next_phase) = Self::wiggle_points(
                    p0,
                    p1,
                    wig as f64,
                    o.wiggle_pitch as f64,
                    o.wiggle_steps,
                    phase,
                    run_at[i],
                    run_len[i],
                );
                phase = next_phase;
                if ends_run {
                    let last = curve.len() - 1;
                    curve[last] = (p1.0 as f64, p1.1 as f64);
                }
                let chain: Vec<(i64, i64)> = curve
                    .iter()
                    .map(|(x, y)| (x.round() as i64, y.round() as i64))
                    .collect();
                Self::require_in_field(&chain, "wiggle point")?;
                // Time the rounded points, not the ideal curve: those integers
                // are the only thing the board ever moves between.
                let mut prev = p0;
                let mut subs: Vec<u16> = Vec::with_capacity(chain.len());
                let mut lens: Vec<f64> = Vec::with_capacity(chain.len());
                for c in &chain {
                    lens.push(self.len_mm(f(prev), f(*c)));
                    prev = *c;
                }
                match speed {
                    Speed::MmPerSec(_) => {
                        let mut prev = p0;
                        for c in &chain {
                            subs.push(self.duration(f(prev), f(*c), speed));
                            prev = *c;
                        }
                    }
                    Speed::Micros(us) => {
                        // Split the requested duration along the traced path, so
                        // a wiggled segment still takes the time asked for.
                        let tot: f64 = lens.iter().sum::<f64>().max(f64::MIN_POSITIVE);
                        for l in &lens {
                            subs.push(((us as f64 * l / tot).round() as i64).clamp(1, 65535) as u16);
                        }
                    }
                }
                let last = chain.len() - 1;
                for (k, (c, sub)) in chain.iter().zip(subs.iter()).enumerate() {
                    // Param4 is a dwell at the end point. Inside a wiggle every
                    // point is an interior point, so it stays 0.
                    let p4 = if k == last && ends_run { o.delay } else { 0 };
                    cmds.push((s::cmd(s::CMD_MARK, *sub, c.0 as u16, c.1 as u16, 0, p4), *sub as u32));
                }
                let seg_straight = self.len_mm(f(p0), f(p1));
                let seg_traced: f64 = lens.iter().sum();
                straight += seg_straight;
                lit_path += seg_traced;
                if let Speed::Micros(_) = speed {
                    if !warned && seg_straight > 0.0 {
                        let us: f64 = subs.iter().map(|v| *v as f64).sum::<f64>() * 1e-6;
                        if us > 0.0 {
                            self.warn_wiggle_load(
                                wig,
                                o.wiggle_pitch,
                                o.wiggle_steps,
                                seg_traced / us,
                            );
                        }
                        warned = true;
                    }
                }
                at = Some(p1);
            } else if on {
                let us = self.duration(f(p0), f(p1), speed);
                let p4 = if ends_run { o.delay } else { o.corner_delay };
                cmds.push((s::cmd(s::CMD_MARK, us, p1.0 as u16, p1.1 as u16, 0, p4), us as u32));
                straight += self.len_mm(f(p0), f(p1));
                lit_path += self.len_mm(f(p0), f(p1));
                at = Some(p1);
            } else {
                // An unlit leg is a move between two pieces of work, so it runs
                // at jump speed: timing it at the marking feed rate spends the
                // whole traverse at cutting speed.
                let us = if o.unlit_at_feed {
                    self.duration(f(p0), f(p1), speed)
                } else {
                    o.jump_speed.max(1)
                };
                let p4 = if o.unlit_at_feed { 0 } else { o.jump_delay };
                cmds.push((s::cmd(s::CMD_JUMP, us, p1.0 as u16, p1.1 as u16, 0, p4), us as u32));
                at = Some(p1);
            }

            if ends_run && over > 0 {
                let (out, got) = Self::fit_overshoot(p0, p1, over, false);
                granted = granted.min(got);
                if got > 0 {
                    let us = self.duration(f(p1), f(out), speed);
                    cmds.push((s::cmd(s::CMD_JUMP, us, out.0 as u16, out.1 as u16, 0, 0), us as u32));
                    at = Some(out);
                }
            }
        }

        let count = cmds.len();
        let total = self.emit(&cmds).map_err(|e| e.to_string())?;
        Ok(Emitted {
            commands: count,
            us: total,
            overshoot: if over > 0 { granted } else { 0 },
            exposure: if straight > 0.0 { lit_path / straight } else { 1.0 },
        })
    }

    /// Mark disjoint segments, each with its own jump, run-up and run-out, all
    /// in one batch: n segments cost one write rather than 2n calls.
    pub fn segments(
        &mut self,
        segs: &[((u16, u16), (u16, u16))],
        speed: Speed,
        o: PathOpts,
    ) -> std::result::Result<Emitted, String> {
        if segs.is_empty() {
            return Err("no segments given".into());
        }
        let mut pts = Vec::new();
        let mut flags = Vec::new();
        for (a, b) in segs {
            if !pts.is_empty() {
                pts.push(*a);
                flags.push(false); // the connecting jump
            } else {
                pts.push(*a);
            }
            pts.push(*b);
            flags.push(true);
        }
        self.path(&pts, Some(&flags), speed, o)
    }

    /// Point marking: jump to each point and fire for `dwell_us`. UNTESTED.
    ///
    /// Emitted as a zero-length 0x0243 whose Param0 is the dwell, the shape both
    /// vendor hosts use for a settle. Whether the board honours a zero-length
    /// lit vector as a timed dot has not been measured here.
    pub fn dots(
        &mut self,
        points: &[(u16, u16)],
        dwell_us: u16,
        o: PathOpts,
    ) -> std::result::Result<Emitted, String> {
        let pts: Vec<(i64, i64)> = points.iter().map(|p| (p.0 as i64, p.1 as i64)).collect();
        Self::require_in_field(&pts, "point")?;
        let dwell = dwell_us.max(1);
        let mut cmds = Vec::with_capacity(points.len() * 2);
        for (x, y) in points {
            cmds.push((
                s::cmd(s::CMD_JUMP, o.jump_speed, *x, *y, 0, o.jump_delay),
                o.jump_speed as u32,
            ));
            cmds.push((s::cmd(s::CMD_MARK, dwell, *x, *y, 0, 0), dwell as u32));
        }
        let count = cmds.len();
        let total = self.emit(&cmds).map_err(|e| e.to_string())?;
        Ok(Emitted {
            commands: count,
            us: total,
            overshoot: 0,
            exposure: 1.0,
        })
    }

    // ---- laser hardware -------------------------------------------------

    /// MO and PA enable, 0x0211 Param1 bit 8. Takes effect on the next header.
    pub fn mo(&mut self, on: bool) -> bool {
        self.mo = on;
        on
    }

    /// MOPA pulse width in nanoseconds, sent as an SPI frame on P1 and P2.
    pub fn mopa_pulse(&mut self, ns: u32) -> Result<()> {
        self.arm();
        let mut blob = self.header();
        blob.extend_from_slice(&s::mopa_pulse_ns(ns));
        self.mopa_pulse = Some(ns);
        self.board.write_data(&blob)?;
        Ok(())
    }

    /// Fiber parallel power word P0..P7. WARNING: this puts a live signal on
    /// the laser control output and leaves it there until laser_off().
    pub fn power_byte(&mut self, value: u8, freq_khz: Option<f64>) -> Result<u8> {
        let f = freq_khz.unwrap_or(self.freq_khz);
        self.arm();
        let mut blob = Vec::new();
        blob.extend_from_slice(&s::cmd(
            s::CMD_LASER_TYPE,
            (self.laser.code as u16) << 8,
            0,
            0,
            0,
            0,
        ));
        blob.extend_from_slice(&s::set_power_raw(f, value, 0, 0.0));
        blob.extend_from_slice(&s::cmd(s::CMD_LASER_GATE, 0, 0, 0, 0, 0));
        self.live = true;
        self.board.write_data(&blob)?;
        Ok(value)
    }

    /// Pilot / red pointer. The low byte of 0x0211 Param0 selects it: 0x22 is
    /// the pointer, 0x00 normal marking, with the laser type in the high byte.
    /// It latches, so this is a static on/off.
    pub fn red_light(&mut self, on: bool) -> Result<bool> {
        self.arm();
        let p0 = ((self.laser.code as u16) << 8) | if on { 0x22 } else { 0x00 };
        let pwr = s::set_power_0210(self.freq_khz, self.power_pct, 0).0;
        let mut blob = Vec::new();
        blob.extend_from_slice(&s::cmd(s::CMD_LASER_TYPE, p0, 0, 0, 0, 0));
        blob.extend_from_slice(&pwr);
        blob.extend_from_slice(&s::cmd(
            s::CMD_TICK,
            if self.tickle { 0x0100 } else { 0 },
            (s::FPGA_CLK_KHZ / self.tick_khz).round() as u16,
            (self.tick_us * 48.0).round() as u16,
            0,
            0,
        ));
        blob.extend_from_slice(&s::cmd(s::CMD_LASER_GATE, 0, 0, 0, 0, 0));
        self.board.write_data(&blob)?;
        Ok(on)
    }

    /// Set the tickle shape. Frequency and width are independent, and the
    /// generator is FREE-RUNNING: it survives job end and host exit.
    pub fn tick(&mut self, freq_khz: Option<f64>, width_us: Option<f64>, enable: Option<bool>) -> std::result::Result<(f64, u16, f64), String> {
        let f = freq_khz.unwrap_or(self.tick_khz);
        let w = width_us.unwrap_or(self.tick_us);
        let (lo, hi) = self.laser.tick_range;
        if f < lo || f > hi {
            return Err(format!("tickle frequency {} kHz outside {}..{} kHz", f, lo, hi));
        }
        let period_us = 1000.0 / f;
        if w <= 0.0 || w >= period_us {
            return Err(format!(
                "tickle width {} us must be >0 and shorter than the {:.1} us period",
                w, period_us
            ));
        }
        self.tick_khz = f;
        self.tick_us = w;
        if let Some(e) = enable {
            self.tickle = e;
        }
        let period = (s::FPGA_CLK_KHZ / f).round();
        let ticks = (w * 48.0).round() as u16;
        Ok((48e6 / (period + 1.0), ticks, 100.0 * w / period_us))
    }

    /// Stepper axis move. Speed is pulses per second directly, the move
    /// self-terminates after `pulses`, and DIR is 0x0230 Param4 bit 0x100.
    /// Returns the expected duration in seconds.
    pub fn axis_move(
        &mut self,
        pulses: u32,
        pps: u32,
        direction: bool,
        flags: u16,
        min_pps: Option<u32>,
        acctime: u16,
        p232: u16,
    ) -> Result<f64> {
        self.arm();
        let mut head = Vec::new();
        head.extend_from_slice(&s::cmd(
            s::CMD_LASER_TYPE,
            (self.laser.code as u16) << 8,
            0,
            0,
            0,
            0,
        ));
        head.extend_from_slice(&s::set_power_raw(self.freq_khz, 0x80, 0, 0.0));
        head.extend_from_slice(&s::cmd(s::CMD_LASER_GATE, 0, 0, 0, 0, 0));
        self.board.write_data(&head)?;
        let mn = min_pps.unwrap_or(pps);
        let p4 = flags | if direction { 0x100 } else { 0 };
        let mut blob = Vec::new();
        blob.extend_from_slice(&s::cmd(
            s::CMD_AXIS_COUNT,
            (pulses >> 16) as u16,
            (pulses & 0xFFFF) as u16,
            0,
            0,
            p4,
        ));
        blob.extend_from_slice(&s::cmd(
            s::CMD_AXIS_RATE,
            (mn & 0xFFFF) as u16,
            (pps & 0xFFFF) as u16,
            (acctime & 0xFF) << 8,
            0,
            0,
        ));
        blob.extend_from_slice(&s::cmd(s::CMD_AXIS_P232, p232, 0, 0, 0, 0));
        blob.extend_from_slice(&s::cmd(s::CMD_AXIS_GO, 0, 0, 0, 0, 0));
        self.board.write_data(&blob)?;
        Ok(if pps > 0 { pulses as f64 / pps as f64 } else { 0.0 })
    }

    /// What would go on the wire, as a settings snapshot.
    pub fn settings(&self) -> Settings {
        let period = (s::FPGA_CLK_KHZ / self.freq_khz).round();
        Settings {
            laser: self.laser.name.to_string(),
            code: self.laser.code,
            freq_khz: 48e3 / (period + 1.0), // N+1 counter, so this is the real output
            power_pct: self.power_pct,
            power_byte: self.power_byte,
            mopa_pulse: self.mopa_pulse,
            mo: self.mo,
            tickle: self.tickle,
            tick_khz: if self.laser.tickle { Some(self.tick_khz) } else { None },
            tick_us: if self.laser.tickle { Some(self.tick_us) } else { None },
            verified: self.laser.verified,
        }
    }

    /// Stop the tickle generator.
    ///
    /// It is free-running: it keeps pulsing after a job ends and after the host
    /// process exits, so it has to be switched off explicitly, and like every
    /// other parameter it travels the EP 0x02 batch path.
    pub fn tick_off(&mut self) -> Result<()> {
        self.tickle = false;
        self.arm();
        let mut blob = Vec::new();
        blob.extend_from_slice(&s::cmd(
            s::CMD_TICK,
            0,
            (s::FPGA_CLK_KHZ / self.tick_khz).round() as u16,
            (self.tick_us * 48.0).round() as u16,
            0,
            0,
        ));
        blob.extend_from_slice(&s::cmd(s::CMD_JUMP, 200, 0x4000, CENTRE, 0, 500));
        self.board.write_data(&blob)?;
        sleep(Duration::from_millis(50));
        self.ask(s::cmd(s::CMD_RESET, 0, 0, 0, 0, 0));
        Ok(())
    }

    /// Sustained laser PWM for scope work, closed-loop against the board's own
    /// free-cache counter. Open-loop pacing drains the queue between chunks and
    /// the output visibly drops back to tickle-only about once a second.
    /// Returns the number of vectors sent.
    pub fn pwm_burst(&mut self, seconds: f64, speed: u16, span: (u16, u16), margin: u16) -> Result<usize> {
        self.begin((span.0, CENTRE), speed)?;
        let t0 = Instant::now();
        let mut n = 0usize;
        while t0.elapsed().as_secs_f64() < seconds {
            let free = self.free_cache();
            let want = (free.saturating_sub(margin) as usize).min(MAX_SEGS);
            if want < 64 {
                sleep(Duration::from_millis(10)); // queue full enough, let it drain
                continue;
            }
            let mut blob = Vec::with_capacity(want * 12);
            for i in 0..want {
                let x = if i % 2 == 0 { span.1 } else { span.0 };
                blob.extend_from_slice(&s::cmd(s::CMD_MARK, speed, x, CENTRE, 0, 500));
            }
            self.board.write_data(&blob)?;
            n += want;
        }
        Ok(n)
    }

    /// Analog power out, 0x0207 Param0 = a 12-bit word (CON3 pin 15 / DA1).
    ///
    /// NOT CONFIRMED WORKING: produced no voltage on this board under every
    /// condition tried. The gate is believed to be a board-side analog enable
    /// that no observed command writes.
    pub fn dac(&mut self, value12: u16, mark: bool) -> Result<u16> {
        self.arm();
        let mut blob = Vec::new();
        blob.extend_from_slice(&s::cmd(
            s::CMD_LASER_TYPE,
            (self.laser.code as u16) << 8,
            0,
            0,
            0,
            0,
        ));
        blob.extend_from_slice(&s::set_power_raw(self.freq_khz, 0x80, 0, 0.0));
        blob.extend_from_slice(&s::cmd(0x0207, value12 & 0x0FFF, 0, 0, 0, 0));
        blob.extend_from_slice(&s::cmd(s::CMD_LASER_GATE, 0, 0, 0, 0, 0));
        if mark {
            blob.extend_from_slice(&s::cmd(s::CMD_JUMP, 200, 0x4000, CENTRE, 0, 500));
        }
        self.live = true;
        self.board.write_data(&blob)?;
        Ok(value12 & 0x0FFF)
    }

    /// Timed output pulse via 0x2F82 on EP 0x02, duration scaled by 2000.
    /// UNTESTED.
    pub fn out_pulse(&mut self, port: u8, value: u8, ms: u32) -> Result<()> {
        let ticks = ms * 2000;
        let blob = s::cmd(
            s::CMD_PORT_PULSE,
            ((port as u16) << 8) | if ms > 0 { 1 } else { 0 },
            ((value as u16) << 8) | ((ticks >> 24) & 0xFF) as u16,
            (((ticks >> 8) & 0xFF) | (((ticks >> 16) & 0xFF) << 8)) as u16,
            ((ms as i64 * -0x3000) & 0xFFFF) as u16,
            0,
        );
        self.board.write_data(&blob)?;
        Ok(())
    }

    /// 0x2F84 laser port switch. Purpose not established; a candidate for
    /// routing or enabling the analog out.
    pub fn laser_port_switch(&mut self, p1: u16, p2: u16, p3: u16, p4: u16, p5: u16, p6: u16) -> Result<()> {
        let _ = p2;
        self.arm();
        let blob = s::cmd(
            0x2F84,
            (p1.wrapping_mul(0x100)).wrapping_add(p4),
            p5.wrapping_mul(0x300),
            p3.wrapping_mul(2),
            p6.wrapping_mul(2),
            0,
        );
        self.board.write_data(&blob)?;
        Ok(())
    }

    // ---- safety ---------------------------------------------------------

    /// Silence every laser output: marking PWM, tickle, gate.
    ///
    /// A plain reset does NOT stop the marking PWM generator. Order matters:
    /// zero the generator first, with the engine in whatever state it is in.
    /// Arming before this restarts it with the old values loaded and emits a
    /// burst, which is exactly what this exists to prevent.
    pub fn laser_off(&mut self) {
        let mut blob = Vec::new();
        blob.extend_from_slice(&s::cmd(
            s::CMD_LASER_TYPE,
            (self.laser.code as u16) << 8,
            0,
            0,
            0,
            0,
        ));
        blob.extend_from_slice(&s::cmd(s::CMD_POWER, 0, 0, 0, 0, 0));
        blob.extend_from_slice(&s::cmd(s::CMD_TICK, 0, 0, 0, 0, 0));
        blob.extend_from_slice(&s::cmd(s::CMD_LASER_GATE, 0, 0, 0, 0, 0));
        let first = self.board.write_data(&blob);
        if let Err(e) = first {
            // This write is the one that silences the laser, so a stalled pipe
            // here leaves an output driving. Clear the endpoints and try again.
            self.board.recover();
            if let Err(e2) = self.board.write_data(&blob) {
                self.warn(format!(
                    "could not silence the laser ({}, then {} after recover): an output may \
                     still be driving. Power-cycle the board.",
                    e, e2
                ));
            }
        }
        sleep(Duration::from_millis(50));
        self.tickle = false;
        self.mo = false;
        self.live = false;
        self.ask(s::cmd(s::CMD_CLEAR_CACHE, 0, 0, 0, 0, 0));
        self.ask(s::cmd(s::CMD_RESET, 0, 0, 0, 0, 0));
    }

    /// Stop marking and kill laser output now: gate first, then the queue, then
    /// reset.
    pub fn abort(&mut self) {
        self.ask(s::cmd(s::CMD_LASER_GATE, 0, 0, 0, 0, 0));
        for op in [s::CMD_CLEAR_CACHE, s::CMD_RESET, s::CMD_RUN, s::CMD_RESET] {
            self.ask(s::cmd(op, 0, 0, 0, 0, 0));
        }
        self.tickle = false;
    }

    /// Poll SGIN for `seconds`, aborting the moment it asserts.
    ///
    /// NOT a substitute for a hardware interlock: each round trip costs several
    /// milliseconds, so worst-case reaction is tens of ms.
    pub fn guard(&mut self, seconds: f64, poll_s: f64) -> bool {
        let t0 = Instant::now();
        while t0.elapsed().as_secs_f64() < seconds {
            if self.sgin() == Some(false) {
                self.abort();
                return false;
            }
            sleep(Duration::from_secs_f64(poll_s));
        }
        true
    }

    pub fn stop(&mut self) {
        self.ask(s::cmd(s::CMD_RESET, 0, 0, 0, 0, 0));
    }

    /// Leave the board silent: every laser output off, then reset.
    pub fn close(&mut self, quiet: bool) {
        if quiet && self.live {
            self.laser_off();
        } else {
            self.stop();
        }
        self.closed = true;
        self.board.close();
    }

    pub fn is_live(&self) -> bool {
        self.live
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // Structural version of the Python atexit hook: a job that programmed an
        // output gets silenced however the scope ends, including a panic unwind.
        if !self.closed && self.live {
            self.laser_off();
        }
    }
}


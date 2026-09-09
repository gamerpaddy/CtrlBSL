//! Python bindings.
//!
//! The core crate knows nothing about Python; this file is the whole of the
//! wrapper, so the same Rust drives a CLI, an editor, or `import dbk2jp_rs`.
//!
//! Long calls detach from the interpreter (release the GIL), so a marking run that sleeps against its own
//! vector durations leaves other Python threads running.

use crate::field::Field;
use crate::job::{Job, Limits, PathOpts, Speed};
use crate::usb::Board;
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyDict;

fn err<E: std::fmt::Display>(e: E) -> PyErr {
    PyRuntimeError::new_err(e.to_string())
}

fn speed_of(speed: Option<u16>, mm_s: Option<f64>) -> PyResult<Speed> {
    match (speed, mm_s) {
        (Some(us), None) => Ok(Speed::Micros(us)),
        (None, Some(v)) if v > 0.0 => Ok(Speed::MmPerSec(v)),
        (None, Some(_)) => Err(PyValueError::new_err("mm_s must be positive")),
        _ => Err(PyValueError::new_err(
            "give exactly one of speed= (microseconds) or mm_s= (millimetres per second)",
        )),
    }
}

/// Scan field: millimetres in, 16-bit galvo counts out.
#[pyclass(name = "Field")]
#[derive(Clone)]
pub struct PyField {
    pub inner: Field,
}

#[pymethods]
impl PyField {
    #[new]
    #[pyo3(signature = (size_mm=100.0, offset_mm=(0.0, 0.0), aspect=(100.0, 100.0)))]
    fn new(size_mm: f64, offset_mm: (f64, f64), aspect: (f64, f64)) -> Self {
        PyField {
            inner: Field {
                size_mm,
                offset_mm,
                aspect,
                ..Field::default()
            },
        }
    }

    /// Load a machine's markcfg0, writing a default one first if missing.
    #[staticmethod]
    #[pyo3(signature = (path="markcfg0", size_mm=100.0))]
    fn load_or_create(path: &str, size_mm: f64) -> PyResult<Self> {
        Ok(PyField {
            inner: Field::load_or_create(path, size_mm).map_err(err)?,
        })
    }

    #[staticmethod]
    fn from_markcfg(path: &str) -> PyResult<Self> {
        Ok(PyField {
            inner: Field::from_markcfg(path).map_err(err)?,
        })
    }

    #[pyo3(signature = (x_mm, y_mm, clamp=false))]
    fn to_counts(&self, x_mm: f64, y_mm: f64, clamp: bool) -> PyResult<(u16, u16)> {
        self.inner
            .to_counts(x_mm, y_mm, clamp)
            .map_err(PyValueError::new_err)
    }

    fn to_mm(&self, x: f64, y: f64) -> (f64, f64) {
        self.inner.to_mm(x, y)
    }

    fn contains(&self, x_mm: f64, y_mm: f64) -> bool {
        self.inner.contains(x_mm, y_mm)
    }

    /// Change factors in place, with validation.
    #[pyo3(signature = (size_mm=None, offset_mm=None, aspect=None, negate=None, swap_xy=None))]
    fn set(
        &mut self,
        size_mm: Option<f64>,
        offset_mm: Option<(f64, f64)>,
        aspect: Option<(f64, f64)>,
        negate: Option<(bool, bool)>,
        swap_xy: Option<bool>,
    ) -> PyResult<()> {
        let mut f = self.inner.clone();
        if let Some(v) = size_mm {
            f.size_mm = v;
        }
        if let Some(v) = offset_mm {
            f.offset_mm = v;
        }
        if let Some(v) = aspect {
            f.aspect = v;
        }
        if let Some(v) = negate {
            f.negate = v;
        }
        if let Some(v) = swap_xy {
            f.swap_xy = v;
        }
        f.validate().map_err(PyValueError::new_err)?;
        self.inner = f;
        Ok(())
    }

    /// Write these factors back, preserving keys this library ignores.
    /// Defaults to the file it was loaded from.
    #[pyo3(signature = (path=None))]
    fn save(&self, path: Option<&str>) -> PyResult<()> {
        let target = path
            .map(|p| p.to_string())
            .or_else(|| self.inner.path.clone())
            .ok_or_else(|| PyValueError::new_err("no path: this field was built inline, pass one"))?;
        self.inner.save(&target).map_err(err)
    }

    #[getter]
    fn offset_mm(&self) -> (f64, f64) {
        self.inner.offset_mm
    }

    #[getter]
    fn aspect(&self) -> (f64, f64) {
        self.inner.aspect
    }

    #[getter]
    fn negate(&self) -> (bool, bool) {
        self.inner.negate
    }

    #[getter]
    fn swap_xy(&self) -> bool {
        self.inner.swap_xy
    }

    #[getter]
    fn size_mm(&self) -> f64 {
        self.inner.size_mm
    }

    #[getter]
    fn mm_per_count(&self) -> f64 {
        self.inner.mm_per_count()
    }

    #[getter]
    fn created(&self) -> bool {
        self.inner.created
    }

    fn __repr__(&self) -> String {
        format!(
            "<Field {} mm offset ({}, {}) aspect ({}, {})>",
            self.inner.size_mm,
            self.inner.offset_mm.0,
            self.inner.offset_mm.1,
            self.inner.aspect.0,
            self.inner.aspect.1
        )
    }
}

/// Open transport to one DBK2JP board.
#[pyclass(name = "Board", unsendable)]
pub struct PyBoard {
    pub inner: Option<Board>,
}

#[pymethods]
impl PyBoard {
    #[new]
    #[pyo3(signature = (path=None, index=0))]
    fn new(path: Option<&str>, index: usize) -> PyResult<Self> {
        let b = match path {
            Some(p) => Board::open_path(p),
            None => Board::open(index),
        }
        .map_err(err)?;
        Ok(PyBoard { inner: Some(b) })
    }

    #[getter]
    fn path(&self) -> String {
        self.inner.as_ref().map(|b| b.path()).unwrap_or_default()
    }

    fn recover(&mut self) -> PyResult<()> {
        self.inner.as_mut().ok_or_else(|| err("board closed"))?.recover();
        Ok(())
    }

    fn close(&mut self) {
        if let Some(mut b) = self.inner.take() {
            b.close();
        }
    }
}

/// A session against one board.
#[pyclass(name = "Job", unsendable)]
pub struct PyJob {
    inner: Job,
}

#[pymethods]
impl PyJob {
    /// Open, and unless unlock_now is false, unlock and arm.
    #[new]
    #[pyo3(signature = (laser="co2", index=0, unlock_now=true, field=None, path=None))]
    fn new(
        laser: &str,
        index: usize,
        unlock_now: bool,
        field: Option<PyField>,
        path: Option<&str>,
    ) -> PyResult<Self> {
        let board = match path {
            Some(p) => Board::open_path(p),
            None => Board::open(index),
        }
        .map_err(err)?;
        let mut job = Job::new(board, laser, field.map(|f| f.inner)).map_err(err)?;
        if unlock_now {
            job.ensure_unlocked(2);
        }
        Ok(PyJob { inner: job })
    }

    /// Warnings raised since the last call, then cleared.
    fn warnings(&mut self) -> Vec<String> {
        std::mem::take(&mut self.inner.warnings)
    }

    // ---- configuration ---------------------------------------------------

    fn select(&mut self, kind: &str) -> PyResult<()> {
        self.inner.select(kind).map_err(err)
    }

    #[pyo3(signature = (freq_khz=None, power_pct=None, power_byte=None, mopa_pulse=None, mo=None, tickle=None))]
    fn configure(
        &mut self,
        freq_khz: Option<f64>,
        power_pct: Option<f64>,
        power_byte: Option<u8>,
        mopa_pulse: Option<u32>,
        mo: Option<bool>,
        tickle: Option<bool>,
    ) -> PyResult<()> {
        self.inner
            .configure(freq_khz, power_pct, power_byte, mopa_pulse, mo, tickle)
            .map_err(err)
    }

    #[pyo3(signature = (max_mm_s=None, max_accel_mm_s2=None, max_loop_hz=None))]
    fn set_limits(&mut self, max_mm_s: Option<f64>, max_accel_mm_s2: Option<f64>, max_loop_hz: Option<f64>) {
        self.inner.set_limits(Limits {
            max_mm_s,
            max_accel_mm_s2,
            max_loop_hz,
        });
    }

    // ---- state -----------------------------------------------------------

    fn status(&mut self, py: Python<'_>) -> Option<Vec<u8>> {
        py.detach(|| self.inner.status())
    }

    fn unlocked(&mut self, py: Python<'_>) -> bool {
        py.detach(|| self.inner.unlocked())
    }

    #[pyo3(signature = (attempts=2))]
    fn ensure_unlocked(&mut self, py: Python<'_>, attempts: u32) -> bool {
        py.detach(|| self.inner.ensure_unlocked(attempts))
    }

    fn inputs(&mut self, py: Python<'_>) -> Option<u8> {
        py.detach(|| self.inner.inputs())
    }

    fn input_pin(&mut self, py: Python<'_>, n: u8) -> Option<bool> {
        py.detach(|| self.inner.input_pin(n))
    }

    fn remark(&mut self, py: Python<'_>) -> Option<bool> {
        py.detach(|| self.inner.remark())
    }

    fn free_cache(&mut self, py: Python<'_>) -> u16 {
        py.detach(|| self.inner.free_cache())
    }

    fn running(&mut self, py: Python<'_>) -> Option<bool> {
        py.detach(|| self.inner.running())
    }

    fn armed(&mut self, py: Python<'_>) -> Option<bool> {
        py.detach(|| self.inner.armed())
    }

    /// True while the vector queue is still executing.
    fn busy(&mut self, py: Python<'_>) -> Option<bool> {
        py.detach(|| self.inner.busy())
    }

    #[pyo3(signature = (timeout=30.0, poll=0.02))]
    fn wait_idle(&mut self, py: Python<'_>, timeout: f64, poll: f64) -> bool {
        py.detach(|| self.inner.wait_idle(timeout, poll))
    }

    fn sgin(&mut self, py: Python<'_>) -> Option<bool> {
        py.detach(|| self.inner.sgin())
    }

    #[pyo3(signature = (seconds, poll=0.005))]
    fn guard(&mut self, py: Python<'_>, seconds: f64, poll: f64) -> bool {
        py.detach(|| self.inner.guard(seconds, poll))
    }

    // ---- marking ---------------------------------------------------------

    #[pyo3(signature = (start=(0x4000, 0x8000), speed=200))]
    fn begin(&mut self, py: Python<'_>, start: (u16, u16), speed: u16) -> PyResult<()> {
        py.detach(|| self.inner.begin(start, speed)).map_err(err)
    }

    #[pyo3(signature = (points, speed=200, delay=500))]
    fn lines(&mut self, py: Python<'_>, points: Vec<(u16, u16)>, speed: u16, delay: u16) -> PyResult<()> {
        py.detach(|| self.inner.lines(&points, speed, delay))
            .map_err(err)
    }

    #[pyo3(signature = (x, y, speed=0x2710, delay=0x01F4))]
    fn jump(&mut self, py: Python<'_>, x: u16, y: u16, speed: u16, delay: u16) -> PyResult<()> {
        py.detach(|| self.inner.jump(x, y, speed, delay)).map_err(err)
    }

    /// Stream a run of points with the laser on or off per segment.
    #[pyo3(signature = (points, lit=None, speed=None, mm_s=None, jump_speed=0x2710,
                        jump_delay=0x01F4, delay=500, corner_delay=0, overshoot=0,
                        wiggle=0, wiggle_pitch=0, wiggle_steps=16, unlit_at_feed=false))]
    #[allow(clippy::too_many_arguments)]
    fn path(
        &mut self,
        py: Python<'_>,
        points: Vec<(u16, u16)>,
        lit: Option<Vec<bool>>,
        speed: Option<u16>,
        mm_s: Option<f64>,
        jump_speed: u16,
        jump_delay: u16,
        delay: u16,
        corner_delay: u16,
        overshoot: i64,
        wiggle: i64,
        wiggle_pitch: i64,
        wiggle_steps: u32,
        unlit_at_feed: bool,
    ) -> PyResult<Py<PyDict>> {
        let sp = speed_of(speed, mm_s)?;
        let opts = PathOpts {
            jump_speed,
            jump_delay,
            delay,
            corner_delay,
            overshoot,
            wiggle,
            wiggle_pitch,
            wiggle_steps,
            unlit_at_feed,
        };
        let out = py
            .detach(|| self.inner.path(&points, lit.as_deref(), sp, opts))
            .map_err(PyValueError::new_err)?;
        emitted(py, out)
    }

    /// Mark disjoint segments in one batch.
    #[pyo3(signature = (segs, speed=None, mm_s=None, jump_speed=0x2710, jump_delay=0x01F4,
                        delay=500, corner_delay=0, overshoot=0, wiggle=0, wiggle_pitch=0,
                        wiggle_steps=16, unlit_at_feed=false))]
    #[allow(clippy::too_many_arguments)]
    fn segments(
        &mut self,
        py: Python<'_>,
        segs: Vec<((u16, u16), (u16, u16))>,
        speed: Option<u16>,
        mm_s: Option<f64>,
        jump_speed: u16,
        jump_delay: u16,
        delay: u16,
        corner_delay: u16,
        overshoot: i64,
        wiggle: i64,
        wiggle_pitch: i64,
        wiggle_steps: u32,
        unlit_at_feed: bool,
    ) -> PyResult<Py<PyDict>> {
        let sp = speed_of(speed, mm_s)?;
        let opts = PathOpts {
            jump_speed,
            jump_delay,
            delay,
            corner_delay,
            overshoot,
            wiggle,
            wiggle_pitch,
            wiggle_steps,
            unlit_at_feed,
        };
        let out = py
            .detach(|| self.inner.segments(&segs, sp, opts))
            .map_err(PyValueError::new_err)?;
        emitted(py, out)
    }

    /// Point marking. UNTESTED on hardware.
    #[pyo3(signature = (points, dwell_us, jump_speed=0x2710, jump_delay=0x01F4))]
    fn dots(
        &mut self,
        py: Python<'_>,
        points: Vec<(u16, u16)>,
        dwell_us: u16,
        jump_speed: u16,
        jump_delay: u16,
    ) -> PyResult<Py<PyDict>> {
        let opts = PathOpts {
            jump_speed,
            jump_delay,
            ..PathOpts::default()
        };
        let out = py
            .detach(|| self.inner.dots(&points, dwell_us, opts))
            .map_err(PyValueError::new_err)?;
        emitted(py, out)
    }

    // ---- millimetres -----------------------------------------------------

    #[pyo3(signature = (x_mm, y_mm, clamp=false))]
    fn mm(&self, x_mm: f64, y_mm: f64, clamp: bool) -> PyResult<(u16, u16)> {
        self.inner.mm(x_mm, y_mm, clamp).map_err(PyValueError::new_err)
    }

    fn where_mm(&self, x: u16, y: u16) -> (f64, f64) {
        self.inner.where_mm(x, y)
    }

    /// What a wiggle asks of the mirrors, before you cut anything.
    #[pyo3(signature = (radius_mm, pitch_mm, mm_s, steps=16))]
    fn wiggle_load(
        &self,
        py: Python<'_>,
        radius_mm: f64,
        pitch_mm: f64,
        mm_s: f64,
        steps: u32,
    ) -> PyResult<Py<PyDict>> {
        let l = self
            .inner
            .wiggle_load(radius_mm, pitch_mm, mm_s, steps)
            .map_err(PyValueError::new_err)?;
        let d = PyDict::new(py);
        d.set_item("loop_hz", l.loop_hz)?;
        d.set_item("accel_mm_s2", l.accel_mm_s2)?;
        d.set_item("accel_g", l.accel_g)?;
        d.set_item("exposure", l.exposure)?;
        d.set_item("chord_mm", l.chord_mm)?;
        d.set_item("vectors_per_s", l.vectors_per_s)?;
        d.set_item("exceeded", l.exceeded)?;
        Ok(d.into())
    }

    /// Run-up length that actually reaches mm_s, as v^2 / 2a.
    #[pyo3(signature = (mm_s, accel_mm_s2=None))]
    fn runup_mm(&self, mm_s: f64, accel_mm_s2: Option<f64>) -> PyResult<f64> {
        self.inner.runup_mm(mm_s, accel_mm_s2).map_err(PyValueError::new_err)
    }

    // ---- laser hardware --------------------------------------------------

    #[pyo3(signature = (on=true))]
    fn mo(&mut self, on: bool) -> bool {
        self.inner.mo(on)
    }

    fn mopa_pulse(&mut self, py: Python<'_>, ns: u32) -> PyResult<()> {
        py.detach(|| self.inner.mopa_pulse(ns)).map_err(err)
    }

    #[pyo3(signature = (value, freq_khz=None))]
    fn power_byte(&mut self, py: Python<'_>, value: u8, freq_khz: Option<f64>) -> PyResult<u8> {
        py.detach(|| self.inner.power_byte(value, freq_khz)).map_err(err)
    }

    #[pyo3(signature = (on=true))]
    fn red_light(&mut self, py: Python<'_>, on: bool) -> PyResult<bool> {
        py.detach(|| self.inner.red_light(on)).map_err(err)
    }

    #[pyo3(signature = (freq_khz=None, width_us=None, enable=None))]
    fn tick(&mut self, freq_khz: Option<f64>, width_us: Option<f64>, enable: Option<bool>) -> PyResult<(f64, u16, f64)> {
        self.inner.tick(freq_khz, width_us, enable).map_err(PyValueError::new_err)
    }

    #[pyo3(signature = (pulses, pps, direction=false, flags=0, min_pps=None, acctime=100, p232=175))]
    #[allow(clippy::too_many_arguments)]
    fn axis_move(
        &mut self,
        py: Python<'_>,
        pulses: u32,
        pps: u32,
        direction: bool,
        flags: u16,
        min_pps: Option<u32>,
        acctime: u16,
        p232: u16,
    ) -> PyResult<f64> {
        py.detach(|| self.inner.axis_move(pulses, pps, direction, flags, min_pps, acctime, p232))
            .map_err(err)
    }

    fn out(&mut self, py: Python<'_>, port: u8, value: u16) -> Option<Vec<u8>> {
        py.detach(|| self.inner.out(port, value))
    }

    fn out_state(&mut self, py: Python<'_>) -> Option<Vec<u8>> {
        py.detach(|| self.inner.out_state())
    }

    /// What would go on the wire, as a dict.
    fn settings(&self, py: Python<'_>) -> PyResult<Py<PyDict>> {
        let s = self.inner.settings();
        let d = PyDict::new(py);
        d.set_item("laser", s.laser)?;
        d.set_item("code", s.code)?;
        d.set_item("freq_khz", s.freq_khz)?;
        d.set_item("power_pct", s.power_pct)?;
        d.set_item("power_byte", s.power_byte)?;
        d.set_item("mopa_pulse", s.mopa_pulse)?;
        d.set_item("mo", s.mo)?;
        d.set_item("tickle", s.tickle)?;
        d.set_item("tick_khz", s.tick_khz)?;
        d.set_item("tick_us", s.tick_us)?;
        d.set_item("verified", s.verified)?;
        Ok(d.into())
    }

    /// Stop the free-running tickle generator.
    fn tick_off(&mut self, py: Python<'_>) -> PyResult<()> {
        py.detach(|| self.inner.tick_off()).map_err(err)
    }

    /// Sustained laser PWM for scope work, paced off the board's own counter.
    #[pyo3(signature = (seconds=10.0, speed=200, span=(0x4000, 0xC000), margin=48))]
    fn pwm_burst(&mut self, py: Python<'_>, seconds: f64, speed: u16, span: (u16, u16), margin: u16) -> PyResult<usize> {
        py.detach(|| self.inner.pwm_burst(seconds, speed, span, margin)).map_err(err)
    }

    /// Analog power out. NOT CONFIRMED WORKING on the board tested.
    #[pyo3(signature = (value12, mark=false))]
    fn dac(&mut self, py: Python<'_>, value12: u16, mark: bool) -> PyResult<u16> {
        py.detach(|| self.inner.dac(value12, mark)).map_err(err)
    }

    /// Timed output pulse. UNTESTED.
    fn out_pulse(&mut self, py: Python<'_>, port: u8, value: u8, ms: u32) -> PyResult<()> {
        py.detach(|| self.inner.out_pulse(port, value, ms)).map_err(err)
    }

    /// 0x2F84 laser port switch. Purpose not established.
    #[pyo3(signature = (p1=0, p2=0, p3=0, p4=0, p5=0, p6=0))]
    fn laser_port_switch(&mut self, py: Python<'_>, p1: u16, p2: u16, p3: u16, p4: u16, p5: u16, p6: u16) -> PyResult<()> {
        py.detach(|| self.inner.laser_port_switch(p1, p2, p3, p4, p5, p6)).map_err(err)
    }

    /// The 16-bit word at bytes 5..6 of the 0x0101 reply.
    fn status_word(&mut self, py: Python<'_>) -> Option<u16> {
        py.detach(|| self.inner.status_word())
    }

    // ---- millimetres -----------------------------------------------------

    #[pyo3(signature = (start=(0.0, 0.0), speed=200, clamp=false))]
    fn begin_mm(&mut self, py: Python<'_>, start: (f64, f64), speed: u16, clamp: bool) -> PyResult<()> {
        let p = self.inner.mm(start.0, start.1, clamp).map_err(PyValueError::new_err)?;
        py.detach(|| self.inner.begin(p, speed)).map_err(err)
    }

    #[pyo3(signature = (x_mm, y_mm, speed=0x2710, delay=0x01F4, clamp=false))]
    fn jump_mm(&mut self, py: Python<'_>, x_mm: f64, y_mm: f64, speed: u16, delay: u16, clamp: bool) -> PyResult<()> {
        let p = self.inner.mm(x_mm, y_mm, clamp).map_err(PyValueError::new_err)?;
        py.detach(|| self.inner.jump(p.0, p.1, speed, delay)).map_err(err)
    }

    #[pyo3(signature = (points_mm, speed=200, delay=500, clamp=false))]
    fn lines_mm(&mut self, py: Python<'_>, points_mm: Vec<(f64, f64)>, speed: u16, delay: u16, clamp: bool) -> PyResult<()> {
        let pts = self.mm_points(&points_mm, clamp)?;
        py.detach(|| self.inner.lines(&pts, speed, delay)).map_err(err)
    }

    /// path() with points, run-up and wiggle in millimetres.
    #[pyo3(signature = (points_mm, lit=None, speed=None, mm_s=None, jump_speed=0x2710,
                        jump_delay=0x01F4, delay=500, corner_delay=0, overshoot_mm=0.0,
                        wiggle_mm=0.0, wiggle_pitch_mm=0.0, wiggle_steps=16,
                        unlit_at_feed=false, clamp=false))]
    #[allow(clippy::too_many_arguments)]
    fn path_mm(
        &mut self,
        py: Python<'_>,
        points_mm: Vec<(f64, f64)>,
        lit: Option<Vec<bool>>,
        speed: Option<u16>,
        mm_s: Option<f64>,
        jump_speed: u16,
        jump_delay: u16,
        delay: u16,
        corner_delay: u16,
        overshoot_mm: f64,
        wiggle_mm: f64,
        wiggle_pitch_mm: f64,
        wiggle_steps: u32,
        unlit_at_feed: bool,
        clamp: bool,
    ) -> PyResult<Py<PyDict>> {
        let sp = speed_of(speed, mm_s)?;
        let pts = self.mm_points(&points_mm, clamp)?;
        let opts = PathOpts {
            jump_speed,
            jump_delay,
            delay,
            corner_delay,
            overshoot: self.inner.counts(overshoot_mm, "run-up"),
            wiggle: self.inner.counts(wiggle_mm, "wiggle radius"),
            wiggle_pitch: self.inner.counts(wiggle_pitch_mm, "wiggle pitch"),
            wiggle_steps,
            unlit_at_feed,
        };
        let out = py
            .detach(|| self.inner.path(&pts, lit.as_deref(), sp, opts))
            .map_err(PyValueError::new_err)?;
        emitted(py, out)
    }

    /// segments() with points, run-up and wiggle in millimetres.
    #[pyo3(signature = (segs_mm, speed=None, mm_s=None, jump_speed=0x2710, jump_delay=0x01F4,
                        delay=500, corner_delay=0, overshoot_mm=0.0, wiggle_mm=0.0,
                        wiggle_pitch_mm=0.0, wiggle_steps=16, unlit_at_feed=false, clamp=false))]
    #[allow(clippy::too_many_arguments)]
    fn segments_mm(
        &mut self,
        py: Python<'_>,
        segs_mm: Vec<((f64, f64), (f64, f64))>,
        speed: Option<u16>,
        mm_s: Option<f64>,
        jump_speed: u16,
        jump_delay: u16,
        delay: u16,
        corner_delay: u16,
        overshoot_mm: f64,
        wiggle_mm: f64,
        wiggle_pitch_mm: f64,
        wiggle_steps: u32,
        unlit_at_feed: bool,
        clamp: bool,
    ) -> PyResult<Py<PyDict>> {
        let sp = speed_of(speed, mm_s)?;
        let mut segs = Vec::with_capacity(segs_mm.len());
        for (a, b) in &segs_mm {
            segs.push((
                self.inner.mm(a.0, a.1, clamp).map_err(PyValueError::new_err)?,
                self.inner.mm(b.0, b.1, clamp).map_err(PyValueError::new_err)?,
            ));
        }
        let opts = PathOpts {
            jump_speed,
            jump_delay,
            delay,
            corner_delay,
            overshoot: self.inner.counts(overshoot_mm, "run-up"),
            wiggle: self.inner.counts(wiggle_mm, "wiggle radius"),
            wiggle_pitch: self.inner.counts(wiggle_pitch_mm, "wiggle pitch"),
            wiggle_steps,
            unlit_at_feed,
        };
        let out = py
            .detach(|| self.inner.segments(&segs, sp, opts))
            .map_err(PyValueError::new_err)?;
        emitted(py, out)
    }

    /// dots() with points in millimetres. UNTESTED on hardware.
    #[pyo3(signature = (points_mm, dwell_us, jump_speed=0x2710, jump_delay=0x01F4, clamp=false))]
    fn dots_mm(
        &mut self,
        py: Python<'_>,
        points_mm: Vec<(f64, f64)>,
        dwell_us: u16,
        jump_speed: u16,
        jump_delay: u16,
        clamp: bool,
    ) -> PyResult<Py<PyDict>> {
        let pts = self.mm_points(&points_mm, clamp)?;
        let opts = PathOpts {
            jump_speed,
            jump_delay,
            ..PathOpts::default()
        };
        let out = py
            .detach(|| self.inner.dots(&pts, dwell_us, opts))
            .map_err(PyValueError::new_err)?;
        emitted(py, out)
    }

    // ---- safety ----------------------------------------------------------

    /// Silence every laser output: marking PWM, tickle, gate.
    fn laser_off(&mut self, py: Python<'_>) {
        py.detach(|| self.inner.laser_off())
    }

    /// Stop marking and kill laser output now.
    fn abort(&mut self, py: Python<'_>) {
        py.detach(|| self.inner.abort())
    }

    fn stop(&mut self, py: Python<'_>) {
        py.detach(|| self.inner.stop())
    }

    #[pyo3(signature = (quiet=true))]
    fn close(&mut self, py: Python<'_>, quiet: bool) {
        py.detach(|| self.inner.close(quiet))
    }

    fn __enter__(slf: Py<Self>) -> Py<Self> {
        slf
    }

    #[pyo3(signature = (*_args))]
    fn __exit__(&mut self, py: Python<'_>, _args: &Bound<'_, pyo3::types::PyTuple>) -> bool {
        py.detach(|| self.inner.close(true));
        false
    }
}

impl PyJob {
    /// Millimetre points to counts, refusing anything outside the field.
    fn mm_points(&self, pts: &[(f64, f64)], clamp: bool) -> PyResult<Vec<(u16, u16)>> {
        pts.iter()
            .map(|(x, y)| self.inner.mm(*x, *y, clamp).map_err(PyValueError::new_err))
            .collect()
    }
}

fn emitted(py: Python<'_>, e: crate::job::Emitted) -> PyResult<Py<PyDict>> {
    let d = PyDict::new(py);
    d.set_item("commands", e.commands)?;
    d.set_item("us", e.us)?;
    d.set_item("overshoot", e.overshoot)?;
    d.set_item("exposure", e.exposure)?;
    Ok(d.into())
}

/// Every DBK2JP on this machine, as backend-specific path strings.
#[pyfunction]
#[pyo3(signature = (any_vidpid=false))]
fn find_devices(any_vidpid: bool) -> Vec<String> {
    crate::find_devices(any_vidpid)
}

/// Build a 12-byte tagSeaCMD.
#[pyfunction]
#[pyo3(signature = (cmd_id, p0=0, p1=0, p2=0, p3=0, p4=0))]
fn cmd(cmd_id: u16, p0: u16, p1: u16, p2: u16, p3: u16, p4: u16) -> Vec<u8> {
    crate::protocol::cmd(cmd_id, p0, p1, p2, p3, p4).to_vec()
}

/// Split a 12-byte reply into (cmd_id, p0, p1, p2, p3, p4).
#[pyfunction]
fn parse(reply: Vec<u8>) -> PyResult<(u16, u16, u16, u16, u16, u16)> {
    crate::protocol::parse(&reply)
        .map(|f| (f[0], f[1], f[2], f[3], f[4], f[5]))
        .ok_or_else(|| PyValueError::new_err("a reply is 12 bytes"))
}

#[pymodule]
fn dbk2jp_rs(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyJob>()?;
    m.add_class::<PyBoard>()?;
    m.add_class::<PyField>()?;
    m.add_function(wrap_pyfunction!(find_devices, m)?)?;
    m.add_function(wrap_pyfunction!(cmd, m)?)?;
    m.add_function(wrap_pyfunction!(parse, m)?)?;
    m.add("VID", crate::usb::VID)?;
    m.add("PID", crate::usb::PID)?;
    m.add("CO2", "co2")?;
    m.add("FIBER", "fiber")?;
    m.add("UV", "uv")?;
    m.add("GREEN", "green")?;
    m.add("MOPA", "mopa")?;
    m.add("YAG", "yag")?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}

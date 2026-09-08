//! Offline checks of the geometry and timing layer, against a fake transport.
//! No hardware is touched: these assert what goes on the wire.

use dbk2jp_rs::job::{Job, PathOpts, Speed, MAX_SEGS};
use dbk2jp_rs::usb::{Board, Result, Transport, Xfer};
use dbk2jp_rs::Field;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Recorder {
    writes: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl Transport for Recorder {
    fn path(&self) -> String {
        "fake".into()
    }
    fn xfer(&mut self, ep: u8, payload: &[u8], read_len: usize, _t: u32) -> Result<Xfer> {
        if read_len > 0 {
            // A plausible 0x0101 reply: armed, idle, 189 free slots.
            let mut st = vec![0u8; 12];
            st[0] = 0x01;
            st[1] = 0x01;
            st[2] = 0x2E;
            st[5] = 0x0F;
            st[6] = 0xBD;
            return Ok(Xfer {
                nt: 0,
                usbd: 0,
                data: st,
                moved: 12,
            });
        }
        if ep == 0x02 {
            self.writes.lock().unwrap().push(payload.to_vec());
        }
        Ok(Xfer {
            nt: 0,
            usbd: 0,
            data: Vec::new(),
            moved: payload.len(),
        })
    }
    fn abort_pipe(&mut self, _ep: u8) -> bool {
        true
    }
    fn reset_pipe(&mut self, _ep: u8) -> bool {
        true
    }
    fn close(&mut self) {}
}

struct Rig {
    job: Job,
    writes: Arc<Mutex<Vec<Vec<u8>>>>,
}

fn rig() -> Rig {
    let writes = Arc::new(Mutex::new(Vec::new()));
    let t = Recorder {
        writes: writes.clone(),
    };
    let job = Job::new(
        Board::with_transport(Box::new(t)),
        "co2",
        Some(Field::new(110.0)),
    )
    .unwrap();
    Rig { job, writes }
}

impl Rig {
    fn cmds(&self) -> Vec<[u16; 6]> {
        let mut out = Vec::new();
        for blob in self.writes.lock().unwrap().iter() {
            for chunk in blob.chunks(12) {
                if let Some(f) = dbk2jp_rs::parse(chunk) {
                    out.push(f);
                }
            }
        }
        out
    }
    fn marks(&self) -> Vec<[u16; 6]> {
        self.cmds().into_iter().filter(|c| c[0] == 0x0243).collect()
    }
    fn jumps(&self) -> Vec<[u16; 6]> {
        self.cmds().into_iter().filter(|c| c[0] == 0x0241).collect()
    }
    fn writes(&self) -> usize {
        self.writes.lock().unwrap().len()
    }
}

const A: (u16, u16) = (0x4000, 0x8000);
const B: (u16, u16) = (0x6000, 0x8000);
/// counts per mm on the 110 mm field used here
const CPM: f64 = 65535.0 / 110.0;

fn dist_to_line(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    ((dx * (a.1 - p.1) - (a.0 - p.0) * dy) / (dx * dx + dy * dy).sqrt()).abs()
}

#[test]
fn gates_the_laser_per_segment() {
    let mut r = rig();
    let pts = [A, B, (0x6000, 0x9000)];
    r.job
        .path(&pts, Some(&[true, false]), Speed::Micros(1000), PathOpts::default())
        .unwrap();
    assert_eq!(r.marks().len(), 1, "one lit segment");
    // The unlit leg is a traverse at jump speed, not a crawl at the feed rate.
    let gap = r.jumps().into_iter().find(|c| c[3] == 0x9000).unwrap();
    assert_eq!(gap[1], 0x2710);
}

#[test]
fn feed_rate_holds_across_unequal_segments() {
    let mut r = rig();
    let pts = [(0x8000, 0x8000), (0x9000, 0x8000), (0x9000, 0x8800)];
    r.job
        .path(&pts, None, Speed::MmPerSec(1000.0), PathOpts::default())
        .unwrap();
    let mut prev: (f64, f64) = (32768.0, 32768.0);
    for c in r.marks() {
        let d = ((c[2] as f64 - prev.0).powi(2) + (c[3] as f64 - prev.1).powi(2)).sqrt() / CPM;
        let mm_s = d / (c[1] as f64 * 1e-6);
        assert!((mm_s - 1000.0).abs() < 5.0, "{} mm/s", mm_s);
        prev = (c[2] as f64, c[3] as f64);
    }
}

#[test]
fn run_up_is_trimmed_to_the_travel_limits_rather_than_refused() {
    let mut r = rig();
    let out = r
        .job
        .path(
            &[(0x0064, 0x8000), B],
            None,
            Speed::Micros(1000),
            PathOpts {
                overshoot: 400,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(out.overshoot, 100, "only 100 counts of room existed");
    assert_eq!(r.jumps()[0][2], 0, "run-up starts at the limit");
}

#[test]
fn out_of_field_geometry_is_refused_before_anything_is_written() {
    let mut r = rig();
    let err = r
        .job
        .path(&[(0x8000, 0x8000); 1], None, Speed::Micros(100), PathOpts::default())
        .unwrap_err();
    assert!(err.contains("at least two points"), "{}", err);
    assert_eq!(r.writes(), 0);
}

#[test]
fn dwell_lands_only_where_a_lit_run_ends() {
    let mut r = rig();
    r.job
        .path(
            &[A, B, (0x6000, 0x9000)],
            None,
            Speed::MmPerSec(600.0),
            PathOpts {
                delay: 500,
                corner_delay: 80,
                ..Default::default()
            },
        )
        .unwrap();
    let m = r.marks();
    assert_eq!(m[0][5], 80, "interior vertex takes the corner delay");
    assert_eq!(m[1][5], 500, "closing vector takes the laser-off delay");
}

#[test]
fn wiggle_widens_by_twice_the_radius_and_lands_on_the_ends() {
    let mut r = rig();
    let out = r
        .job
        .path(
            &[A, B],
            None,
            Speed::MmPerSec(300.0),
            PathOpts {
                wiggle: 60,
                wiggle_pitch: 400,
                ..Default::default()
            },
        )
        .unwrap();
    let m = r.marks();
    let offs: Vec<f64> = m
        .iter()
        .map(|c| {
            dist_to_line(
                (c[2] as f64, c[3] as f64),
                (A.0 as f64, A.1 as f64),
                (B.0 as f64, B.1 as f64),
            )
        })
        .collect();
    let max = offs.iter().cloned().fold(0.0, f64::max);
    assert!((max - 60.0).abs() <= 1.5, "kerf half-width {}", max);
    // The amplitude ramps in and out, so the cut starts and ends on the line.
    assert!(offs[0] < 10.0 && *offs.last().unwrap() < 10.0);
    assert_eq!(
        (m.last().unwrap()[2], m.last().unwrap()[3]),
        B,
        "ends exactly on the endpoint"
    );
    assert!(out.exposure > 1.15, "exposure {}", out.exposure);
    // Every wiggle point except the last carries no dwell of its own.
    assert!(m[..m.len() - 1].iter().all(|c| c[5] == 0));
}

#[test]
fn wiggle_belongs_to_the_run_not_to_each_segment() {
    let opts = PathOpts {
        wiggle: 60,
        wiggle_pitch: 400,
        ..Default::default()
    };
    let mut whole = rig();
    let w = whole
        .job
        .path(&[A, B], None, Speed::MmPerSec(300.0), opts)
        .unwrap();
    let split_pts: Vec<(u16, u16)> = (0..6).map(|i| (0x4000 + i * 1638, 0x8000)).collect();
    let mut split = rig();
    let s = split
        .job
        .path(&split_pts, None, Speed::MmPerSec(300.0), opts)
        .unwrap();
    assert!(
        (w.exposure - s.exposure).abs() < 0.02,
        "whole {} vs split {}",
        w.exposure,
        s.exposure
    );

    // A run shorter than one pitch gets a fraction of a turn, not a full loop.
    let short: Vec<(u16, u16)> = (0..6).map(|i| (0x8000 + i * 30, 0x8000)).collect();
    let mut r = rig();
    let out = r.job.path(&short, None, Speed::MmPerSec(300.0), opts).unwrap();
    assert!(out.commands < 20, "{} commands", out.commands);
    assert!(out.exposure < 2.5, "{}x", out.exposure);
}

#[test]
fn a_wiggle_leaving_the_field_is_refused() {
    let mut r = rig();
    let err = r
        .job
        .path(
            &[(0x4000, 100), (0x6000, 100)],
            None,
            Speed::MmPerSec(300.0),
            PathOpts {
                wiggle: 200,
                wiggle_pitch: 400,
                ..Default::default()
            },
        )
        .unwrap_err();
    assert!(err.contains("wiggle point"), "{}", err);
}

#[test]
fn raw_duration_is_split_across_the_traced_path() {
    let mut r = rig();
    r.job
        .path(
            &[A, B],
            None,
            Speed::Micros(5000),
            PathOpts {
                wiggle: 60,
                wiggle_pitch: 400,
                ..Default::default()
            },
        )
        .unwrap();
    let total: u32 = r.marks().iter().map(|c| c[1] as u32).sum();
    let n = r.marks().len() as u32;
    assert!(
        (total as i64 - 5000).unsigned_abs() <= n as u64,
        "{} us over {} marks",
        total,
        n
    );
}

#[test]
fn tighter_pitch_concentrates_more_exposure() {
    let mut wide = rig();
    let w = wide
        .job
        .path(
            &[A, B],
            None,
            Speed::MmPerSec(600.0),
            PathOpts {
                wiggle: 60,
                wiggle_pitch: 400,
                ..Default::default()
            },
        )
        .unwrap();
    let mut tight = rig();
    let t = tight
        .job
        .path(
            &[A, B],
            None,
            Speed::MmPerSec(600.0),
            PathOpts {
                wiggle: 60,
                wiggle_pitch: 120,
                ..Default::default()
            },
        )
        .unwrap();
    assert!(t.exposure > w.exposure * 2.0, "{} vs {}", t.exposure, w.exposure);
}

#[test]
fn segments_batch_into_one_write_and_report_the_kinematics() {
    let mut r = rig();
    let out = r
        .job
        .segments(
            &[(A, B), ((0xA000, 0x8000), (0xC000, 0x8000))],
            Speed::MmPerSec(2000.0),
            PathOpts {
                overshoot: 200,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(r.writes(), 1, "one write for both segments");
    assert_eq!(out.overshoot, 200);

    // The load report needs no machine data to state the physics.
    let load = r.job.wiggle_load(0.1, 0.6, 600.0, 16).unwrap();
    assert!((load.loop_hz - 1000.0).abs() < 1.0);
    assert!((load.accel_g - 367.0).abs() < 2.0, "{} g", load.accel_g);
}

#[test]
fn limits_flag_a_wiggle_the_mirrors_will_not_follow() {
    let mut r = rig();
    r.job.set_limits(dbk2jp_rs::Limits {
        max_mm_s: Some(3000.0),
        max_accel_mm_s2: Some(200_000.0),
        max_loop_hz: Some(250.0),
    });
    let load = r.job.wiggle_load(0.1, 0.6, 600.0, 16).unwrap();
    assert_eq!(load.exceeded.len(), 2, "{:?}", load.exceeded);
    assert!((r.job.runup_mm(600.0, None).unwrap() - 0.9).abs() < 1e-9);

    r.job
        .path(
            &[A, B],
            None,
            Speed::MmPerSec(600.0),
            PathOpts {
                wiggle: 60,
                wiggle_pitch: 400,
                ..Default::default()
            },
        )
        .unwrap();
    assert!(
        r.job.warnings.iter().any(|w| w.contains("lateral acceleration")),
        "{:?}",
        r.job.warnings
    );
}

#[test]
fn batches_are_chunked_at_max_segs() {
    let mut r = rig();
    let out = r
        .job
        .path(
            &[(0x2000, 0x8000), (0xE000, 0x8000)],
            None,
            Speed::MmPerSec(500.0),
            PathOpts {
                wiggle: 40,
                wiggle_pitch: 100,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(r.writes(), out.commands.div_ceil(MAX_SEGS));
}

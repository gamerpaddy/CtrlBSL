//! Scan field: millimetres in, 16-bit galvo counts out.
//!
//! The board only ever sees raw 16-bit coordinates, 0x0000 to 0xFFFF with
//! 0x8000 at the centre. Everything optical lives here.
//!
//! Field size, offsets, per-axis aspect, mirror and axis swap are exact. The
//! barrel, horizontal-vertical and trapezoid terms follow the conventional
//! galvo model rather than a recovered formula: every one of them is 1.0 in the
//! available config, so there was nothing to measure against.

use std::collections::BTreeMap;
use std::fs;

pub const CENTRE: f64 = 32768.0;
pub const FULL: f64 = 65535.0;

#[derive(Clone, Debug)]
pub struct Field {
    pub size_mm: f64,
    pub offset_mm: (f64, f64),
    pub aspect: (f64, f64),
    pub negate: (bool, bool),
    pub swap_xy: bool,
    pub distor: (f64, f64),
    pub horver: (f64, f64),
    pub trapezoid: (f64, f64),
    pub path: Option<String>,
    pub created: bool,
}

impl Default for Field {
    fn default() -> Self {
        Field {
            size_mm: 100.0,
            offset_mm: (0.0, 0.0),
            aspect: (100.0, 100.0),
            negate: (false, false),
            swap_xy: false,
            distor: (1.0, 1.0),
            horver: (1.0, 1.0),
            trapezoid: (1.0, 1.0),
            path: None,
            created: false,
        }
    }
}

/// Parse a markcfg0 / LmcPar.cfg into a flat map. Section headers are ignored;
/// the keys this reads are unique across the file.
pub fn read_markcfg(path: &str) -> Result<BTreeMap<String, String>, String> {
    let raw = fs::read(path).map_err(|e| format!("cannot read {}: {}", path, e))?;
    let text = String::from_utf8_lossy(&raw);
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim_matches(|c: char| c == '\0' || c.is_whitespace());
        if line.is_empty() || line.starts_with('[') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            out.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    Ok(out)
}

impl Field {
    pub fn new(size_mm: f64) -> Self {
        Field {
            size_mm,
            ..Default::default()
        }
    }

    /// Build from a machine's markcfg0.
    pub fn from_markcfg(path: &str) -> Result<Self, String> {
        let cfg = read_markcfg(path)?;
        let f = |key: &str, default: f64| -> f64 {
            cfg.get(key)
                .and_then(|v| v.parse::<f64>().ok())
                .unwrap_or(default)
        };
        Ok(Field {
            size_mm: f("FIELDSIZE", 100.0),
            offset_mm: (f("FIELDOFFSETX", 0.0), f("FIELDOFFSETY", 0.0)),
            aspect: (f("GALVOASPECT0", 100.0), f("GALVOASPECT1", 100.0)),
            negate: (f("GALVONEGATE0", 0.0) != 0.0, f("GALVONEGATE1", 0.0) != 0.0),
            swap_xy: f("GALVOX", 0.0) != 0.0,
            distor: (f("GALVODISTOR0", 1.0), f("GALVODISTOR1", 1.0)),
            horver: (f("GALVOHORVER0", 1.0), f("GALVOHORVER1", 1.0)),
            trapezoid: (
                f("GALVOTRAPEDISTOR0", 1.0),
                f("GALVOTRAPEDISTOR1", 1.0),
            ),
            path: Some(path.to_string()),
            created: false,
        })
    }

    /// Load a markcfg0, writing a default one first if it does not exist.
    /// A missing config should not stop anyone from marking.
    pub fn load_or_create(path: &str, size_mm: f64) -> Result<Self, String> {
        let mut created = false;
        if fs::metadata(path).is_err() {
            Field::new(size_mm).save(path)?;
            created = true;
        }
        let mut field = Field::from_markcfg(path)?;
        field.created = created;
        Ok(field)
    }

    pub fn half_mm(&self) -> f64 {
        self.size_mm / 2.0
    }

    pub fn mm_per_count(&self) -> f64 {
        self.size_mm / FULL
    }

    pub fn contains(&self, x_mm: f64, y_mm: f64) -> bool {
        let h = self.half_mm();
        (x_mm - self.offset_mm.0).abs() <= h && (y_mm - self.offset_mm.1).abs() <= h
    }

    /// Optical distortion, applied in mm about the field centre. Identity when
    /// every factor is 1.0, which is the common case, exactly.
    fn distort(&self, x: f64, y: f64) -> (f64, f64) {
        let (dx, dy) = self.distor;
        let (hx, hy) = self.horver;
        let (tx, ty) = self.trapezoid;
        if dx == 1.0 && dy == 1.0 && hx == 1.0 && hy == 1.0 && tx == 1.0 && ty == 1.0 {
            return (x, y);
        }
        let h = if self.half_mm() == 0.0 { 1.0 } else { self.half_mm() };
        let (mut u, mut v) = (x / h, y / h);
        u *= 1.0 + (dx - 1.0) * v * v; // barrel / pincushion
        v *= 1.0 + (dy - 1.0) * u * u;
        u *= hx; // horizontal-vertical ratio
        v *= hy;
        u *= 1.0 + (tx - 1.0) * v; // trapezoid / keystone
        v *= 1.0 + (ty - 1.0) * u;
        (u * h, v * h)
    }

    /// Millimetres to a (x, y) pair of 16-bit galvo counts.
    ///
    /// Errors outside the field unless `clamp` is set: wrapping a coordinate
    /// silently would put the beam somewhere else entirely rather than at the
    /// edge.
    pub fn to_counts(&self, x_mm: f64, y_mm: f64, clamp: bool) -> Result<(u16, u16), String> {
        let x = x_mm - self.offset_mm.0;
        let y = y_mm - self.offset_mm.1;
        let (x, y) = self.distort(x, y);
        let h = if self.half_mm() == 0.0 { 1.0 } else { self.half_mm() };
        let half = (FULL as u32 / 2) as f64;
        let mut cx = CENTRE + (x / h) * (self.aspect.0 / 100.0) * half;
        let mut cy = CENTRE + (y / h) * (self.aspect.1 / 100.0) * half;
        if self.negate.0 {
            cx = FULL - cx;
        }
        if self.negate.1 {
            cy = FULL - cy;
        }
        if self.swap_xy {
            std::mem::swap(&mut cx, &mut cy);
        }
        let mut out = [0u16; 2];
        for (i, (value, axis, asked)) in
            [(cx, "X", x_mm), (cy, "Y", y_mm)].into_iter().enumerate()
        {
            let v = value.round();
            if v < 0.0 || v > FULL {
                if !clamp {
                    return Err(format!(
                        "{}={:.3} mm is outside the {} mm field (centre {}, {})",
                        axis, asked, self.size_mm, self.offset_mm.0, self.offset_mm.1
                    ));
                }
                out[i] = v.clamp(0.0, FULL) as u16;
            } else {
                out[i] = v as u16;
            }
        }
        Ok((out[0], out[1]))
    }

    /// Counts back to millimetres.
    pub fn to_mm(&self, x_counts: f64, y_counts: f64) -> (f64, f64) {
        let (mut cx, mut cy) = (x_counts, y_counts);
        if self.swap_xy {
            std::mem::swap(&mut cx, &mut cy);
        }
        if self.negate.0 {
            cx = FULL - cx;
        }
        if self.negate.1 {
            cy = FULL - cy;
        }
        let h = if self.half_mm() == 0.0 { 1.0 } else { self.half_mm() };
        let half = (FULL as u32 / 2) as f64;
        let x = (cx - CENTRE) / half / (self.aspect.0 / 100.0) * h;
        let y = (cy - CENTRE) / half / (self.aspect.1 / 100.0) * h;
        (x + self.offset_mm.0, y + self.offset_mm.1)
    }

    /// These factors as markcfg0 key/value strings.
    pub fn as_markcfg(&self) -> Vec<(String, String)> {
        let e = |v: f64| format!("{:.6e}", v);
        vec![
            ("FIELDSIZE".into(), e(self.size_mm)),
            ("FIELDOFFSETX".into(), e(self.offset_mm.0)),
            ("FIELDOFFSETY".into(), e(self.offset_mm.1)),
            ("GALVOASPECT0".into(), e(self.aspect.0)),
            ("GALVOASPECT1".into(), e(self.aspect.1)),
            ("GALVONEGATE0".into(), (self.negate.0 as u8).to_string()),
            ("GALVONEGATE1".into(), (self.negate.1 as u8).to_string()),
            ("GALVOX".into(), (self.swap_xy as u8).to_string()),
            ("GALVODISTOR0".into(), e(self.distor.0)),
            ("GALVODISTOR1".into(), e(self.distor.1)),
            ("GALVOHORVER0".into(), e(self.horver.0)),
            ("GALVOHORVER1".into(), e(self.horver.1)),
            ("GALVOTRAPEDISTOR0".into(), e(self.trapezoid.0)),
            ("GALVOTRAPEDISTOR1".into(), e(self.trapezoid.1)),
        ]
    }

    /// Write these factors to a markcfg0, preserving keys this library ignores.
    pub fn save(&self, path: &str) -> Result<(), String> {
        let mut kv: BTreeMap<String, String> = read_markcfg(path).unwrap_or_default();
        for (k, v) in self.as_markcfg() {
            kv.insert(k, v);
        }
        let mut text = String::from("[FileBegin]\n[default]\n");
        for (k, v) in &kv {
            text.push_str(&format!("{}={}\n", k, v));
        }
        fs::write(path, text).map_err(|e| format!("cannot write {}: {}", path, e))
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.size_mm <= 0.0 {
            return Err(format!("size_mm must be positive, got {}", self.size_mm));
        }
        if self.aspect.0 == 0.0 || self.aspect.1 == 0.0 {
            return Err("aspect of 0% collapses the axis".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_counts() {
        let f = Field::new(110.0);
        let (cx, cy) = f.to_counts(10.0, -5.0, false).unwrap();
        let (x, y) = f.to_mm(cx as f64, cy as f64);
        assert!((x - 10.0).abs() < 0.01 && (y + 5.0).abs() < 0.01);
    }

    #[test]
    fn matches_the_captured_geometry() {
        // LightBurn marked 55.000 / 54.000 mm on a 110 mm lens at these counts.
        let f = Field {
            offset_mm: (55.0, 55.0),
            ..Field::new(110.0)
        };
        let (x, y) = f.to_mm(40005.0, 39278.0);
        assert!((x - 67.13).abs() < 0.05, "x was {}", x);
        assert!((y - 65.91).abs() < 0.05, "y was {}", y);
    }

    #[test]
    fn outside_the_field_is_an_error_unless_clamped() {
        let f = Field::new(100.0);
        assert!(f.to_counts(80.0, 0.0, false).is_err());
        assert_eq!(f.to_counts(80.0, 0.0, true).unwrap().0, 65535);
    }
}

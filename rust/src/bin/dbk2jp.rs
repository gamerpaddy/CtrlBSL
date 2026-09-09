//! Command line for the DBK2JP.
//!
//!   dbk2jp devices | status | inputs [secs] | unlock | off
//!   dbk2jp out <port> <0|1> | jump <x> <y> | field [path] [key=value ...]
//!
//! Read-only by default. `jump` moves the mirrors with the laser off, and `off`
//! silences every output, which is the one to reach for when something is
//! running that should not be.

use dbk2jp_rs::field::Field;
use dbk2jp_rs::job::Job;
use dbk2jp_rs::usb::Board;
use std::time::{Duration, Instant};

fn num(s: &str) -> Option<i64> {
    let t = s.trim();
    if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        i64::from_str_radix(h, 16).ok()
    } else {
        t.parse().ok()
    }
}

fn job() -> Result<Job, String> {
    let board = Board::open(0).map_err(|e| e.to_string())?;
    Job::new(board, "co2", None)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("help");
    if let Err(e) = run(cmd, &args[args.len().min(1)..]) {
        eprintln!("{}", e);
        std::process::exit(1);
    }
}

fn run(cmd: &str, rest: &[String]) -> Result<(), String> {
    match cmd {
        "devices" => {
            let paths = dbk2jp_rs::find_devices(false);
            if paths.is_empty() {
                println!("no DBK2JP found");
            }
            for p in paths {
                println!("{}", p);
            }
        }
        "status" => {
            let mut j = job()?;
            match j.status() {
                None => println!("no reply"),
                Some(st) => {
                    println!("raw        {}", hex(&st));
                    println!("unlocked   {}", j.unlocked());
                    println!("running    {:?}", j.running());
                    println!("busy       {:?}", j.busy());
                    println!("free cache {} of 256", j.free_cache());
                    println!("inputs     0x{:02X}", j.inputs().unwrap_or(0));
                    println!("SGIN ok    {:?}", j.sgin());
                }
            }
        }
        "inputs" => {
            let secs: f64 = rest.first().and_then(|s| s.parse().ok()).unwrap_or(5.0);
            let mut j = job()?;
            let t0 = Instant::now();
            let mut last: Option<u8> = None;
            println!("watching inputs for {} s, ctrl-c to stop", secs);
            while t0.elapsed().as_secs_f64() < secs {
                if let Some(v) = j.inputs() {
                    if Some(v) != last {
                        println!("{:8.3} s  0x{:02X}  {:08b}", t0.elapsed().as_secs_f64(), v, v);
                        last = Some(v);
                    }
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        "unlock" => {
            let mut j = job()?;
            println!("unlocked: {}", j.ensure_unlocked(2));
            for w in &j.warnings {
                println!("warning: {}", w);
            }
        }
        "off" => {
            let mut j = job()?;
            j.laser_off();
            for w in &j.warnings {
                println!("warning: {}", w);
            }
            println!("every laser output silenced");
        }
        "out" => {
            let (port, value) = match (rest.first().and_then(|s| num(s)), rest.get(1).and_then(|s| num(s))) {
                (Some(p), Some(v)) => (p as u8, v as u16),
                _ => return Err("usage: dbk2jp out <port> <0|1>".into()),
            };
            let mut j = job()?;
            j.ensure_unlocked(2);
            j.out(port, value);
            println!("OUT{} = {}", port, value);
        }
        "jump" => {
            let (x, y) = match (rest.first().and_then(|s| num(s)), rest.get(1).and_then(|s| num(s))) {
                (Some(x), Some(y)) => (x as u16, y as u16),
                _ => return Err("usage: dbk2jp jump <x> <y>   (0x8000 is centre)".into()),
            };
            let mut j = job()?;
            j.ensure_unlocked(2);
            j.begin((x, y), 200).map_err(|e| e.to_string())?;
            println!("jumped to ({}, {})", x, y);
        }
        "field" => {
            let path = rest
                .first()
                .filter(|a| !a.contains('='))
                .cloned()
                .unwrap_or_else(|| "markcfg0".into());
            let mut f = Field::load_or_create(&path, 100.0)?;
            if f.created {
                println!("{} did not exist, wrote a default", path);
            }
            let mut changed = false;
            for a in rest.iter().filter(|a| a.contains('=')) {
                let (k, v) = a.split_once('=').unwrap();
                let val: f64 = v.parse().map_err(|_| format!("{} needs a number", k))?;
                match k {
                    "size_mm" => f.size_mm = val,
                    "offset_x" => f.offset_mm.0 = val,
                    "offset_y" => f.offset_mm.1 = val,
                    "aspect_x" => f.aspect.0 = val,
                    "aspect_y" => f.aspect.1 = val,
                    "negate_x" => f.negate.0 = val != 0.0,
                    "negate_y" => f.negate.1 = val != 0.0,
                    "swap_xy" => f.swap_xy = val != 0.0,
                    _ => return Err(format!("unknown factor {}", k)),
                }
                changed = true;
            }
            if changed {
                f.validate()?;
                f.save(&path)?;
                println!("saved {}", path);
            }
            println!("field      {} mm", f.size_mm);
            println!("offset     ({}, {}) mm", f.offset_mm.0, f.offset_mm.1);
            println!("aspect     ({}, {}) %", f.aspect.0, f.aspect.1);
            println!("negate     ({}, {})  swap {}", f.negate.0, f.negate.1, f.swap_xy);
            println!("counts/mm  {:.3}", 1.0 / f.mm_per_count());
        }
        _ => {
            println!("{}", HELP);
        }
    }
    Ok(())
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|v| format!("{:02x}", v)).collect::<Vec<_>>().join(" ")
}

const HELP: &str = "dbk2jp -- BSL/SeaCAD DBK2JP galvo controller

  dbk2jp devices              list boards
  dbk2jp status               unlock state, inputs, free cache, busy
  dbk2jp inputs [secs]        watch the opto inputs
  dbk2jp unlock               replay the auth frames
  dbk2jp out <port> <0|1>     set an output port
  dbk2jp jump <x> <y>         move the galvos, laser off (0x8000 is centre)
  dbk2jp field [path] [k=v]   scan field: size_mm, offset_x/y, aspect_x/y,
                              negate_x/y, swap_xy
  dbk2jp off                  silence every laser output

Coordinates take decimal or 0x hex.";

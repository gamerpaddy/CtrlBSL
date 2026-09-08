//! dbk2jp: drive a BSL/SeaCAD DBK2JP galvo laser controller directly over USB.
//!
//! ```no_run
//! use dbk2jp_rs::{Board, Job, Speed, PathOpts};
//!
//! let mut job = Job::new(Board::open(0)?, "co2", None)?;
//! job.ensure_unlocked(2);
//! job.configure(Some(20.0), Some(50.0), None, None, None, None)?;
//! job.begin((0x4000, 0x8000), 200)?;
//! job.path(&[(0x4000, 0x8000), (0xC000, 0x8000)], None,
//!          Speed::MmPerSec(600.0), PathOpts::default())?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! THE RULE THAT MATTERS: session/control commands go on EP 0x06; anything that
//! configures a job goes inline in the EP 0x02 batch ahead of the vectors. The
//! board ACKs parameter commands sent on EP 0x06 and then silently ignores them.
//!
//! See API.md for the full surface and DBK2JP_PROTOCOL.md for the reverse
//! engineering behind it.

pub mod field;
pub mod job;
pub mod laser;
pub mod protocol;
pub mod unlock;
pub mod usb;

#[cfg(windows)]
mod backend_win;
#[cfg(all(unix, feature = "libusb"))]
mod backend_libusb;

#[cfg(feature = "python")]
mod python;

pub use field::Field;
pub use job::{Emitted, Job, Limits, PathOpts, Speed, WiggleLoad, MAX_SEGS, SEG_MIN, SEG_TIME};
pub use laser::{Laser, Power};
pub use protocol::{cmd, parse, set_power_0210, set_power_raw};
pub use usb::{Board, BoardError, Transport, VID, PID};

/// Backend dispatch: one place that knows which transport this platform uses.
pub(crate) mod backend {
    #[allow(unused_imports)]
    use crate::usb::{BoardError, Result, Transport};

    pub fn open(path: Option<&str>, index: usize) -> Result<Box<dyn Transport>> {
        #[cfg(windows)]
        {
            return Ok(Box::new(crate::backend_win::WinTransport::open(path, index)?));
        }
        #[cfg(all(unix, feature = "libusb"))]
        {
            return Ok(Box::new(crate::backend_libusb::LibusbTransport::open(
                path, index,
            )?));
        }
        #[cfg(not(any(windows, all(unix, feature = "libusb"))))]
        {
            let _ = (path, index);
            Err(BoardError::Unsupported(
                "build with --features libusb for a non-Windows transport".into(),
            ))
        }
    }

    pub fn find_devices(any_vidpid: bool) -> Vec<String> {
        #[cfg(windows)]
        {
            return crate::backend_win::find_devices(any_vidpid);
        }
        #[cfg(all(unix, feature = "libusb"))]
        {
            return crate::backend_libusb::find_devices(any_vidpid);
        }
        #[cfg(not(any(windows, all(unix, feature = "libusb"))))]
        {
            let _ = any_vidpid;
            Vec::new()
        }
    }
}

/// Every DBK2JP on this machine, as backend-specific path strings.
pub fn find_devices(any_vidpid: bool) -> Vec<String> {
    backend::find_devices(any_vidpid)
}

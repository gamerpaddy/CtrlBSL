//! Linux / macOS transport: plain bulk transfers through libusb.
//!
//! The board's protocol is nothing but bulk transfers on four endpoints, so no
//! driver-specific layer is needed here.
//!
//! One Linux-specific detail, and it bites on the second run rather than the
//! first: the endpoints keep any stalled state across a close, so a job that
//! finishes leaves the pipes halted and the next process enumerates, unlocks
//! and reports status happily while every transfer that matters goes nowhere.
//! So the halts are cleared on open as well as on close, which also recovers
//! from a process that was killed before it could clean up.

use crate::usb::{BoardError, Result, Transport, Xfer, EP_CTRL_IN, EP_CTRL_OUT, EP_DATA_IN, EP_DATA_OUT, PID, VID};
use rusb::{Direction, UsbContext};
use std::time::Duration;

pub fn find_devices(_any_vidpid: bool) -> Vec<String> {
    let ctx = match rusb::Context::new() {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    let mut out = Vec::new();
    if let Ok(list) = ctx.devices() {
        for d in list.iter() {
            if let Ok(desc) = d.device_descriptor() {
                if desc.vendor_id() == VID && desc.product_id() == PID {
                    out.push(format!("usb:{}.{}", d.bus_number(), d.address()));
                }
            }
        }
    }
    out
}

pub struct LibusbTransport {
    handle: rusb::DeviceHandle<rusb::Context>,
    path: String,
}

impl LibusbTransport {
    pub fn open(path: Option<&str>, index: usize) -> Result<Self> {
        let ctx = rusb::Context::new().map_err(|e| BoardError::Open(e.to_string()))?;
        let list = ctx.devices().map_err(|e| BoardError::Open(e.to_string()))?;
        let mut found = Vec::new();
        for d in list.iter() {
            if let Ok(desc) = d.device_descriptor() {
                if desc.vendor_id() == VID && desc.product_id() == PID {
                    found.push(d);
                }
            }
        }
        if found.is_empty() {
            return Err(BoardError::NotFound(format!("VID {:04X} PID {:04X}", VID, PID)));
        }
        let dev = match path {
            Some(p) => {
                let want = p.rsplit(':').next().unwrap_or("").to_string();
                found
                    .into_iter()
                    .find(|d| format!("{}.{}", d.bus_number(), d.address()) == want)
                    .ok_or_else(|| BoardError::NotFound(format!("no board at {}", p)))?
            }
            None => {
                if index >= found.len() {
                    return Err(BoardError::NotFound(format!(
                        "only {} board(s) present",
                        found.len()
                    )));
                }
                found.remove(index)
            }
        };
        let path = format!("usb:{}.{}", dev.bus_number(), dev.address());
        let mut handle = dev.open().map_err(|e| {
            BoardError::Open(format!(
                "{}: {}. On Linux this is usually permissions: add a udev rule for \
                 {:04x}:{:04x} or run as root",
                path, e, VID, PID
            ))
        })?;
        let _ = handle.set_auto_detach_kernel_driver(true);
        let _ = handle.claim_interface(0);
        let mut t = LibusbTransport { handle, path };
        t.clear_halts(); // inherit nothing from whoever had the board last
        Ok(t)
    }

    fn clear_halts(&mut self) {
        for ep in [EP_CTRL_IN, EP_DATA_IN, EP_CTRL_OUT, EP_DATA_OUT] {
            let _ = self.handle.clear_halt(ep);
        }
    }
}

impl Transport for LibusbTransport {
    fn path(&self) -> String {
        self.path.clone()
    }

    fn xfer(&mut self, ep: u8, payload: &[u8], read_len: usize, timeout_ms: u32) -> Result<Xfer> {
        let timeout = Duration::from_millis(timeout_ms as u64);
        if read_len > 0 {
            let mut buf = vec![0u8; read_len];
            match self.handle.read_bulk(ep, &mut buf, timeout) {
                Ok(n) => Ok(Xfer {
                    nt: 0,
                    usbd: 0,
                    data: buf,
                    moved: n,
                }),
                Err(rusb::Error::Timeout) => Err(BoardError::Timeout(format!("{} ms", timeout_ms))),
                Err(e) => Err(BoardError::Transfer(e.to_string())),
            }
        } else {
            match self.handle.write_bulk(ep, payload, timeout) {
                Ok(n) if n == payload.len() => Ok(Xfer {
                    nt: 0,
                    usbd: 0,
                    data: Vec::new(),
                    moved: n,
                }),
                // libusb reports the byte count honestly, so a short write here
                // is a real partial transfer: the board got half a command.
                Ok(n) => Err(BoardError::Transfer(format!(
                    "short write on EP 0x{:02X}: {} of {} bytes",
                    ep,
                    n,
                    payload.len()
                ))),
                Err(rusb::Error::Timeout) => Err(BoardError::Timeout(format!("{} ms", timeout_ms))),
                Err(e) => Err(BoardError::Transfer(e.to_string())),
            }
        }
    }

    fn abort_pipe(&mut self, _ep: u8) -> bool {
        false // no libusb equivalent; clear_halt is the reset below
    }

    fn reset_pipe(&mut self, ep: u8) -> bool {
        let _ = Direction::In; // keep the import honest across rusb versions
        self.handle.clear_halt(ep).is_ok()
    }

    fn close(&mut self) {
        // Before releasing: a stall left on an endpoint survives the close, and
        // the next process silently fails to mark.
        self.clear_halts();
        let _ = self.handle.release_interface(0);
    }
}

impl Drop for LibusbTransport {
    fn drop(&mut self) {
        self.close();
    }
}

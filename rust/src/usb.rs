//! Transport for the DBK2JP (Cypress FX2LP, VID 04B4 PID 1004).
//!
//! One public type, `Board`, on every platform. The platform-specific part is
//! only how bytes reach the endpoints, so that lives behind `Transport`:
//!
//!   backend_win.rs      Windows, the stock CYUSB3 driver's IOCTLs
//!   backend_libusb.rs   everything else, plain bulk transfers
//!
//! Everything above that (command framing, the stale-reply fix, endpoint
//! recovery) is shared and lives here.

use std::fmt;
use std::thread::sleep;
use std::time::Duration;

pub const VID: u16 = 0x04B4;
pub const PID: u16 = 0x1004;

pub const EP_DATA_OUT: u8 = 0x02; // job batch: parameters and geometry
pub const EP_CTRL_OUT: u8 = 0x06; // session/control commands
pub const EP_DATA_IN: u8 = 0x84;
pub const EP_CTRL_IN: u8 = 0x88; // replies

/// A transfer the board refused or never completed.
///
/// Raised rather than returned quietly: a write that fails on a stalled pipe
/// looks exactly like a write that worked, and the board then sits there doing
/// nothing while the host reports success. That failure mode cost a full
/// debugging session on Linux before the endpoints were cleared on open.
#[derive(Debug, Clone)]
pub enum BoardError {
    NotFound(String),
    Open(String),
    Transfer(String),
    Timeout(String),
    Unsupported(String),
}

impl fmt::Display for BoardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BoardError::NotFound(m) => write!(f, "no DBK2JP found: {}", m),
            BoardError::Open(m) => write!(f, "cannot open board: {}", m),
            BoardError::Transfer(m) => write!(f, "transfer failed: {}", m),
            BoardError::Timeout(m) => write!(f, "transfer timed out: {}", m),
            BoardError::Unsupported(m) => write!(f, "unsupported: {}", m),
        }
    }
}

impl std::error::Error for BoardError {}

pub type Result<T> = std::result::Result<T, BoardError>;

/// One transfer's outcome: the two driver status words exist to match the
/// Windows backend's shape and are 0 elsewhere.
pub struct Xfer {
    pub nt: u32,
    pub usbd: u32,
    pub data: Vec<u8>,
    pub moved: usize,
}

pub trait Transport: Send {
    fn path(&self) -> String;
    fn xfer(&mut self, ep: u8, payload: &[u8], read_len: usize, timeout_ms: u32) -> Result<Xfer>;
    fn abort_pipe(&mut self, ep: u8) -> bool;
    fn reset_pipe(&mut self, ep: u8) -> bool;
    fn close(&mut self);
}

pub struct Board {
    t: Box<dyn Transport>,
}

impl Board {
    /// Open the board at `index` among those present.
    pub fn open(index: usize) -> Result<Self> {
        Ok(Board {
            t: crate::backend::open(None, index)?,
        })
    }

    /// Open a specific board by backend path.
    pub fn open_path(path: &str) -> Result<Self> {
        Ok(Board {
            t: crate::backend::open(Some(path), 0)?,
        })
    }

    /// Wrap an already-built transport, which is what the tests use.
    pub fn with_transport(t: Box<dyn Transport>) -> Self {
        Board { t }
    }

    pub fn path(&self) -> String {
        self.t.path()
    }

    pub fn close(&mut self) {
        self.t.close();
    }

    // ---- raw ------------------------------------------------------------

    pub fn xfer(&mut self, ep: u8, payload: &[u8], read_len: usize, timeout_ms: u32) -> Result<Xfer> {
        self.t.xfer(ep, payload, read_len, timeout_ms)
    }

    pub fn abort_pipe(&mut self, ep: u8) -> bool {
        self.t.abort_pipe(ep)
    }

    pub fn reset_pipe(&mut self, ep: u8) -> bool {
        self.t.reset_pipe(ep)
    }

    /// Clear any stalled or pending state on all four endpoints.
    pub fn recover(&mut self) {
        for ep in [EP_CTRL_IN, EP_DATA_IN, EP_CTRL_OUT, EP_DATA_OUT] {
            self.t.abort_pipe(ep);
            self.t.reset_pipe(ep);
        }
    }

    // ---- framed ---------------------------------------------------------

    fn check(nt: u32, usbd: u32, ep: u8, nbytes: usize) -> Result<()> {
        // `nt` is an NTSTATUS from the Windows backend and 0 everywhere else, so
        // any non-zero value is a real failure. The byte count is deliberately
        // not checked: the CYUSB3 IOCTL does not report a write length the same
        // way on every driver build.
        if nt != 0 {
            return Err(BoardError::Transfer(format!(
                "{} bytes on EP 0x{:02X}: NTSTATUS 0x{:08X}, USBD 0x{:08X}. The pipe is \
                 probably stalled, call recover()",
                nbytes, ep, nt, usbd
            )));
        }
        Ok(())
    }

    /// One 12-byte command on EP 0x06.
    pub fn write_cmd(&mut self, cmd: &[u8]) -> Result<usize> {
        assert_eq!(cmd.len(), 12, "a command is 12 bytes");
        let x = self.t.xfer(EP_CTRL_OUT, cmd, 0, 3000)?;
        Board::check(x.nt, x.usbd, EP_CTRL_OUT, cmd.len())?;
        Ok(x.moved)
    }

    /// Batched 12-byte commands on EP 0x02.
    pub fn write_data(&mut self, blob: &[u8]) -> Result<usize> {
        assert!(
            !blob.is_empty() && blob.len() % 12 == 0,
            "a batch is whole 12-byte commands"
        );
        let x = self.t.xfer(EP_DATA_OUT, blob, 0, 3000)?;
        Board::check(x.nt, x.usbd, EP_DATA_OUT, blob.len())?;
        Ok(x.moved)
    }

    pub fn read_status(&mut self, ep: u8, timeout_ms: u32) -> Result<Vec<u8>> {
        let x = self.t.xfer(ep, &[], 12, timeout_ms)?;
        Ok(x.data)
    }

    /// Send a command on EP 0x06 and return ITS reply, not a stale one.
    ///
    /// EP 0x88 runs one behind after a burst of mixed opcodes, so a plain
    /// write-then-read hands back the previous command's answer. Matching the
    /// echoed opcode and discarding mismatches is the fix.
    ///
    /// Returns None when the board does not answer: the status helpers above
    /// this all treat that as "unknown" rather than as a failure, so a refused
    /// write is reported the same way. The data path keeps raising, because
    /// geometry that never lands must not look like geometry that did.
    pub fn ask(&mut self, cmd: &[u8], timeout_ms: u32, retries: u32, settle_s: f64) -> Option<Vec<u8>> {
        if self.write_cmd(cmd).is_err() {
            return None;
        }
        sleep(Duration::from_secs_f64(settle_s));
        let mut recovered = false;
        for _ in 0..retries.max(1) {
            match self.read_status(EP_CTRL_IN, timeout_ms) {
                Ok(r) => {
                    if r.len() >= 2 && r[0] == cmd[0] && r[1] == cmd[1] {
                        return Some(r);
                    }
                    sleep(Duration::from_millis(10));
                }
                Err(_) => {
                    // A timed-out read usually means the reply pipe is halted,
                    // and every later read on it fails the same way. Clear it
                    // once and use the remaining attempts.
                    if recovered {
                        return None;
                    }
                    recovered = true;
                    self.recover();
                    if self.write_cmd(cmd).is_err() {
                        return None;
                    }
                    sleep(Duration::from_secs_f64(settle_s));
                }
            }
        }
        None
    }
}

impl Drop for Board {
    fn drop(&mut self) {
        self.t.close();
    }
}

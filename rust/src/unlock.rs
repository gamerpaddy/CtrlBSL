//! Unlock the DBK2JP board (the encryption LED red -> green).
//!
//! The board carries an Atmel ATSHA204A authentication chip reached through the
//! 0x0C5C / 0x0C5D passthrough on EP 0x06. The host computes a SHA-256 digest
//! and hands it to the FPGA; the chip computes the same MAC in hardware; the
//! FPGA compares them and ungates.
//!
//! No key is needed to replay: the challenge is host-supplied, the key travels
//! in the clear and the chip is on the board, so replaying the captured frames
//! makes the real chip produce the real answer again. Vendor captures of both
//! BslApp and LightBurn run the identical exchange with their own challenge
//! bytes, so it works with any of them.
//!
//! Success is read from 0x0102 (GetEncryptState) byte 7: 2 = authenticated.
//! 0x0101 bit 5 is a separate ready/arm flag that moves without the crypto.
//!
//! The latch is sticky: once green it survives everything short of a power
//! cycle, so a repeat costs nothing.

use crate::protocol::cmd;
use crate::usb::{Board, EP_CTRL_IN, EP_CTRL_OUT};
use std::thread::sleep;
use std::time::Duration;

/// (gap_after_seconds, frame_hex) -- EP 0x06 writes, each answered on EP 0x88.
///
/// Frames 10, 11 and 12 are the whole unlock: the host-computed SHA-256
/// digest on 0x0C5D, the MAC command on 0x0C5C, then the transmit token.
/// The gaps are load-bearing: the ATSHA204 needs 40-120 ms per command and
/// answers from stale registers when it is rushed.
pub const FRAMES: [(f64, &str); 74] = [
    (0.010, "014000000000000000000001"),
    (0.057, "011000000000000000000000"),
    (0.061, "0C5C77070280000009AD"),
    (0.071, "0C5C8888"),
    (0.010, "010800000000000000000000"),
    (0.010, "010900000000000000000000"),
    (0.010, "010A00000000000000000000"),
    (0.051, "010B00000000000000000000"),
    (0.113, "0C5CBB"),
    (0.063, "0C5CBB"),
    (0.102, "0C5D000009D6956A72D6622E779074242405FC5CBFEE96B17460B9AF8CC7323408A05959"),
    (0.060, "0C5C77270800000036215A9F208A3A8C1CA354D8E78898392CC22F256E35C7BC79C23842FBB3D9CD29E2"),
    (0.081, "0C5C8888"),
    (0.010, "010800000000000000000000"),
    (0.010, "010900000000000000000000"),
    (0.010, "010A00000000000000000000"),
    (0.051, "010B00000000000000000000"),
    (0.063, "0C5CBB"),
    (0.053, "010200000000000000000000"),
    (0.060, "0C5C77070282200009B0"),
    (0.071, "0C5C8888"),
    (0.010, "010800000000000000000000"),
    (0.010, "010900000000000000000000"),
    (0.010, "010A00000000000000000000"),
    (0.050, "010B00000000000000000000"),
    (0.113, "0C5CBB"),
    (0.123, "0C5CBB"),
    (0.061, "0C5C77070282200009B0"),
    (0.071, "0C5C8888"),
    (0.010, "010800000000000000000000"),
    (0.010, "010900000000000000000000"),
    (0.010, "010A00000000000000000000"),
    (0.051, "010B00000000000000000000"),
    (0.113, "0C5CBB"),
    (0.113, "0C5CBB"),
    (0.061, "0C5C7707028230000A00"),
    (0.071, "0C5C8888"),
    (0.010, "010800000000000000000000"),
    (0.010, "010900000000000000000000"),
    (0.010, "010A00000000000000000000"),
    (0.051, "010B00000000000000000000"),
    (0.114, "0C5CBB"),
    (0.125, "0C5CBB"),
    (0.060, "0C5C7707028230000A00"),
    (0.070, "0C5C8888"),
    (0.010, "010800000000000000000000"),
    (0.010, "010900000000000000000000"),
    (0.010, "010A00000000000000000000"),
    (0.050, "010B00000000000000000000"),
    (0.114, "0C5CBB"),
    (0.113, "0C5CBB"),
    (0.060, "0C5C77070282380009E0"),
    (0.071, "0C5C8888"),
    (0.010, "010800000000000000000000"),
    (0.010, "010900000000000000000000"),
    (0.010, "010A00000000000000000000"),
    (0.050, "010B00000000000000000000"),
    (0.113, "0C5CBB"),
    (0.123, "0C5CBB"),
    (0.061, "0C5C77070282380009E0"),
    (0.071, "0C5C8888"),
    (0.010, "010800000000000000000000"),
    (0.010, "010900000000000000000000"),
    (0.010, "010A00000000000000000000"),
    (0.051, "010B00000000000000000000"),
    (0.112, "0C5CBB"),
    (0.062, "0C5CBB"),
    (0.010, "010100000000000000000000"),
    (0.010, "010600000000000000000000"),
    (0.010, "010500000000000000000000"),
    (0.010, "010400000000000000000000"),
    (0.010, "010500000000000000000000"),
    (0.010, "011800000000000000000000"),
    (0.010, "010500000000000000000000"),
];

/// The EP 0x02 blob that goes out after frame 70 in the arm tail.
pub const EP2_AFTER: usize = 70;
pub const EP2_BLOB: &str = "0241271080008000000001F4";

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap_or(0))
        .collect()
}

/// The three frames that are enough on their own: digest, MAC, token.
pub fn minimal() -> Vec<usize> {
    vec![10, 11, 12]
}

/// Everything captured, for debugging.
pub fn full() -> Vec<usize> {
    (0..FRAMES.len()).collect()
}

/// Named frame sets, each verified to work.
pub fn set(name: &str) -> Option<Vec<usize>> {
    Some(match name {
        "bare" => minimal(),
        "rb" => (10..18).collect(),
        "wake" => [8, 9].into_iter().chain(10..18).collect(),
        "core" => [0, 1, 8, 9].into_iter().chain(10..18).collect(),
        "auth" => (0..18).collect(),
        "min" => (0..18).chain(67..74).collect(),
        "full" => full(),
        "tailonly" => (67..74).collect(),
        _ => return None,
    })
}

/// Replay the arm sequence. True if the board then reports unlocked.
pub fn unlock(b: &mut Board, indices: Option<&[usize]>, ep2: bool) -> bool {
    let idx: Vec<usize> = match indices {
        Some(v) => v.to_vec(),
        None => minimal(),
    };
    for i in idx {
        let (gap, hx) = FRAMES[i];
        let _ = b.xfer(EP_CTRL_OUT, &unhex(hx), 0, 2000);
        sleep(Duration::from_secs_f64(gap * 0.6));
        let _ = b.read_status(EP_CTRL_IN, 800);
        sleep(Duration::from_secs_f64(gap * 0.4));
        if ep2 && i == EP2_AFTER {
            let _ = b.write_data(&unhex(EP2_BLOB));
        }
    }
    encrypt_state(b) == Some(2)
}

/// 0x0102 GetEncryptState byte 7: 2 = authenticated, 0 = not.
/// None when the reply is missing or short, so an unreadable board is
/// distinguishable from a locked one.
pub fn encrypt_state(b: &mut Board) -> Option<u8> {
    if b.write_cmd(&cmd(0x0102, 0, 0, 0, 0, 0)).is_err() {
        return None;
    }
    sleep(Duration::from_millis(60));
    match b.read_status(EP_CTRL_IN, 1500) {
        Ok(st) if st.len() >= 8 => Some(st[7]),
        _ => None,
    }
}

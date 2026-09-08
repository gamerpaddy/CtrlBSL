//! Wire format and parameter packing for the DBK2JP.
//!
//! Every command is a 12-byte big-endian tagSeaCMD: CMD_ID then five params.
//! No header, no checksum, no sequence number.

// Session / control -- EP 0x06.
pub const CMD_STATUS: u16 = 0x0101;
pub const CMD_ENCRYPT_STATE: u16 = 0x0102;
pub const CMD_RUN: u16 = 0x0104;
pub const CMD_RESET: u16 = 0x0105;
pub const CMD_CLEAR_CACHE: u16 = 0x0106;
pub const CMD_PORT_OUT: u16 = 0x0111;
pub const CMD_PORT_OUT_STATE: u16 = 0x0112;
pub const CMD_ONLINE: u16 = 0x0118;
pub const CMD_REDLIGHT: u16 = 0x0171;

// Job parameters and geometry -- MUST go inline in the EP 0x02 batch.
pub const CMD_LASER_TYPE: u16 = 0x0211;
pub const CMD_POWER: u16 = 0x0210;
pub const CMD_TICK: u16 = 0x0217;
pub const CMD_PEN_FPK: u16 = 0x0218;
pub const CMD_LASER_GATE: u16 = 0x0208;
pub const CMD_JUMP: u16 = 0x0241;
pub const CMD_MARK: u16 = 0x0243;
pub const CMD_AXIS_COUNT: u16 = 0x0230;
pub const CMD_AXIS_RATE: u16 = 0x0231;
pub const CMD_AXIS_P232: u16 = 0x0232;
pub const CMD_AXIS_GO: u16 = 0x0233;
pub const CMD_PORT_PULSE: u16 = 0x2F82;

/// 0x0211 Param1 bit 8: the MO / PA enable, outside the documented flag list.
/// With it clear both pins stay low however long the engine runs.
pub const MO_ENABLE: u16 = 0x0100;

/// 0x0206 with Param0 = 0xA501 is a four byte SPI frame, not a selector plus a
/// value: 0xA5 0x01 then Param1 big-endian, shifted out on P1 (data) and
/// P2 (clock). Param1 is the pulse width in NANOSECONDS.
pub const MOPA_SPI_HDR: u16 = 0xA501;
/// Those two SPI lines are also bits 1 and 2 of the parallel power word.
pub const MOPA_SPI_MASK: u8 = 0x06;

/// Timebase for every period/width field.
pub const FPGA_CLK_KHZ: f64 = 48000.0;

/// Build a 12-byte tagSeaCMD. All fields big-endian.
pub fn cmd(cmd_id: u16, p0: u16, p1: u16, p2: u16, p3: u16, p4: u16) -> [u8; 12] {
    let mut out = [0u8; 12];
    for (i, v) in [cmd_id, p0, p1, p2, p3, p4].iter().enumerate() {
        out[i * 2] = (v >> 8) as u8;
        out[i * 2 + 1] = (*v & 0xFF) as u8;
    }
    out
}

/// Split a 12-byte reply into (cmd_id, p0, p1, p2, p3, p4).
pub fn parse(reply: &[u8]) -> Option<[u16; 6]> {
    if reply.len() < 12 {
        return None;
    }
    let mut out = [0u16; 6];
    for i in 0..6 {
        out[i] = ((reply[i * 2] as u16) << 8) | reply[i * 2 + 1] as u16;
    }
    Some(out)
}

/// 0x0206 frame carrying a MOPA pulse width in nanoseconds.
pub fn mopa_pulse_ns(ns: u32) -> [u8; 12] {
    cmd(0x0206, MOPA_SPI_HDR, (ns & 0xFFFF) as u16, 0, 0, 0)
}

fn pack_power(freq_khz: f64, power_byte: u8, width_us: f64, wait: u16) -> ([u8; 12], u32, u32) {
    // +0x04 Param1 = (period >> 8) & 0xFF | (round(freq*0.5) << 8)
    // +0x06 Param2 = (width  >> 8)        | ((period & 0xFF) << 8)
    // +0x08 Param3 = power byte           | ((width  & 0xFF) << 8)
    // period and width are 48 MHz ticks, each split across two params.
    let period = (FPGA_CLK_KHZ / freq_khz).round() as u32;
    let width = ((width_us * 48.0).round() as i64 & 0xFFFF) as u32;
    let fbyte = ((freq_khz * 0.5).round() as i64 & 0xFF) as u16;
    let p1 = (((period >> 8) & 0xFF) as u16) | (fbyte << 8);
    let p2 = (((width >> 8) & 0xFF) as u16) | (((period & 0xFF) as u16) << 8);
    let p3 = (power_byte as u16) | (((width & 0xFF) as u16) << 8);
    (cmd(CMD_POWER, 0, p1, p2, p3, wait), period, width)
}

/// CMD 0x0210, the marking power/frequency word, power as a percentage.
/// Returns (bytes, period_ticks, width_ticks).
pub fn set_power_0210(freq_khz: f64, power_pct: f64, wait: u16) -> ([u8; 12], u32, u32) {
    let duty_us = (power_pct / 100.0) * (1000.0 / freq_khz);
    let pbyte = ((power_pct * 255.0) as i64 / 100) as u8;
    pack_power(freq_khz, pbyte, duty_us, wait)
}

/// 0x0210 with an EXPLICIT 8-bit power word instead of a percentage.
///
/// For fiber that byte appears on the parallel power pins P0..P7. `duty_pct` is
/// the PWM duty on the MARKING line and defaults to 0 for a reason: this command
/// programs the marking PWM generator whatever else it is used for, so a
/// non-zero default puts a live modulated signal on the laser control pin.
pub fn set_power_raw(freq_khz: f64, power_byte: u8, wait: u16, duty_pct: f64) -> [u8; 12] {
    let duty_us = (duty_pct / 100.0) * (1000.0 / freq_khz);
    pack_power(freq_khz, power_byte, duty_us, wait).0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_is_big_endian() {
        assert_eq!(
            cmd(0x0243, 1495, 0x9C45, 0x952B, 0, 10),
            [0x02, 0x43, 0x05, 0xD7, 0x9C, 0x45, 0x95, 0x2B, 0x00, 0x00, 0x00, 0x0A]
        );
    }

    #[test]
    fn parse_round_trips() {
        let c = cmd(0x0101, 1, 2, 3, 4, 5);
        assert_eq!(parse(&c).unwrap(), [0x0101, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn mopa_frame_is_a5_01_then_the_width() {
        assert_eq!(&mopa_pulse_ns(100)[..6], &[0x02, 0x06, 0xA5, 0x01, 0x00, 0x64]);
    }

    #[test]
    fn power_matches_the_python_packing() {
        // 20 kHz, 50 %: period 2400 ticks, width 1200 ticks.
        let (bytes, period, width) = set_power_0210(20.0, 50.0, 0);
        assert_eq!((period, width), (2400, 1200));
        let f = parse(&bytes).unwrap();
        assert_eq!(f[0], 0x0210);
        assert_eq!(f[2], ((period >> 8) & 0xFF) as u16 | (10u16 << 8));
        assert_eq!(f[4] & 0xFF, 127); // (50*255)/100
    }
}

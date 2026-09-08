//! Laser selection: what a laser type is and how it wants to be driven.
//!
//! The type code is the high byte of 0x0211 Param0.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Power {
    /// Power is PWM duty on the marking line.
    Pwm,
    /// Power is an 8-bit parallel word on P0..P7, latched.
    Byte,
}

#[derive(Clone, Debug)]
pub struct Laser {
    pub name: &'static str,
    pub code: u8,
    pub power: Power,
    pub freq_khz: f64,
    pub freq_range: (f64, f64),
    pub tickle: bool,
    pub tick_khz: f64,
    pub tick_us: f64,
    pub tick_range: (f64, f64),
    pub mopa_pulse: bool,
    pub verified: bool,
    pub note: &'static str,
}

const fn base(name: &'static str, code: u8, power: Power, freq_khz: f64) -> Laser {
    Laser {
        name,
        code,
        power,
        freq_khz,
        freq_range: (1.0, 40.0),
        tickle: false,
        tick_khz: 5.0,
        tick_us: 1.0,
        tick_range: (0.74, 100.0),
        mopa_pulse: false,
        verified: false,
        note: "",
    }
}

pub fn co2() -> Laser {
    Laser {
        tickle: true,
        verified: true,
        note: "PWM duty is the power. Tickle is on by default: a CO2 tube wants \
               priming between marks, from a separate free-running generator.",
        ..base("co2", 0x22, Power::Pwm, 20.0)
    }
}

pub fn fiber() -> Laser {
    Laser {
        verified: true,
        note: "Power is the 8-bit word on P0..P7, static and latched. No marking \
               run needed to set it.",
        ..base("fiber", 0x11, Power::Byte, 20.0)
    }
}

pub fn uv() -> Laser {
    base("uv", 0x33, Power::Pwm, 30.0)
}

pub fn green() -> Laser {
    base("green", 0x44, Power::Pwm, 30.0)
}

pub fn mopa() -> Laser {
    Laser {
        mopa_pulse: true,
        note: "WARNING: on the board tested, type code 0x55 mutes every laser \
               output while the same job under fiber 0x11 drives all four pins. \
               Drive a MOPA source as FIBER and set the pulse width.",
        ..base("mopa", 0x55, Power::Byte, 30.0)
    }
}

pub fn yag() -> Laser {
    Laser {
        note: "Type code is a guess: the only unused low nibble, never confirmed.",
        ..base("yag", 0x00, Power::Pwm, 20.0)
    }
}

pub fn all() -> Vec<Laser> {
    vec![co2(), fiber(), uv(), green(), mopa(), yag()]
}

/// Accept a name ("co2"), or a raw type code as "0x22".
pub fn get(kind: &str) -> Result<Laser, String> {
    let k = kind.trim().to_ascii_lowercase();
    for l in all() {
        if l.name == k {
            return Ok(l);
        }
    }
    let code = if let Some(hex) = k.strip_prefix("0x") {
        u8::from_str_radix(hex, 16).ok()
    } else {
        k.parse::<u8>().ok()
    };
    if let Some(code) = code {
        return Ok(get_code(code));
    }
    Err(format!(
        "unknown laser {:?}, pick one of: co2, fiber, green, mopa, uv, yag",
        kind
    ))
}

/// A laser by raw type code, falling back to a plain PWM description.
pub fn get_code(code: u8) -> Laser {
    for l in all() {
        if l.code == code {
            return l;
        }
    }
    Laser {
        code,
        ..base("custom", code, Power::Pwm, 20.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_codes_resolve() {
        assert_eq!(get("CO2").unwrap().code, 0x22);
        assert_eq!(get("fiber").unwrap().power, Power::Byte);
        assert_eq!(get("0x33").unwrap().name, "uv");
        assert!(get("plasma").is_err());
    }
}

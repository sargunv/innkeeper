//! Host state service messages (id 8, types 0x0801..=0x08ff, events
//! 0x4801..=0x48ff). The clients are the Health HAL, for the battery, and
//! the Innkeeper System app, for the power saver, network and location. A
//! client sends `START` with the state it wants. The current value of each
//! follows as an event, and so does every later change.
//!
//! | Type     | Request        | Body                                                          | Reply                                                     |
//! | -------- | -------------- | ------------------------------------------------------------- | --------------------------------------------------------- |
//! | `0x0801` | START          | `u32 features` (1 battery, 2 power saver, 4 network, 8 location) | `u32` features served; current state follows as events |
//! | `0x0802` | LOCATION_START | `u8 precise`, `u8 0`, `u16 0`                                  | `-ENOSYS` without location; fixes and status follow       |
//! | `0x0803` | LOCATION_STOP  |                                                               |                                                           |
//!
//! | Event    | Message         | Body                                                                                                                                         |
//! | -------- | --------------- | -------------------------------------------------------------------------------------------------------------------------------------------- |
//! | `0x4801` | BATTERY         | `u8 present`, `u8 state` (UPower: 0 unknown, 1 charging, 2 discharging, 3 empty, 4 full, 5 pending charge, 6 pending discharge), `u8 ac`, `u8 level` (0..=100), `u32 time_to_empty` (s, 0 unknown), `u32 time_to_full` (s) |
//! | `0x4802` | POWER_SAVER     | `u8 on`, `u8 0`, `u16 0`                                                                                                                      |
//! | `0x4803` | NETWORK         | `u8 available` (0 unknown, 1 no, 2 yes), `u8 metered` (0 unknown, 1 no, 2 yes), `u8 connectivity` (0 unknown, 1 none, 2 local, 3 limited, 4 captive portal, 5 full), `u8 0` |
//! | `0x4804` | LOCATION        | `f64 latitude`, `f64 longitude`, `f64 altitude` (NaN unknown), `f32 accuracy` (m), `f32 speed` (m/s), `f32 bearing` (degrees), all three -1 unknown, `u64 time` (ms since the epoch) |
//! | `0x4805` | LOCATION_STATUS | `u8 status` (1 running, 2 denied, 3 disabled on the desktop, 4 unavailable), `u8 0`, `u16 0`                                                  |
//!
//! The bridge asks the desktop for location only on `LOCATION_START`. The
//! system app sends it once an app that Android allowed asks for a location.
//! A denial (status 2) or location turned off on the desktop (3) holds for
//! the bridge's lifetime and is not retried through another backend.

use crate::ipc::wire::{DecodeError, Reader, Writer};

pub mod ty {
    pub const START: u16 = 0x0801;
    pub const LOCATION_START: u16 = 0x0802;
    pub const LOCATION_STOP: u16 = 0x0803;
}

pub mod ev {
    pub const BATTERY: u16 = 0x4801;
    pub const POWER_SAVER: u16 = 0x4802;
    pub const NETWORK: u16 = 0x4803;
    pub const LOCATION: u16 = 0x4804;
    pub const LOCATION_STATUS: u16 = 0x4805;
}

/// `START` feature bits.
pub mod feature {
    pub const BATTERY: u32 = 1 << 0;
    pub const POWER_SAVER: u32 = 1 << 1;
    pub const NETWORK: u32 = 1 << 2;
    pub const LOCATION: u32 = 1 << 3;
    pub const ALL: u32 = BATTERY | POWER_SAVER | NETWORK | LOCATION;
}

/// `LOCATION_STATUS` values.
pub mod status {
    pub const RUNNING: u8 = 1;
    pub const DENIED: u8 = 2;
    pub const DISABLED: u8 = 3;
    pub const UNAVAILABLE: u8 = 4;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Start { features: u32 },
    LocationStart { precise: bool },
    LocationStop,
}

pub fn decode(t: u16, body: &[u8]) -> Result<Request, DecodeError> {
    let mut r = Reader::new(body);
    let req = match t {
        ty::START => Request::Start { features: r.u32()? },
        ty::LOCATION_START => {
            let precise = r.bool()?;
            if r.u8()? != 0 || r.u16()? != 0 {
                return Err(DecodeError::Invalid("reserved"));
            }
            Request::LocationStart { precise }
        }
        ty::LOCATION_STOP => Request::LocationStop,
        t => return Err(DecodeError::UnknownType(t)),
    };
    r.end()?;
    Ok(req)
}

pub fn encode(tag: u32, req: &Request) -> Vec<u8> {
    let w = match *req {
        Request::Start { features } => {
            let mut w = Writer::message(ty::START, tag);
            w.u32(features);
            w
        }
        Request::LocationStart { precise } => {
            let mut w = Writer::message(ty::LOCATION_START, tag);
            w.u8(precise as u8).u8(0).u16(0);
            w
        }
        Request::LocationStop => Writer::message(ty::LOCATION_STOP, tag),
    };
    w.into_vec()
}

/// The host's battery, from UPower's `DisplayDevice`, which is the composite
/// of every battery, and from `OnBattery`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Battery {
    /// Whether a battery exists. Without one the host is on AC and full.
    pub present: bool,
    /// UPower's `State`.
    pub state: u8,
    /// On line power (`!OnBattery`).
    pub ac: bool,
    /// Percent, 0 to 100.
    pub level: u8,
    pub time_to_empty: u32,
    pub time_to_full: u32,
}

impl Battery {
    /// What a host without a battery reports, and what is reported before
    /// UPower answers.
    pub const AC_ONLY: Battery = Battery {
        present: false,
        state: 0,
        ac: true,
        level: 100,
        time_to_empty: 0,
        time_to_full: 0,
    };

    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::default();
        w.u8(self.present as u8)
            .u8(self.state)
            .u8(self.ac as u8)
            .u8(self.level)
            .u32(self.time_to_empty)
            .u32(self.time_to_full);
        w.into_vec()
    }

    pub fn decode(body: &[u8]) -> Result<Battery, DecodeError> {
        let mut r = Reader::new(body);
        let b = Battery {
            present: r.bool()?,
            state: r.u8()?,
            ac: r.bool()?,
            level: r.u8()?,
            time_to_empty: r.u32()?,
            time_to_full: r.u32()?,
        };
        r.end()?;
        if b.state > 6 || b.level > 100 {
            return Err(DecodeError::Invalid("battery"));
        }
        Ok(b)
    }
}

/// The host's network, from the NetworkMonitor portal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Network {
    pub available: u8,
    pub metered: u8,
    pub connectivity: u8,
}

impl Network {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::default();
        w.u8(self.available)
            .u8(self.metered)
            .u8(self.connectivity)
            .u8(0);
        w.into_vec()
    }

    pub fn decode(body: &[u8]) -> Result<Network, DecodeError> {
        let mut r = Reader::new(body);
        let n = Network {
            available: r.u8()?,
            metered: r.u8()?,
            connectivity: r.u8()?,
        };
        if r.u8()? != 0 {
            return Err(DecodeError::Invalid("reserved"));
        }
        r.end()?;
        if n.available > 2 || n.metered > 2 || n.connectivity > 5 {
            return Err(DecodeError::Invalid("network"));
        }
        Ok(n)
    }
}

/// One location fix.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fix {
    pub latitude: f64,
    pub longitude: f64,
    /// Meters above WGS 84, or NaN if unknown.
    pub altitude: f64,
    /// Horizontal accuracy in meters, or -1 if unknown.
    pub accuracy: f32,
    /// Speed in m/s, or -1 if unknown.
    pub speed: f32,
    /// Degrees from north in `[0, 360)`, or -1 if unknown.
    pub bearing: f32,
    /// Milliseconds since the Unix epoch.
    pub time_ms: u64,
}

impl Fix {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::default();
        w.u64(self.latitude.to_bits())
            .u64(self.longitude.to_bits())
            .u64(self.altitude.to_bits())
            .f32(self.accuracy)
            .f32(self.speed)
            .f32(self.bearing)
            .u64(self.time_ms);
        w.into_vec()
    }

    pub fn decode(body: &[u8]) -> Result<Fix, DecodeError> {
        let mut r = Reader::new(body);
        let f = Fix {
            latitude: f64::from_bits(r.u64()?),
            longitude: f64::from_bits(r.u64()?),
            altitude: f64::from_bits(r.u64()?),
            accuracy: r.f32()?,
            speed: r.f32()?,
            bearing: r.f32()?,
            time_ms: r.u64()?,
        };
        r.end()?;
        if !f.valid() {
            return Err(DecodeError::Invalid("fix"));
        }
        Ok(f)
    }

    /// Whether the coordinates are in range and each optional field is
    /// either in range or unknown (-1, or NaN for altitude).
    pub fn valid(&self) -> bool {
        let opt = |v: f32, max: f32| v == -1.0 || (v.is_finite() && (0.0..max).contains(&v));
        (-90.0..=90.0).contains(&self.latitude)
            && (-180.0..=180.0).contains(&self.longitude)
            && (self.altitude.is_nan() || self.altitude.is_finite())
            && opt(self.accuracy, 1e7)
            && opt(self.speed, 1e4)
            && opt(self.bearing, 360.0)
    }
}

/// Encodes a `u8` value followed by three reserved zero bytes, as
/// POWER_SAVER and LOCATION_STATUS use.
pub fn encode_u8(v: u8) -> Vec<u8> {
    vec![v, 0, 0, 0]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::wire::{HDR, decode_header};

    #[test]
    fn requests() {
        for r in [
            Request::Start {
                features: feature::ALL,
            },
            Request::LocationStart { precise: true },
            Request::LocationStop,
        ] {
            let b = encode(3, &r);
            let h = decode_header(&b).unwrap();
            assert_eq!(decode(h.ty, &b[HDR..]), Ok(r));
        }
        assert!(decode(ty::LOCATION_START, &[2, 0, 0, 0]).is_err());
        assert!(decode(ty::LOCATION_START, &[1, 1, 0, 0]).is_err());
        assert!(decode(ty::LOCATION_STOP, &[0]).is_err());
        assert_eq!(decode(0x0804, &[]), Err(DecodeError::UnknownType(0x0804)));
    }

    #[test]
    fn events() {
        let b = Battery {
            present: true,
            state: 2,
            ac: false,
            level: 42,
            time_to_empty: 3600,
            time_to_full: 0,
        };
        assert_eq!(Battery::decode(&b.encode()), Ok(b));
        assert_eq!(b.encode().len(), 12);
        assert_eq!(
            Battery::decode(&Battery::AC_ONLY.encode()),
            Ok(Battery::AC_ONLY)
        );
        let n = Network {
            available: 2,
            metered: 2,
            connectivity: 5,
        };
        assert_eq!(Network::decode(&n.encode()), Ok(n));
        let f = Fix {
            latitude: 52.52,
            longitude: 13.405,
            altitude: f64::NAN,
            accuracy: 20.0,
            speed: -1.0,
            bearing: -1.0,
            time_ms: 1_800_000_000_000,
        };
        let d = Fix::decode(&f.encode()).unwrap();
        assert_eq!(f.encode(), d.encode());
        assert_eq!(f.encode().len(), 44);
        assert!(
            !Fix {
                latitude: 91.0,
                ..f
            }
            .valid()
        );
        assert!(
            !Fix {
                bearing: 360.0,
                ..f
            }
            .valid()
        );
        assert!(
            !Fix {
                accuracy: f32::NAN,
                ..f
            }
            .valid()
        );
    }
}

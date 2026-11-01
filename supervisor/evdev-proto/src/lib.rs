//! The evdev.sock protocol. The desktop bridge, which is the only writer,
//! sends input for the broker's session-local evdev devices to
//! `/dev/socket/innkeeper/evdev.sock`, one datagram per [`Message`]. Fields
//! are little-endian, and the records are x86_64 Linux `input_event`s
//! ([`event`]).

/// One `input_event` record's size.
pub const EVENT_SIZE: usize = 24;
/// The keyboard and mouse devices.
pub const KEYBOARD: u32 = 0;
pub const MOUSE: u32 = 1;
/// The touchscreen, which shows the bridge's own windows (DISPLAY_WINDOW) to
/// a guest without a window manager on the input service. Its axes span
/// [`touch::MAX`] across the screen, whatever the screen's size.
pub const TOUCHSCREEN: u32 = gamepad::FIRST - 1;

pub mod touch {
    /// The largest value of the touchscreen's X and Y axes.
    pub const MAX: i32 = 32767;
    /// The number of multitouch slots.
    pub const SLOTS: usize = 10;
}

/// The canonical evdev layout that Android's Generic.kl consumes. The bridge
/// maps host axes to it. FF_RUMBLE keeps the Linux `ff_effect` ABI.
pub mod gamepad {
    /// The first gamepad device id.
    pub const FIRST: u32 = 0x10000;
    pub const AXES: [u16; 8] = [0, 1, 2, 5, 9, 10, 16, 17];
    pub const BUTTONS: [u16; 11] = [304, 305, 307, 308, 314, 316, 315, 317, 318, 310, 311];
    pub const EFFECT_SIZE: usize = 48;
}

/// An x86_64 Linux `input_event` with a CLOCK_MONOTONIC timeval. It is built
/// field by field because Rust struct padding is not part of the protocol.
pub fn event(micros: i64, ty: u16, code: u16, value: i32) -> [u8; EVENT_SIZE] {
    let mut b = [0; EVENT_SIZE];
    b[..8].copy_from_slice(&(micros / 1_000_000).to_le_bytes());
    b[8..16].copy_from_slice(&(micros % 1_000_000).to_le_bytes());
    b[16..18].copy_from_slice(&ty.to_le_bytes());
    b[18..20].copy_from_slice(&code.to_le_bytes());
    b[20..].copy_from_slice(&value.to_le_bytes());
    b
}

/// One evdev.sock datagram.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message<'a> {
    /// `u32 device` + `input_event` records, the last a SYN_REPORT.
    Events { device: u32, records: &'a [u8] },
    /// `u32 device`, `u8 enabled`. A keyboard or mouse appears or goes away.
    Device { device: u32, enabled: bool },
    /// `u32 device` (at least [`gamepad::FIRST`]), `u8 kind`, `u8 0` and an
    /// 8-byte identity. Kind 2 is a host evdev device and kind 3 is a test
    /// harness's fake. The datagram carries one fd, the device's
    /// force-feedback endpoint, if it has one.
    Gamepad {
        device: u32,
        identity: &'a str,
        synthetic: bool,
    },
}

impl<'a> Message<'a> {
    pub fn decode(b: &'a [u8]) -> Option<Message<'a>> {
        let device = u32::from_le_bytes(b.get(..4)?.try_into().ok()?);
        let rest = &b[4..];
        if device >= gamepad::FIRST && rest.len() == 10 && matches!(rest[0], 2 | 3) && rest[1] == 0
        {
            return Some(Message::Gamepad {
                device,
                identity: std::str::from_utf8(&rest[2..]).ok()?,
                synthetic: rest[0] == 3,
            });
        }
        if rest.len() == 1 && device != KEYBOARD {
            return Some(Message::Device {
                device,
                enabled: rest[0] != 0,
            });
        }
        (!rest.is_empty() && rest.len().is_multiple_of(EVENT_SIZE)).then_some(Message::Events {
            device,
            records: rest,
        })
    }

    pub fn encode(&self) -> Vec<u8> {
        match *self {
            Message::Events { device, records } => [&device.to_le_bytes()[..], records].concat(),
            Message::Device { device, enabled } => {
                [&device.to_le_bytes()[..], &[u8::from(enabled)]].concat()
            }
            Message::Gamepad {
                device,
                identity,
                synthetic,
            } => [
                &device.to_le_bytes()[..],
                &[if synthetic { 3 } else { 2 }, 0],
                identity.as_bytes(),
            ]
            .concat(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_round_trip() {
        let records = [event(1_500_000, 1, 57, 1), event(1_500_000, 0, 0, 0)].concat();
        for m in [
            Message::Events {
                device: MOUSE,
                records: &records,
            },
            Message::Device {
                device: 7,
                enabled: true,
            },
            Message::Gamepad {
                device: gamepad::FIRST + 3,
                identity: "0001abcd",
                synthetic: true,
            },
            Message::Gamepad {
                device: gamepad::FIRST,
                identity: "deadbeef",
                synthetic: false,
            },
        ] {
            assert_eq!(Message::decode(&m.encode()), Some(m.clone()));
        }
        assert_eq!(&records[..8], &1i64.to_le_bytes());
        assert_eq!(&records[8..16], &500_000i64.to_le_bytes());
    }

    #[test]
    fn gamepad_timestamp_bytes_never_register_a_device() {
        // Event packets are 4 + N * EVENT_SIZE bytes, so a timestamp whose
        // first byte is 2 or 3 can't pass for a registration.
        for seconds in [258, 259] {
            let mut packet = gamepad::FIRST.to_le_bytes().to_vec();
            packet.extend_from_slice(&event(seconds * 1_000_000, 1, 304, 1));
            packet.extend_from_slice(&event(seconds * 1_000_000, 0, 0, 0));
            assert!(matches!(packet[4], 2 | 3));
            assert!(matches!(
                Message::decode(&packet),
                Some(Message::Events { .. })
            ));
        }
    }

    #[test]
    fn malformed_datagrams_are_nothing() {
        let id = |d: u32| d.to_le_bytes().to_vec();
        assert_eq!(Message::decode(&[1, 0]), None);
        assert_eq!(Message::decode(&id(MOUSE)), None);
        // The keyboard is never added or removed.
        assert_eq!(Message::decode(&[id(KEYBOARD), vec![1]].concat()), None);
        // A partial record.
        assert_eq!(Message::decode(&[id(MOUSE), vec![0; 23]].concat()), None);
        // A registration below the gamepad range, or of another kind.
        let reg = |d: u32, kind: u8| [id(d), vec![kind, 0], b"0001abcd".to_vec()].concat();
        assert_eq!(Message::decode(&reg(5, 2)), None);
        assert_eq!(Message::decode(&reg(gamepad::FIRST, 4)), None);
    }
}

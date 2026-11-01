//! The sysfs entries of the session's evdev devices, for OpenHarmony. MMI's
//! libinput gets a device's udev properties from MMI's own libudev, and that
//! libudev reads them from sysfs. It `stat`s the device node for its
//! `st_rdev`, resolves `/sys/dev/char/<major>:<minor>` with `readlink`, and
//! reads the `uevent` files of the device and its parent. It then derives
//! `ID_INPUT_*` from the capability bitmaps in the parent's `uevent`, as
//! udev's input_id builtin does. The host's sysfs has no entries for the
//! broker's devices, so the broker answers these calls for the paths below.
//! Android's EventHub reads no sysfs.
//!
//! Device `n` is `/dev/input/innkeeper-event<n>` with [`rdev`]. Its sysfs
//! device is `/sys/devices/virtual/input/innkeeper<n>`, and its event node's
//! device is `event<n>` under that. MMI takes the number in that name as its
//! device id, and it refuses a device whose name has another form.

/// The input subsystem's character major.
pub const MAJOR: u32 = 13;
/// Below this minor, the kernel numbers the host's own evdev nodes.
const MINOR_BASE: u32 = 1 << 19;

const DEVNODE: &[u8] = b"/dev/input/innkeeper-event";
const CHAR: &[u8] = b"/sys/dev/char/";
const VIRTUAL: &[u8] = b"/sys/devices/virtual/input";

/// A path that names part of device `.0`'s sysfs view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entry {
    /// The device node.
    Devnode(u32),
    /// `/sys/dev/char/13:<minor>`, a symlink to the event node's device.
    Char(u32),
    /// The input device's directory, and its `uevent` and `subsystem`.
    Device(u32),
    DeviceUevent(u32),
    DeviceSubsystem(u32),
    /// The event node's directory, and its `uevent` and `subsystem`.
    Event(u32),
    EventUevent(u32),
    EventSubsystem(u32),
}

/// What `stat` reports for an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stat {
    Char(u64),
    Dir,
    File,
    Link,
}

pub fn minor(device: u32) -> u32 {
    MINOR_BASE + device
}

pub fn rdev(device: u32) -> u64 {
    libc::makedev(MAJOR, minor(device))
}

/// The path's entry, if it names one. The caller checks that the device
/// exists.
pub fn parse(path: &[u8]) -> Option<Entry> {
    if let Some(n) = path.strip_prefix(DEVNODE) {
        return number(n).map(Entry::Devnode);
    }
    if let Some(rest) = path.strip_prefix(CHAR) {
        let rest = std::str::from_utf8(rest).ok()?;
        let (major, minor) = rest.split_once(':')?;
        if major != MAJOR.to_string() {
            return None;
        }
        let minor: u32 = number(minor.as_bytes())?;
        return minor.checked_sub(MINOR_BASE).map(Entry::Char);
    }
    let rest = path.strip_prefix(VIRTUAL)?.strip_prefix(b"/innkeeper")?;
    let digits = rest.iter().take_while(|c| c.is_ascii_digit()).count();
    let device = number(&rest[..digits])?;
    let mut parts = rest[digits..].split(|&c| c == b'/').skip(1);
    let event = format!("event{device}");
    let entry = match (parts.next(), parts.next(), parts.next()) {
        (None, ..) => Entry::Device(device),
        (Some(b"uevent"), None, _) => Entry::DeviceUevent(device),
        (Some(b"subsystem"), None, _) => Entry::DeviceSubsystem(device),
        (Some(e), None, _) if e == event.as_bytes() => Entry::Event(device),
        (Some(e), Some(b"uevent"), None) if e == event.as_bytes() => Entry::EventUevent(device),
        (Some(e), Some(b"subsystem"), None) if e == event.as_bytes() => {
            Entry::EventSubsystem(device)
        }
        _ => return None,
    };
    Some(entry)
}

/// A decimal number without a sign or leading zeros.
fn number(b: &[u8]) -> Option<u32> {
    if b.is_empty() || (b.len() > 1 && b[0] == b'0') || !b.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(b).ok()?.parse().ok()
}

impl Entry {
    pub fn device(self) -> u32 {
        match self {
            Entry::Devnode(d)
            | Entry::Char(d)
            | Entry::Device(d)
            | Entry::DeviceUevent(d)
            | Entry::DeviceSubsystem(d)
            | Entry::Event(d)
            | Entry::EventUevent(d)
            | Entry::EventSubsystem(d) => d,
        }
    }

    pub fn stat(self) -> Stat {
        match self {
            Entry::Devnode(d) => Stat::Char(rdev(d)),
            Entry::Char(_) | Entry::DeviceSubsystem(_) | Entry::EventSubsystem(_) => Stat::Link,
            Entry::Device(_) | Entry::Event(_) => Stat::Dir,
            Entry::DeviceUevent(_) | Entry::EventUevent(_) => Stat::File,
        }
    }

    /// The symlink's target, or None for an entry that is not a symlink.
    /// `readlink` of those fails with EINVAL, which realpath takes to mean
    /// that the path component exists.
    pub fn target(self) -> Option<String> {
        match self {
            Entry::Char(d) => Some(format!("../../devices/virtual/input/innkeeper{d}/event{d}")),
            Entry::DeviceSubsystem(_) => Some("../../../../class/input".into()),
            Entry::EventSubsystem(_) => Some("../../../../../class/input".into()),
            _ => None,
        }
    }
}

/// The event node's `uevent`.
pub fn event_uevent(device: u32) -> String {
    format!(
        "MAJOR={MAJOR}\nMINOR={}\nDEVNAME=input/innkeeper-event{device}\n",
        minor(device)
    )
}

/// A capability bitmap in sysfs's format: 64-bit words in hex, highest
/// first, separated by spaces, without leading zero words.
pub fn bitmap(bytes: &[u8]) -> String {
    let words: Vec<u64> = bytes
        .chunks(8)
        .map(|c| {
            let mut w = [0; 8];
            w[..c.len()].copy_from_slice(c);
            u64::from_le_bytes(w)
        })
        .collect();
    let top = words.iter().rposition(|&w| w != 0).unwrap_or(0);
    words[..=top]
        .iter()
        .rev()
        .map(|w| format!("{w:x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_name_their_device_entries() {
        let m = minor(65535);
        for (path, entry) in [
            ("/dev/input/innkeeper-event0", Entry::Devnode(0)),
            (&format!("/sys/dev/char/13:{m}"), Entry::Char(65535)),
            ("/sys/devices/virtual/input/innkeeper7", Entry::Device(7)),
            (
                "/sys/devices/virtual/input/innkeeper7/uevent",
                Entry::DeviceUevent(7),
            ),
            (
                "/sys/devices/virtual/input/innkeeper7/subsystem",
                Entry::DeviceSubsystem(7),
            ),
            (
                "/sys/devices/virtual/input/innkeeper7/event7",
                Entry::Event(7),
            ),
            (
                "/sys/devices/virtual/input/innkeeper7/event7/uevent",
                Entry::EventUevent(7),
            ),
            (
                "/sys/devices/virtual/input/innkeeper7/event7/subsystem",
                Entry::EventSubsystem(7),
            ),
        ] {
            assert_eq!(parse(path.as_bytes()), Some(entry), "{path}");
        }
    }

    #[test]
    fn other_paths_are_the_hosts() {
        for path in [
            "/dev/input/event3",
            "/dev/input/innkeeper-event",
            "/dev/input/innkeeper-event01",
            "/dev/input/innkeeper-event1x",
            "/sys/dev/char/13:64",
            "/sys/dev/char/1:524288",
            "/sys/dev/char/13:",
            "/sys/devices/virtual/input",
            "/sys/devices/virtual/input/input3/uevent",
            "/sys/devices/virtual/input/innkeeper",
            "/sys/devices/virtual/input/innkeeper7/event8",
            "/sys/devices/virtual/input/innkeeper7/event7/uevent/x",
            "/sys/devices/virtual/input/innkeeper7/name",
        ] {
            assert_eq!(parse(path.as_bytes()), None, "{path}");
        }
    }

    /// MMI's libudev resolves the link's leading `../` against
    /// `/sys/dev/char/<major>:<minor>` and must land on the event node.
    #[test]
    fn the_char_link_resolves_to_the_event_node() {
        let link = format!("/sys/dev/char/13:{}", minor(3));
        let target = parse(link.as_bytes()).unwrap().target().unwrap();
        let ups = target.matches("../").count();
        let base: Vec<_> = link.split('/').collect();
        let resolved = format!(
            "{}/{}",
            base[..base.len() - ups - 1].join("/"),
            target.trim_start_matches("../")
        );
        assert_eq!(
            parse(resolved.as_bytes()),
            Some(Entry::Event(3)),
            "{resolved}"
        );
        assert_eq!(Entry::Event(3).target(), None);
        assert_eq!(Entry::Devnode(3).stat(), Stat::Char(rdev(3)));
    }

    #[test]
    fn bitmaps_have_sysfs_format() {
        assert_eq!(bitmap(&[0; 16]), "0");
        assert_eq!(bitmap(&[0x0b, 0, 0, 0, 0, 0, 0, 0, 0, 0]), "b");
        let mut key = [0u8; 96];
        key[330 / 8] |= 1 << (330 % 8); // BTN_TOUCH
        assert_eq!(bitmap(&key), "400 0 0 0 0 0");
        let mut keys = [0u8; 16];
        keys[0] = 0xfe;
        keys[8] = 1;
        assert_eq!(bitmap(&keys), "1 fe");
    }

    #[test]
    fn the_event_node_names_its_devnode() {
        assert_eq!(
            event_uevent(2),
            format!(
                "MAJOR=13\nMINOR={}\nDEVNAME=input/innkeeper-event2\n",
                minor(2)
            )
        );
    }
}

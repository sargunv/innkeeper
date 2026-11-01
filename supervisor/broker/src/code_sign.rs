//! OpenHarmony's code-signing device (`INNKEEPER_GUEST=ohos`). When installd
//! installs a HAP with a debug signing profile, key_enable adds the profile's
//! certificate chain to the kernel through `/dev/code_sign`. The kernel then
//! accepts code signed by that chain, and BMS refuses the install if the add
//! fails. The session's kernel doesn't enforce code signatures, so the broker
//! accepts every add and remove and keeps no state.
//!
//! The node is an empty file that the launcher plants. The broker serves its
//! `'k'` ioctls and lets the open go through.

/// The device node. Its ioctls are type `'k'`.
pub const NODE: &str = "/dev/code_sign";
pub const IOCTL_TYPE: u8 = b'k';

// key_enable's cert_path_utils.rs passes a 64-byte `cert_path_info`.
const fn iow(nr: u32) -> u32 {
    1 << 30 | 64 << 16 | (IOCTL_TYPE as u32) << 8 | nr
}
const ADD_CERT_CHAIN: u32 = iow(1);
const REMOVE_CERT_CHAIN: u32 = iow(2);
const ADD_ENTERPRISE_CERT: u32 = iow(4);
const REMOVE_ENTERPRISE_CERT: u32 = iow(5);

/// Whether `cmd` is a code-signing ioctl, judged by its type byte.
pub fn is_code_sign_ioctl(cmd: u32) -> bool {
    (cmd >> 8) & 0xff == IOCTL_TYPE as u32
}

/// Serves code-signing ioctl `cmd`. Returns the ioctl's return value, or an
/// errno.
pub fn ioctl(cmd: u32) -> Result<i64, i32> {
    match cmd {
        ADD_CERT_CHAIN | REMOVE_CERT_CHAIN | ADD_ENTERPRISE_CERT | REMOVE_ENTERPRISE_CERT => Ok(0),
        _ => Err(libc::ENOTTY),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cert_chains_are_accepted() {
        // The numbers key_enable passes.
        for cmd in [0x40406b01, 0x40406b02, 0x40406b04, 0x40406b05] {
            assert!(is_code_sign_ioctl(cmd));
            assert_eq!(ioctl(cmd), Ok(0));
        }
    }

    #[test]
    fn other_ioctls_fail() {
        assert_eq!(ioctl(iow(3)), Err(libc::ENOTTY));
        assert_eq!(ioctl(0x80406b01), Err(libc::ENOTTY));
        assert!(!is_code_sign_ioctl(0x40084101));
    }
}

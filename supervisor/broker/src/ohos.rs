//! OpenHarmony's access tokens (`INNKEEPER_GUEST=ohos`): the identity OH's
//! kernel keeps beside the uid, a token and a first token per task
//! (`current->token`, `current->ftoken`). Init and appspawn set them before
//! exec through `/dev/access_token_id`; processes read their own back there,
//! and binder hands the sender's to the receiving thread
//! (`BINDER_GET_ACCESS_TOKEN`, `BINDER_GET_SENDER_INFO` in
//! [`crate::binder::driver`]).
//!
//! The kernel keeps them in the task, so fork inherits them. The broker keeps
//! one record per process: keyed by pid plus start time, so a reused pid
//! never shows a dead process's token, and a process without a record of its
//! own gets its closest recorded ancestor's, which it would have inherited.
//! The same node holds the kernel's permission table: a 2048-bit map per
//! native token, which init fills from the service cfgs' "permission" lists
//! and the access-token service reads to answer permission checks.
//!
//! Callers are not checked: OH's kernel only lets privileged callers set
//! tokens or permissions, a guard beyond desktop parity (docs/security.md).
//!
//! The node is an empty file the launcher plants; the broker serves its
//! `'A'` ioctls and lets the open itself through.

use std::collections::HashMap;

/// The device node; its ioctls are type `'A'` (`ACCESS_TOKEN_ID_IOCTL_BASE`).
pub const NODE: &str = "/dev/access_token_id";
pub const IOCTL_TYPE: u8 = b'A';

// The kernel's drivers/accesstokenid/access_tokenid.h and
// security_access_token's setproc_common.h and perm_setproc_c.cpp.
const fn ioc(dir: u32, nr: u32, size: usize) -> u32 {
    dir << 30 | (size as u32) << 16 | (IOCTL_TYPE as u32) << 8 | nr
}
const W: u32 = 1;
const R: u32 = 2;
/// Permission map words per token (`MAX_PERM_GROUP_NUM`).
const PERM_WORDS: usize = 64;
/// `{u32 token; u32 op_code; bool is_granted}`.
const PERM_OP: usize = 12;
/// `{u32 token; u32 perm[64]}`.
const PERM_MAP: usize = 4 + 4 * PERM_WORDS;
const GET_TOKENID: u32 = ioc(R, 1, 8);
const SET_TOKENID: u32 = ioc(W, 2, 8);
const GET_FTOKENID: u32 = ioc(R, 3, 8);
const SET_FTOKENID: u32 = ioc(W, 4, 8);
const ADD_PERMISSIONS: u32 = ioc(W, 5, PERM_MAP);
const REMOVE_PERMISSIONS: u32 = ioc(W, 6, 4);
const GET_PERMISSION: u32 = ioc(W, 7, PERM_OP);
const SET_PERMISSION: u32 = ioc(W, 8, PERM_OP);
const GET_ALL_PERMISSIONS: u32 = ioc(W, 11, PERM_MAP);
/// The kernel's bound on tokens with a permission map (`MAX_NODE_NUM`).
const MAX_PERM_TOKENS: usize = 1000;

/// Is `cmd` an access-token ioctl (by its type byte)?
pub fn is_token_ioctl(cmd: u32) -> bool {
    (cmd >> 8) & 0xff == IOCTL_TYPE as u32
}

struct Entry {
    start: u64,
    token: u64,
    ftoken: u64,
}

/// A live process's (start time, parent pid), from `/proc/<pid>/stat`.
pub type Stat<'a> = &'a dyn Fn(i32) -> Option<(u64, i32)>;

/// Every process's (token, first token), and the permission maps.
#[derive(Default)]
pub struct Tokens {
    map: HashMap<i32, Entry>,
    perms: HashMap<u32, [u32; PERM_WORDS]>,
}

/// An ioctl's argument in the caller's memory.
pub trait Arg {
    fn read(&mut self, len: usize) -> Option<Vec<u8>>;
    fn write(&mut self, data: &[u8]) -> bool;
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_ne_bytes(b[i..i + 4].try_into().unwrap())
}

impl Tokens {
    /// `pid`'s tokens as last resolved: the per-transaction lookup, which
    /// reads no `/proc` files. A pid without a record has none.
    pub fn get(&self, pid: i32) -> (u64, u64) {
        self.map.get(&pid).map_or((0, 0), |e| (e.token, e.ftoken))
    }

    /// `pid`'s tokens, recording what it inherited if it has no current
    /// record of its own. Called when a process opens binder or the node.
    pub fn resolve(&mut self, pid: i32, stat: Stat) -> (u64, u64) {
        let Some((start, _)) = stat(pid) else {
            self.map.remove(&pid);
            return (0, 0);
        };
        if let Some(e) = self.map.get(&pid)
            && e.start == start
        {
            return (e.token, e.ftoken);
        }
        let mut inherited = (0, 0);
        let mut p = pid;
        // Bounded: a chain of more than a few dozen recordless ancestors
        // would be a loop in a racing reparent.
        for _ in 0..64 {
            let Some((_, parent)) = stat(p) else { break };
            if parent <= 0 {
                break;
            }
            if let Some(e) = self.map.get(&parent)
                && stat(parent).is_some_and(|(s, _)| s == e.start)
            {
                inherited = (e.token, e.ftoken);
                break;
            }
            p = parent;
        }
        self.map.insert(
            pid,
            Entry {
                start,
                token: inherited.0,
                ftoken: inherited.1,
            },
        );
        inherited
    }

    /// Serve access-token ioctl `cmd` from process `pid`: the ioctl's
    /// return value, or an errno.
    pub fn ioctl(&mut self, pid: i32, cmd: u32, stat: Stat, arg: &mut dyn Arg) -> Result<i64, i32> {
        let read = |arg: &mut dyn Arg, len| arg.read(len).ok_or(libc::EFAULT);
        let done = |ok: bool| if ok { Ok(0) } else { Err(libc::EFAULT) };
        match cmd {
            GET_TOKENID | GET_FTOKENID => {
                let (token, ftoken) = self.resolve(pid, stat);
                let v = if cmd == GET_TOKENID { token } else { ftoken };
                done(arg.write(&v.to_ne_bytes()))
            }
            SET_TOKENID | SET_FTOKENID => {
                self.resolve(pid, stat);
                let v = u64::from_ne_bytes(read(arg, 8)?.try_into().unwrap());
                let Some(e) = self.map.get_mut(&pid) else {
                    return Err(libc::ESRCH);
                };
                if cmd == SET_TOKENID {
                    e.token = v;
                } else {
                    e.ftoken = v;
                }
                Ok(0)
            }
            ADD_PERMISSIONS => {
                let b = read(arg, PERM_MAP)?;
                let token = u32_at(&b, 0);
                if !self.perms.contains_key(&token) && self.perms.len() >= MAX_PERM_TOKENS {
                    return Err(libc::EDQUOT);
                }
                let map = std::array::from_fn(|i| u32_at(&b, 4 + 4 * i));
                self.perms.insert(token, map);
                Ok(0)
            }
            REMOVE_PERMISSIONS => {
                self.perms.remove(&u32_at(&read(arg, 4)?, 0));
                Ok(0)
            }
            GET_PERMISSION | SET_PERMISSION => {
                let b = read(arg, PERM_OP)?;
                let (token, op) = (u32_at(&b, 0), u32_at(&b, 4) as usize);
                if op / 32 >= PERM_WORDS {
                    return Err(libc::EINVAL);
                }
                let map = self.perms.get_mut(&token).ok_or(libc::ENODATA)?;
                let bit = 1u32 << (op % 32);
                if cmd == GET_PERMISSION {
                    // The answer is the return value, as in the kernel.
                    return Ok((map[op / 32] & bit != 0) as i64);
                }
                if b[8] != 0 {
                    map[op / 32] |= bit;
                } else {
                    map[op / 32] &= !bit;
                }
                Ok(0)
            }
            GET_ALL_PERMISSIONS => {
                let token = u32_at(&read(arg, 4)?, 0);
                let map = self.perms.get(&token).ok_or(libc::ENODATA)?;
                let mut out = token.to_ne_bytes().to_vec();
                map.iter()
                    .for_each(|w| out.extend_from_slice(&w.to_ne_bytes()));
                done(arg.write(&out))
            }
            // Newer kernels' SPM entries and reference counts: absent, as
            // on a kernel built without them.
            _ => Err(libc::ENOTTY),
        }
    }
}

/// `/proc/<pid>/stat`'s start time (field 22) and parent (field 4).
pub fn proc_stat(pid: i32) -> Option<(u64, i32)> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command name is parenthesized and may hold spaces.
    let f: Vec<&str> = s.get(s.rfind(')')? + 2..)?.split(' ').collect();
    Some((f.get(19)?.parse().ok()?, f.get(1)?.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// pid -> (start, parent).
    fn procs(list: &[(i32, u64, i32)]) -> impl Fn(i32) -> Option<(u64, i32)> {
        let m: HashMap<i32, (u64, i32)> = list.iter().map(|&(p, s, pp)| (p, (s, pp))).collect();
        move |p| m.get(&p).copied()
    }

    /// The caller's argument: `buf`, which writes replace.
    struct Buf(Vec<u8>);
    impl Arg for Buf {
        fn read(&mut self, len: usize) -> Option<Vec<u8>> {
            self.0.get(..len).map(<[u8]>::to_vec)
        }
        fn write(&mut self, data: &[u8]) -> bool {
            self.0 = data.to_vec();
            true
        }
    }

    fn set(t: &mut Tokens, pid: i32, stat: Stat, cmd: u32, v: u64) -> i32 {
        match t.ioctl(pid, cmd, stat, &mut Buf(v.to_ne_bytes().to_vec())) {
            Ok(_) => 0,
            Err(e) => e,
        }
    }

    fn get(t: &mut Tokens, pid: i32, stat: Stat, cmd: u32) -> Option<u64> {
        let mut b = Buf(Vec::new());
        t.ioctl(pid, cmd, stat, &mut b).ok()?;
        Some(u64::from_ne_bytes(b.0.try_into().ok()?))
    }

    fn perm_op(token: u32, op: u32, granted: bool) -> Buf {
        let mut b = token.to_ne_bytes().to_vec();
        b.extend_from_slice(&op.to_ne_bytes());
        b.extend_from_slice(&[granted as u8, 0, 0, 0]);
        Buf(b)
    }

    #[test]
    fn a_process_reads_back_what_it_set() {
        let s = procs(&[(10, 100, 1)]);
        let mut t = Tokens::default();
        assert_eq!(get(&mut t, 10, &s, GET_TOKENID), Some(0));
        assert_eq!(set(&mut t, 10, &s, SET_TOKENID, 0x2800_0001), 0);
        assert_eq!(set(&mut t, 10, &s, SET_FTOKENID, 7), 0);
        assert_eq!(get(&mut t, 10, &s, GET_TOKENID), Some(0x2800_0001));
        assert_eq!(get(&mut t, 10, &s, GET_FTOKENID), Some(7));
        assert_eq!(t.get(10), (0x2800_0001, 7));
    }

    #[test]
    fn children_inherit_their_closest_recorded_ancestor() {
        let s = procs(&[(10, 100, 1), (11, 110, 10), (12, 120, 11)]);
        let mut t = Tokens::default();
        set(&mut t, 10, &s, SET_TOKENID, 5);
        assert_eq!(t.get(12), (0, 0), "no record before it is resolved");
        assert_eq!(t.resolve(12, &s), (5, 0));
        assert_eq!(t.get(12), (5, 0));
        // The child's own token is its own from then on.
        set(&mut t, 12, &s, SET_TOKENID, 9);
        assert_eq!((t.get(10), t.get(12)), ((5, 0), (9, 0)));
    }

    #[test]
    fn a_reused_pid_does_not_get_a_dead_process_token() {
        let mut t = Tokens::default();
        set(&mut t, 10, &procs(&[(10, 100, 1)]), SET_TOKENID, 5);
        let reused = procs(&[(10, 200, 1)]);
        assert_eq!(t.resolve(10, &reused), (0, 0));
        assert_eq!(get(&mut t, 10, &reused, GET_TOKENID), Some(0));
        // Nor does a child of a pid that was reused.
        let s = procs(&[(10, 300, 1), (11, 310, 10)]);
        let mut t = Tokens::default();
        set(&mut t, 10, &procs(&[(10, 100, 1)]), SET_TOKENID, 5);
        assert_eq!(t.resolve(11, &s), (0, 0));
    }

    #[test]
    fn permission_maps_answer_checks_through_the_return_value() {
        let s = procs(&[(10, 100, 1)]);
        let mut t = Tokens::default();
        let mut map = 7u32.to_ne_bytes().to_vec();
        map.extend(
            (0..PERM_WORDS as u32).flat_map(|i| (if i == 1 { 0b100u32 } else { 0 }).to_ne_bytes()),
        );
        assert_eq!(t.ioctl(10, ADD_PERMISSIONS, &s, &mut Buf(map)), Ok(0));
        // Bit 34 is word 1, bit 2.
        assert_eq!(
            t.ioctl(10, GET_PERMISSION, &s, &mut perm_op(7, 34, false)),
            Ok(1)
        );
        assert_eq!(
            t.ioctl(10, GET_PERMISSION, &s, &mut perm_op(7, 35, false)),
            Ok(0)
        );
        assert_eq!(
            t.ioctl(10, SET_PERMISSION, &s, &mut perm_op(7, 35, true)),
            Ok(0)
        );
        assert_eq!(
            t.ioctl(10, GET_PERMISSION, &s, &mut perm_op(7, 35, false)),
            Ok(1)
        );
        let mut all = Buf(7u32.to_ne_bytes().to_vec());
        assert_eq!(t.ioctl(10, GET_ALL_PERMISSIONS, &s, &mut all), Ok(0));
        assert_eq!(u32_at(&all.0, 8), 0b1100);
        assert_eq!(
            t.ioctl(10, GET_PERMISSION, &s, &mut perm_op(7, 64 * 32, false)),
            Err(libc::EINVAL)
        );
        assert_eq!(
            t.ioctl(
                10,
                REMOVE_PERMISSIONS,
                &s,
                &mut Buf(7u32.to_ne_bytes().to_vec())
            ),
            Ok(0)
        );
        assert_eq!(
            t.ioctl(10, GET_PERMISSION, &s, &mut perm_op(7, 34, false)),
            Err(libc::ENODATA)
        );
    }

    #[test]
    fn unknown_ioctls_and_gone_processes_fail() {
        let s = procs(&[(10, 100, 1)]);
        let mut t = Tokens::default();
        assert_eq!(set(&mut t, 10, &s, ioc(R | W, 16, 8), 1), libc::ENOTTY);
        assert_eq!(set(&mut t, 99, &s, SET_TOKENID, 1), libc::ESRCH);
        assert_eq!(
            t.ioctl(10, SET_TOKENID, &s, &mut Buf(Vec::new())),
            Err(libc::EFAULT)
        );
    }

    #[test]
    fn proc_stat_parses_names_with_spaces() {
        let (start, parent) = proc_stat(std::process::id() as i32).unwrap();
        assert!(start > 0);
        assert_eq!(parent, unsafe { libc::getppid() });
    }
}

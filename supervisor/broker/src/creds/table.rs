//! Virtual ids for Android processes (supervisor/README.md).
//!
//! The session's user namespace maps only the user's uid and gid, so the
//! kernel can't give Android's processes their own uids. The broker traps the
//! credential syscalls instead, and this module is their pure model. It holds
//! per-process credentials, the kernel's `set*id` rules (`kernel/sys.c`), and
//! an interning table.
//!
//! The credentials live in the process, as the soft `RLIMIT_RTTIME` limit,
//! which holds an id into the [`CredTable`]. Rlimits are per process,
//! inherited by `fork` and kept across `execve`, and the supervisor can read
//! and set them with `prlimit(pid, ...)`. A new process therefore carries its
//! parent's identity without fork tracking or races. The default
//! (`RLIM_INFINITY`) is root. `RLIMIT_RTTIME` limits only real-time tasks, and
//! the session has none because SCHED_FIFO/RR requests are faked.

use std::collections::HashMap;

/// The kernel's NGROUPS_MAX.
pub const NGROUPS_MAX: usize = 65536;
/// `(uid_t)-1`, which means "leave unchanged" for setre*/setres* and is
/// invalid elsewhere.
pub const NO_ID: u32 = u32::MAX;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Creds {
    pub ruid: u32,
    pub euid: u32,
    pub suid: u32,
    pub fsuid: u32,
    pub rgid: u32,
    pub egid: u32,
    pub sgid: u32,
    pub fsgid: u32,
    /// Supplementary groups, sorted. The kernel sorts them too, and getgroups
    /// returns that order.
    pub groups: Vec<u32>,
}

impl Creds {
    pub fn root() -> Creds {
        Creds {
            ruid: 0,
            euid: 0,
            suid: 0,
            fsuid: 0,
            rgid: 0,
            egid: 0,
            sgid: 0,
            fsgid: 0,
            groups: Vec::new(),
        }
    }
}

/// A credential-changing syscall, with its raw arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SetId {
    Uid(u32),
    Reuid(u32, u32),
    Resuid(u32, u32, u32),
    Fsuid(u32),
    Gid(u32),
    Regid(u32, u32),
    Resgid(u32, u32, u32),
    Fsgid(u32),
    Groups(Vec<u32>),
}

/// CAP_SETUID and CAP_SETGID for a credential syscall. Virtual root has both,
/// and every other uid has neither. `capset` is answered without changing the
/// kernel's set, and a sandboxed `CapEff` is always 0, so there is nothing
/// else to read.
#[derive(Clone, Copy, Debug, Default)]
pub struct Caps {
    pub setuid: bool,
    pub setgid: bool,
}

/// Applies `op` to `old` as the kernel would. Returns the new credentials and
/// the syscall's return value, or an errno. setfsuid and setfsgid don't fail.
/// They return the previous fs id whether or not it changed.
pub fn apply(old: &Creds, op: &SetId, caps: Caps) -> Result<(Creds, i64), i32> {
    let mut new = old.clone();
    match *op {
        SetId::Uid(u) => {
            if u == NO_ID {
                return Err(libc_errno::EINVAL);
            }
            if caps.setuid {
                new.ruid = u;
                new.suid = u;
            } else if u != old.ruid && u != old.suid {
                return Err(libc_errno::EPERM);
            }
            new.euid = u;
            new.fsuid = u;
        }
        SetId::Reuid(r, e) => {
            if r != NO_ID {
                if r != old.ruid && r != old.euid && !caps.setuid {
                    return Err(libc_errno::EPERM);
                }
                new.ruid = r;
            }
            if e != NO_ID {
                if e != old.ruid && e != old.euid && e != old.suid && !caps.setuid {
                    return Err(libc_errno::EPERM);
                }
                new.euid = e;
            }
            if r != NO_ID || (e != NO_ID && e != old.ruid) {
                new.suid = new.euid;
            }
            new.fsuid = new.euid;
        }
        SetId::Resuid(r, e, s) => {
            if !caps.setuid {
                let ok = |v: u32| v == NO_ID || v == old.ruid || v == old.euid || v == old.suid;
                if !ok(r) || !ok(e) || !ok(s) {
                    return Err(libc_errno::EPERM);
                }
            }
            if r != NO_ID {
                new.ruid = r;
            }
            if e != NO_ID {
                new.euid = e;
            }
            if s != NO_ID {
                new.suid = s;
            }
            new.fsuid = new.euid;
        }
        SetId::Fsuid(f) => {
            let prev = old.fsuid as i64;
            if f != NO_ID
                && (f == old.ruid
                    || f == old.euid
                    || f == old.suid
                    || f == old.fsuid
                    || caps.setuid)
            {
                new.fsuid = f;
            }
            return Ok((new, prev));
        }
        SetId::Gid(g) => {
            if g == NO_ID {
                return Err(libc_errno::EINVAL);
            }
            if caps.setgid {
                new.rgid = g;
                new.sgid = g;
            } else if g != old.rgid && g != old.sgid {
                return Err(libc_errno::EPERM);
            }
            new.egid = g;
            new.fsgid = g;
        }
        SetId::Regid(r, e) => {
            if r != NO_ID {
                if r != old.rgid && r != old.egid && !caps.setgid {
                    return Err(libc_errno::EPERM);
                }
                new.rgid = r;
            }
            if e != NO_ID {
                if e != old.rgid && e != old.egid && e != old.sgid && !caps.setgid {
                    return Err(libc_errno::EPERM);
                }
                new.egid = e;
            }
            if r != NO_ID || (e != NO_ID && e != old.rgid) {
                new.sgid = new.egid;
            }
            new.fsgid = new.egid;
        }
        SetId::Resgid(r, e, s) => {
            if !caps.setgid {
                let ok = |v: u32| v == NO_ID || v == old.rgid || v == old.egid || v == old.sgid;
                if !ok(r) || !ok(e) || !ok(s) {
                    return Err(libc_errno::EPERM);
                }
            }
            if r != NO_ID {
                new.rgid = r;
            }
            if e != NO_ID {
                new.egid = e;
            }
            if s != NO_ID {
                new.sgid = s;
            }
            new.fsgid = new.egid;
        }
        SetId::Fsgid(f) => {
            let prev = old.fsgid as i64;
            if f != NO_ID
                && (f == old.rgid
                    || f == old.egid
                    || f == old.sgid
                    || f == old.fsgid
                    || caps.setgid)
            {
                new.fsgid = f;
            }
            return Ok((new, prev));
        }
        SetId::Groups(ref g) => {
            if !caps.setgid {
                return Err(libc_errno::EPERM);
            }
            if g.len() > NGROUPS_MAX {
                return Err(libc_errno::EINVAL);
            }
            let mut g = g.clone();
            g.sort_unstable();
            new.groups = g;
        }
    }
    Ok((new, 0))
}

/// The number of entries getgroups(size, list) writes, or an errno.
pub fn getgroups_count(c: &Creds, size: usize) -> Result<usize, i32> {
    if size == 0 || size >= c.groups.len() {
        Ok(c.groups.len())
    } else {
        Err(libc_errno::EINVAL)
    }
}

/// Interned credentials. Id 0 is root. A process's soft `RLIMIT_RTTIME`
/// encodes ids >= 1 as `id` and root as `RLIM_INFINITY`.
pub struct CredTable {
    by_id: Vec<Creds>,
    ids: HashMap<Creds, u64>,
}

pub const RLIM_INFINITY: u64 = u64::MAX;

impl Default for CredTable {
    fn default() -> Self {
        Self::new()
    }
}

impl CredTable {
    pub fn new() -> CredTable {
        let root = Creds::root();
        let mut ids = HashMap::new();
        ids.insert(root.clone(), 0);
        CredTable {
            by_id: vec![root],
            ids,
        }
    }

    pub fn intern(&mut self, c: &Creds) -> u64 {
        if let Some(&id) = self.ids.get(c) {
            return id;
        }
        let id = self.by_id.len() as u64;
        self.by_id.push(c.clone());
        self.ids.insert(c.clone(), id);
        id
    }

    /// Credentials for a raw soft-limit value. Any value that isn't one of
    /// the table's ids (infinity, 0, out of range) is root.
    pub fn lookup(&self, rlim: u64) -> &Creds {
        let id = if rlim == RLIM_INFINITY { 0 } else { rlim };
        self.by_id.get(id as usize).unwrap_or(&self.by_id[0])
    }

    /// The soft-limit value that encodes `id`.
    pub fn encode(id: u64) -> u64 {
        if id == 0 { RLIM_INFINITY } else { id }
    }
}

/// The kernel's permission-bit check (`generic_permission` without ACLs or
/// capabilities). Returns whether a process with `uid`, `gid` and `groups` may
/// access a file with `st_mode`, owned by `owner:group`, for `want` (R=4, W=2,
/// X=1).
pub fn may_access(
    st_mode: u32,
    owner: u32,
    group: u32,
    uid: u32,
    gid: u32,
    groups: &[u32],
    want: u32,
) -> bool {
    let bits = if uid == owner {
        st_mode >> 6
    } else if gid == group || groups.contains(&group) {
        st_mode >> 3
    } else {
        st_mode
    } & 7;
    bits & want == want
}

/// The virtual owner recorded for a file (`user.innkeeper.owner`, "uid:gid").
pub const OWNER_XATTR: &str = "user.innkeeper.owner";

pub fn parse_owner(v: &[u8]) -> Option<(u32, u32)> {
    let s = std::str::from_utf8(v).ok()?;
    let (u, g) = s.trim_end_matches('\0').split_once(':')?;
    Some((u.parse().ok()?, g.parse().ok()?))
}

pub fn format_owner(uid: u32, gid: u32) -> String {
    format!("{uid}:{gid}")
}

/// The owner after chown(uid, gid) of a file whose recorded owner is `cur`.
pub fn chown_owner(cur: (u32, u32), uid: u32, gid: u32) -> (u32, u32) {
    (
        if uid == NO_ID { cur.0 } else { uid },
        if gid == NO_ID { cur.1 } else { gid },
    )
}

/// Which process's `status` an open of `path` reads, if it is one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcStatus {
    /// /proc/self/status
    Myself,
    /// /proc/thread-self/status
    MyThread,
    /// /proc/<pid>/status
    Pid(u32),
    /// /proc/<pid>/task/<tid>/status
    Task(u32, u32),
}

pub fn parse_proc_status(path: &[u8]) -> Option<ProcStatus> {
    let s = std::str::from_utf8(path).ok()?;
    let rest = s.strip_prefix("/proc/")?.strip_suffix("/status")?;
    let num = |x: &str| -> Option<u32> {
        if x.is_empty() || !x.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        x.parse().ok()
    };
    match rest.split('/').collect::<Vec<_>>().as_slice() {
        ["self"] => Some(ProcStatus::Myself),
        ["thread-self"] => Some(ProcStatus::MyThread),
        [p] => Some(ProcStatus::Pid(num(p)?)),
        [p, "task", t] => Some(ProcStatus::Task(num(p)?, num(t)?)),
        _ => None,
    }
}

/// `status` contents with the Uid, Gid and Groups lines replaced by `c`'s.
/// The lines use the kernel's format, with tab-separated fields and a space
/// after each group.
pub fn rewrite_status(content: &str, c: &Creds) -> String {
    let mut out = String::with_capacity(content.len() + 64);
    for line in content.split_inclusive('\n') {
        if line.starts_with("Uid:") {
            out.push_str(&format!(
                "Uid:\t{}\t{}\t{}\t{}\n",
                c.ruid, c.euid, c.suid, c.fsuid
            ));
        } else if line.starts_with("Gid:") {
            out.push_str(&format!(
                "Gid:\t{}\t{}\t{}\t{}\n",
                c.rgid, c.egid, c.sgid, c.fsgid
            ));
        } else if line.starts_with("Groups:") {
            out.push_str("Groups:\t");
            for g in &c.groups {
                out.push_str(&format!("{g} "));
            }
            out.push('\n');
        } else {
            out.push_str(line);
        }
    }
    out
}

/// errno values (x86_64/generic), so the pure model needs no libc.
pub mod libc_errno {
    pub const EPERM: i32 = 1;
    pub const EINVAL: i32 = 22;
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRIV: Caps = Caps {
        setuid: true,
        setgid: true,
    };
    const UNPRIV: Caps = Caps {
        setuid: false,
        setgid: false,
    };

    fn creds(r: u32, e: u32, s: u32) -> Creds {
        Creds {
            ruid: r,
            euid: e,
            suid: s,
            fsuid: e,
            ..Creds::root()
        }
    }

    #[test]
    fn zygote_specialize() {
        // setresgid, setgroups, setresuid as zygote does for an app.
        let c = Creds::root();
        let (c, _) = apply(&c, &SetId::Resgid(10050, 10050, 10050), PRIV).unwrap();
        let (c, _) = apply(&c, &SetId::Groups(vec![9997, 3003, 20050, 50050]), PRIV).unwrap();
        let (c, r) = apply(&c, &SetId::Resuid(10050, 10050, 10050), PRIV).unwrap();
        assert_eq!(r, 0);
        assert_eq!(
            (c.ruid, c.euid, c.suid, c.fsuid),
            (10050, 10050, 10050, 10050)
        );
        assert_eq!(
            (c.rgid, c.egid, c.sgid, c.fsgid),
            (10050, 10050, 10050, 10050)
        );
        assert_eq!(c.groups, vec![3003, 9997, 20050, 50050]);
        // Once caps are dropped, it can't go back.
        assert_eq!(apply(&c, &SetId::Uid(0), UNPRIV), Err(libc_errno::EPERM));
        assert_eq!(
            apply(&c, &SetId::Groups(vec![]), UNPRIV),
            Err(libc_errno::EPERM)
        );
    }

    #[test]
    fn setuid_rules() {
        let c = creds(1000, 0, 0);
        // Unprivileged setuid may only pick the real or saved uid, and only
        // changes the effective (and fs) uid.
        let (n, _) = apply(&c, &SetId::Uid(1000), UNPRIV).unwrap();
        assert_eq!((n.ruid, n.euid, n.suid, n.fsuid), (1000, 1000, 0, 1000));
        assert_eq!(apply(&c, &SetId::Uid(2000), UNPRIV), Err(libc_errno::EPERM));
        // Privileged setuid sets all four.
        let (n, _) = apply(&c, &SetId::Uid(2000), PRIV).unwrap();
        assert_eq!((n.ruid, n.euid, n.suid, n.fsuid), (2000, 2000, 2000, 2000));
        assert_eq!(apply(&c, &SetId::Uid(NO_ID), PRIV), Err(libc_errno::EINVAL));
    }

    #[test]
    fn setreuid_saved_uid() {
        // Swapping real and effective (the classic setreuid(geteuid(), getuid())).
        let c = creds(1000, 2000, 2000);
        let (n, _) = apply(&c, &SetId::Reuid(2000, 1000), UNPRIV).unwrap();
        assert_eq!((n.ruid, n.euid, n.suid), (2000, 1000, 1000));
        // Setting only the euid, to the real uid, leaves the saved uid alone.
        let (n, _) = apply(&c, &SetId::Reuid(NO_ID, 1000), UNPRIV).unwrap();
        assert_eq!((n.ruid, n.euid, n.suid), (1000, 1000, 2000));
        assert_eq!(
            apply(&c, &SetId::Reuid(3000, NO_ID), UNPRIV),
            Err(libc_errno::EPERM)
        );
    }

    #[test]
    fn setresuid_unprivileged() {
        let c = creds(1000, 2000, 3000);
        let (n, _) = apply(&c, &SetId::Resuid(3000, 1000, NO_ID), UNPRIV).unwrap();
        assert_eq!((n.ruid, n.euid, n.suid, n.fsuid), (3000, 1000, 3000, 1000));
        assert_eq!(
            apply(&c, &SetId::Resuid(NO_ID, 4000, NO_ID), UNPRIV),
            Err(libc_errno::EPERM)
        );
    }

    #[test]
    fn setfsuid_returns_previous() {
        let c = creds(1000, 1000, 1000);
        // setfsuid(-1) is a query and changes nothing.
        let (n, r) = apply(&c, &SetId::Fsuid(NO_ID), UNPRIV).unwrap();
        assert_eq!((r, n.fsuid), (1000, 1000));
        // A disallowed change leaves the fs uid unchanged without an error.
        let (n, r) = apply(&c, &SetId::Fsuid(0), UNPRIV).unwrap();
        assert_eq!((r, n.fsuid), (1000, 1000));
        let (n, r) = apply(&c, &SetId::Fsuid(0), PRIV).unwrap();
        assert_eq!((r, n.fsuid), (1000, 0));
    }

    #[test]
    fn gids_mirror_uids() {
        let c = Creds::root();
        let (c, _) = apply(&c, &SetId::Regid(1000, 1001), PRIV).unwrap();
        assert_eq!((c.rgid, c.egid, c.sgid, c.fsgid), (1000, 1001, 1001, 1001));
        let (c, _) = apply(&c, &SetId::Gid(1000), UNPRIV).unwrap();
        assert_eq!((c.rgid, c.egid, c.sgid), (1000, 1000, 1001));
        assert_eq!(apply(&c, &SetId::Gid(5), UNPRIV), Err(libc_errno::EPERM));
        let (c, prev) = apply(&c, &SetId::Fsgid(1001), UNPRIV).unwrap();
        assert_eq!((prev, c.fsgid), (1000, 1001));
        // gid changes leave uids alone.
        assert_eq!((c.ruid, c.euid), (0, 0));
    }

    #[test]
    fn groups_limits() {
        let c = Creds::root();
        let too_many = vec![1; NGROUPS_MAX + 1];
        assert_eq!(
            apply(&c, &SetId::Groups(too_many), PRIV),
            Err(libc_errno::EINVAL)
        );
        let (c, _) = apply(&c, &SetId::Groups(vec![5, 3]), PRIV).unwrap();
        assert_eq!(getgroups_count(&c, 0), Ok(2));
        assert_eq!(getgroups_count(&c, 2), Ok(2));
        assert_eq!(getgroups_count(&c, 1), Err(libc_errno::EINVAL));
    }

    #[test]
    fn table_interning_and_encoding() {
        let mut t = CredTable::new();
        assert_eq!(t.intern(&Creds::root()), 0);
        let a = creds(1000, 1000, 1000);
        let id = t.intern(&a);
        assert_eq!(id, 1);
        assert_eq!(t.intern(&a.clone()), 1);
        assert_eq!(t.lookup(CredTable::encode(id)), &a);
        assert_eq!(t.lookup(RLIM_INFINITY), &Creds::root());
        assert_eq!(CredTable::encode(0), RLIM_INFINITY);
        // Foreign values read as root.
        assert_eq!(t.lookup(12345), &Creds::root());
        assert_eq!(t.by_id.len(), 2);
    }

    #[test]
    fn proc_status_paths() {
        assert_eq!(
            parse_proc_status(b"/proc/self/status"),
            Some(ProcStatus::Myself)
        );
        assert_eq!(
            parse_proc_status(b"/proc/thread-self/status"),
            Some(ProcStatus::MyThread)
        );
        assert_eq!(
            parse_proc_status(b"/proc/348/status"),
            Some(ProcStatus::Pid(348))
        );
        assert_eq!(
            parse_proc_status(b"/proc/348/task/350/status"),
            Some(ProcStatus::Task(348, 350))
        );
        for bad in [
            &b"/proc/348/stat"[..],
            b"/proc/self/statusx",
            b"/proc//status",
            b"/proc/-1/status",
            b"/proc/1/2/status",
            b"/proc/net/status",
            b"/data/proc/1/status",
        ] {
            assert_eq!(
                parse_proc_status(bad),
                None,
                "{}",
                String::from_utf8_lossy(bad)
            );
        }
    }

    #[test]
    fn status_rewrite() {
        let real = "Name:\tsystem_server\nUmask:\t0077\nState:\tS (sleeping)\n\
                    Uid:\t0\t0\t0\t0\nGid:\t0\t0\t0\t0\nFDSize:\t512\n\
                    Groups:\t65534 65534 0 \nNStgid:\t348\n";
        let mut c = creds(1000, 1000, 1000);
        c.rgid = 1000;
        c.egid = 1000;
        c.sgid = 1000;
        c.fsgid = 1000;
        c.groups = vec![1004, 1007];
        let v = rewrite_status(real, &c);
        assert!(v.contains("\nUid:\t1000\t1000\t1000\t1000\n"));
        assert!(v.contains("\nGid:\t1000\t1000\t1000\t1000\n"));
        assert!(v.contains("\nGroups:\t1004 1007 \n"));
        assert!(v.starts_with("Name:\tsystem_server\n"));
        assert!(v.ends_with("NStgid:\t348\n"));
        // Root with no groups gets an empty Groups line, as from the kernel.
        assert!(rewrite_status(real, &Creds::root()).contains("\nGroups:\t\n"));
    }

    /// xorshift64*, so the randomized tests need no dependencies.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
        }
        fn id(&mut self) -> u32 {
            // Mostly a few interesting ids (root, system, an app, -1).
            match self.next() % 6 {
                0 => 0,
                1 => 1000,
                2 => 10050,
                3 => NO_ID,
                _ => (self.next() % 20000) as u32,
            }
        }
    }

    /// Random set*id sequences against the kernel's invariants.
    #[test]
    fn setid_invariants_randomized() {
        let mut r = Rng(0x5eed_1234_abcd_ef01);
        let mut t = CredTable::new();
        for _ in 0..20 {
            let mut c = Creds::root();
            for _ in 0..2000 {
                let op = match r.next() % 9 {
                    0 => SetId::Uid(r.id()),
                    1 => SetId::Reuid(r.id(), r.id()),
                    2 => SetId::Resuid(r.id(), r.id(), r.id()),
                    3 => SetId::Fsuid(r.id()),
                    4 => SetId::Gid(r.id()),
                    5 => SetId::Regid(r.id(), r.id()),
                    6 => SetId::Resgid(r.id(), r.id(), r.id()),
                    7 => SetId::Fsgid(r.id()),
                    _ => SetId::Groups((0..r.next() % 5).map(|_| r.id()).collect()),
                };
                let caps = Caps {
                    setuid: r.next().is_multiple_of(4),
                    setgid: r.next().is_multiple_of(4),
                };
                let Ok((n, ret)) = apply(&c, &op, caps) else {
                    continue;
                };
                // Without CAP_SETUID no new uid appears. The same holds for
                // gids.
                let olduids = [c.ruid, c.euid, c.suid, c.fsuid];
                let oldgids = [c.rgid, c.egid, c.sgid, c.fsgid];
                if !caps.setuid {
                    for u in [n.ruid, n.euid, n.suid, n.fsuid] {
                        assert!(olduids.contains(&u), "{op:?}: {c:?} -> {n:?}");
                    }
                }
                if !caps.setgid {
                    for g in [n.rgid, n.egid, n.sgid, n.fsgid] {
                        assert!(oldgids.contains(&g), "{op:?}: {c:?} -> {n:?}");
                    }
                    assert_eq!(n.groups, c.groups);
                }
                // -1 is never stored.
                for v in [
                    n.ruid, n.euid, n.suid, n.fsuid, n.rgid, n.egid, n.sgid, n.fsgid,
                ] {
                    assert_ne!(v, NO_ID, "{op:?}");
                }
                match op {
                    SetId::Fsuid(_) | SetId::Fsgid(_) => {
                        let prev = if matches!(op, SetId::Fsuid(_)) {
                            c.fsuid
                        } else {
                            c.fsgid
                        };
                        assert_eq!(ret, prev as i64);
                    }
                    _ => {
                        assert_eq!(ret, 0);
                        // The other id class never changes.
                        if matches!(op, SetId::Uid(_) | SetId::Reuid(..) | SetId::Resuid(..)) {
                            assert_eq!(n.fsuid, n.euid);
                            assert_eq!(oldgids, [n.rgid, n.egid, n.sgid, n.fsgid]);
                        } else {
                            assert_eq!(olduids, [n.ruid, n.euid, n.suid, n.fsuid]);
                        }
                        if matches!(op, SetId::Gid(_) | SetId::Regid(..) | SetId::Resgid(..)) {
                            assert_eq!(n.fsgid, n.egid);
                        }
                    }
                }
                assert!(n.groups.windows(2).all(|w| w[0] <= w[1]));
                // Interning round-trips through the rlimit encoding.
                let id = t.intern(&n);
                assert_eq!(t.lookup(CredTable::encode(id)), &n);
                c = n;
            }
        }
    }

    /// Parsers fed garbage never panic, and the rewrite keeps other lines.
    #[test]
    fn parsers_randomized() {
        let mut r = Rng(42);
        let alphabet = b"/proc/selfthread-tasku0123456789:\t\n Gid";
        for _ in 0..50_000 {
            let len = (r.next() % 40) as usize;
            let b: Vec<u8> = (0..len)
                .map(|_| alphabet[(r.next() % alphabet.len() as u64) as usize])
                .collect();
            if let Some(ProcStatus::Pid(p)) = parse_proc_status(&b) {
                assert_eq!(b, format!("/proc/{p}/status").as_bytes());
            }
            let _ = parse_owner(&b);
            let text = String::from_utf8_lossy(&b);
            let out = rewrite_status(&text, &Creds::root());
            let keep = |s: &str| {
                s.split_inclusive('\n')
                    .filter(|l| {
                        !l.starts_with("Uid:")
                            && !l.starts_with("Gid:")
                            && !l.starts_with("Groups:")
                    })
                    .map(|l| l.trim_end_matches('\n').to_string())
                    .collect::<Vec<_>>()
            };
            assert_eq!(keep(&text), keep(&out));
        }
    }

    #[test]
    fn access_bits() {
        // base.apk is system:system 0644. The app may read it but not write
        // it.
        assert!(may_access(0o100644, 1000, 1000, 10050, 10050, &[], 4));
        assert!(!may_access(0o100644, 1000, 1000, 10050, 10050, &[], 2));
        assert!(may_access(0o100644, 1000, 1000, 1000, 1000, &[], 6));
        // An app's own 0700 directory.
        assert!(may_access(0o40700, 10050, 10050, 10050, 10050, &[], 7));
        assert!(!may_access(0o40700, 10050, 10050, 10051, 10051, &[], 1));
        // Group access through a supplementary group (the cache gid).
        assert!(may_access(0o42771, 10050, 20050, 10050, 10050, &[20050], 3));
        assert!(may_access(0o40770, 1000, 1004, 10089, 10089, &[1004], 2));
        // The owner class wins even when it has fewer bits than others.
        assert!(!may_access(0o40077, 10050, 10050, 10050, 10050, &[], 4));
    }

    #[test]
    fn owner_xattr() {
        assert_eq!(parse_owner(b"1000:1007"), Some((1000, 1007)));
        assert_eq!(parse_owner(b"10050:20050\0"), Some((10050, 20050)));
        assert_eq!(parse_owner(b"junk"), None);
        assert_eq!(format_owner(1000, 1000), "1000:1000");
        assert_eq!(chown_owner((1000, 1000), NO_ID, 1007), (1000, 1007));
        assert_eq!(chown_owner((1000, 1000), 0, NO_ID), (0, 1000));
    }
}

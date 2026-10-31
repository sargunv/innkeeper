//! Virtual credentials for Android's uids: the pure model and the syscalls
//! that the broker answers with it.

mod syscalls;
mod table;

pub(crate) use syscalls::{Vid, is_cred_syscall};

//! Single-uid mode's virtual credentials: the pure model and the syscalls
//! that the broker answers with it.

mod syscalls;
mod table;

pub(crate) use syscalls::{Vid, is_cred_syscall};

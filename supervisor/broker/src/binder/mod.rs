//! The binder driver: its state machine and the kernel UAPI it speaks.

pub mod driver;
pub mod uapi;

#[cfg(test)]
mod tests;

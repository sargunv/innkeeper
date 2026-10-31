//! The binder driver: its state machine and the kernel UAPI it speaks.

pub mod driver;
pub mod uapi;

#[cfg(test)]
mod fake;
#[cfg(test)]
mod tests;

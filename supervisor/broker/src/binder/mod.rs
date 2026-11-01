//! The binder driver state machine and the kernel UAPI it implements.

pub mod driver;
pub mod uapi;

#[cfg(test)]
mod fake;
#[cfg(test)]
mod tests;

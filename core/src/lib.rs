//! Linux.apk host library: everything the `linuxapk` CLI and the
//! `linuxapk-gui` manager share. [`ops`] holds the user-level operations
//! (inspect, install, uninstall, run, list, setup); the other modules are
//! their building blocks. See docs/cli.md and docs/gui.md.

pub mod android;
pub mod bundle;
pub mod desktop;
pub mod manifest;
pub mod ops;
pub mod proc;
pub mod session;
pub mod validate;
pub mod xdg;

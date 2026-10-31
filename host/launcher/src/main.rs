//! The Innkeeper session launcher (`innkeeper-session`, host/launcher/README.md),
//! an internal tool. What host/core shares with it is innkeeper-profile.
//! docs/session.md describes the session.

mod adb;
mod cli;
mod conf;
mod control;
mod exec;
mod graphics;
mod image;
mod init;
mod layer;
mod lifecycle;
mod phone;
mod prune;
mod root;
mod sandbox;
mod selftest;
mod session;
mod sys;

pub use innkeeper_profile::{Paths, Status, field, paths, preferences, profile, status};

fn main() {
    cli::main()
}

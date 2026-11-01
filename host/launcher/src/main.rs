//! The Innkeeper session launcher (`innkeeper-session`), an internal tool
//! described in host/launcher/README.md. It shares code with host/core through
//! the innkeeper-profile crate. docs/session.md describes the session.

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
mod ohos;
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

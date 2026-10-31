//! Starting Android init (second stage) under the broker, with a clean
//! environment plus what the broker and the shim read.

use std::path::Path;
use std::process::Command;

use crate::conf::Config;

/// `innkeeper-brk --preload <shim> /system/bin/init second_stage`, plus
/// `extra` variables.
pub fn command(cfg: &Config, extra: &[(&str, String)]) -> Command {
    let mut c = Command::new("/system/bin/innkeeper-brk");
    c.args([
        // Absolute: a bare name is not found from APEX linker namespaces.
        "--preload",
        "/system/lib64/libinnkeeper_shim.so",
        "/system/bin/init",
        "second_stage",
    ]);
    c.current_dir("/")
        .env_clear()
        .env("PATH", "/system/bin:/system/xbin:/vendor/bin")
        .env(
            "INNKEEPER_RENDER_NODE",
            cfg.render_node.as_deref().unwrap_or(Path::new("")),
        );
    if let Some(d) = &cfg.broker_debug {
        c.env("INNKEEPER_BROKER_DEBUG", d);
    }
    for (k, v) in extra {
        c.env(k, v);
    }
    c
}

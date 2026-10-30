//! Starting Android init (second stage) through pivot-exec, with a clean
//! environment plus what pivot-exec, the broker and the shim read.

use std::path::Path;
use std::process::Command;

use crate::conf::Config;

/// `pivot-exec <root> /system/bin/init second_stage`, plus `extra` variables.
pub fn command(cfg: &Config, root: &Path, extra: &[(&str, String)]) -> Command {
    let mut c = Command::new(cfg.paths.libexec.join("pivot-exec"));
    c.arg(root).args(["/system/bin/init", "second_stage"]);
    c.env_clear()
        .env("PATH", "/system/bin:/system/xbin:/vendor/bin")
        // Absolute: a bare name is not found from APEX linker namespaces.
        .env("LD_PRELOAD", "/system/lib64/liblinuxapk_shim.so")
        .env("LINUXAPK_BROKER", "/system/bin/linuxapk-broker")
        .env(
            "LINUXAPK_RENDER_NODE",
            cfg.render_node.as_deref().unwrap_or(Path::new("")),
        );
    if let Some(d) = &cfg.broker_debug {
        c.env("LINUXAPK_BROKER_DEBUG", d);
    }
    for (k, v) in extra {
        c.env(k, v);
    }
    c
}

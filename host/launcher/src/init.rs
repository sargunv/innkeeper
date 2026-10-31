//! Starting the guest's init (second stage) under the broker, with a clean
//! environment plus what the broker and the shim read.

use std::path::Path;
use std::process::Command;

use crate::conf::Config;
use crate::profile::System;

/// `innkeeper-brk --preload <shim> /system/bin/init <second stage>`, plus
/// `extra` variables. The image's first stage only mounts block devices and
/// switches root.
pub fn command(cfg: &Config, extra: &[(&str, String)]) -> Command {
    let mut c = Command::new(cfg.paths.libexec.join("innkeeper-brk"));
    // The shim's path is absolute: a bare name is not found from APEX linker
    // namespaces.
    let (shim, stage, path): (_, &[&str], _) = match cfg.system {
        System::Android => (
            "/system/lib64/libinnkeeper_shim.so",
            &["second_stage"],
            "/system/bin:/system/xbin:/vendor/bin",
        ),
        System::Ohos => (
            "/sys_prod/lib64/libinnkeeper_ohos_shim.so",
            &["--second-stage", "0"],
            "/system/bin:/vendor/bin",
        ),
    };
    c.args(["--preload", shim, "/system/bin/init"]).args(stage);
    c.current_dir("/")
        .env_clear()
        .env("PATH", path)
        .env("INNKEEPER_GUEST", cfg.system.name())
        .env(
            "INNKEEPER_RENDER_NODE",
            cfg.render_node.as_deref().unwrap_or(Path::new("")),
        );
    if cfg.system == System::Ohos {
        // The image's Mesa has no radeonsi, and CI has no GPU
        // (ohos/README.md, "GPU").
        c.env("LIBGL_ALWAYS_SOFTWARE", "1");
    }
    if let Some(d) = &cfg.broker_debug {
        c.env("INNKEEPER_BROKER_DEBUG", d);
    }
    for (k, v) in extra {
        c.env(k, v);
    }
    c
}

//! `exec`: a command inside the live session as the guest's root, through
//! the exec relay. Binder needs the seccomp filter only init's descendants
//! have, so the relay (a static binary, android/system/exec, which
//! OpenHarmony's layer builds too) hands the command to its server under
//! init, which runs it there.

use std::fs;

use crate::profile::System;

/// The relay's path in the guest's root.
pub fn relay(system: System) -> &'static str {
    match system {
        System::Android => "/system/bin/innkeeper-exec",
        System::Ohos => "/sys_prod/bin/innkeeper-exec",
    }
}

/// The guest's environment for a command.
pub fn env(system: System) -> Vec<(String, String)> {
    match system {
        System::Android => android_env(),
        System::Ohos => vec![
            ("PATH".into(), "/system/bin:/vendor/bin".into()),
            ("HOME".into(), "/data".into()),
        ],
    }
}

/// Android's environment for a command: init.environ.rc's and the derived
/// classpath's `export NAME VALUE` lines, read from the session's root (this
/// process's `/`), plus a shell's PATH and HOME.
fn android_env() -> Vec<(String, String)> {
    let mut env = vec![];
    for f in ["/init.environ.rc", "/data/system/environ/classpath"] {
        for l in fs::read_to_string(f).unwrap_or_default().lines() {
            let Some(rest) = l.trim_start().strip_prefix("export ") else {
                continue;
            };
            if let Some((k, v)) = rest.trim_start().split_once(' ') {
                env.push((k.to_string(), v.to_string()));
            }
        }
    }
    env.push((
        "PATH".into(),
        "/product/bin:/apex/com.android.runtime/bin:/apex/com.android.art/bin:/system_ext/bin:\
         /system/bin:/system/xbin:/odm/bin:/vendor/bin:/vendor/xbin"
            .into(),
    ));
    env.push(("HOME".into(), "/data/local/tmp".into()));
    env.push(("TMPDIR".into(), "/data/local/tmp".into()));
    env
}

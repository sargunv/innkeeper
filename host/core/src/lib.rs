//! Linux.apk host library: everything the `linuxapk` CLI and the
//! `linuxapk-manager` app share. [`ops`] holds the user-level operations
//! (inspect, install, uninstall, run, list); the other modules are their
//! building blocks. See host/core/README.md.

pub mod android;
pub mod apk;
pub mod app_settings;
pub mod bundle;
pub mod desktop;
pub mod dynlauncher;
pub mod launcher;
pub mod ops;
pub mod paths;
pub mod proc;
pub mod validate;
pub mod xdg;

/// A fresh, empty directory for one test's files: `<repo>/out/test/core/<name>`.
#[cfg(test)]
fn test_dir(name: &str) -> std::path::PathBuf {
    let d = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../out/test/core")
        .join(name);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

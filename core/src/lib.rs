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

/// Where tests find the compat corpus APKs (fetched by `compat/run.sh fetch`,
/// never committed): `$LINUXAPK_CORPUS`, else `<repo>/out/corpus`.
#[cfg(test)]
fn test_corpus() -> std::path::PathBuf {
    std::env::var_os("LINUXAPK_CORPUS").map_or_else(
        || std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../out/corpus"),
        std::path::PathBuf::from,
    )
}

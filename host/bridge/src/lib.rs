//! The Innkeeper desktop bridge, which serves Android clients over
//! `bridge.sock`. `host/bridge/README.md` lists the services and their ids,
//! and `docs/bridge-protocol.md` describes the protocol.

pub mod a11y;
pub mod apps;
pub mod audio;
pub mod display;
pub mod documents;
pub mod hoststate;
pub mod input;
pub mod ipc;
pub mod log;
pub mod session;
pub mod system;

/// Creates a fresh, empty directory for one test's files at
/// `<repo>/out/test/bridge/<name>`.
#[cfg(test)]
fn test_dir(name: &str) -> std::path::PathBuf {
    let d = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../out/test/bridge")
        .join(name);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

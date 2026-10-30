//! One render node and Vulkan feature set for the whole Android session.

use std::fs;
use std::path::{Path, PathBuf};

/// Only RADV is shipped for hardware rendering. Unsupported nodes must not
/// hide a supported GPU later in sysfs (for example on a hybrid laptop).
pub fn select_node(drm: &Path, dev: &Path) -> Option<PathBuf> {
    let mut nodes: Vec<_> = fs::read_dir(drm)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_prefix("renderD"))
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        })
        .collect();
    nodes.sort();
    nodes.into_iter().find_map(|node| {
        let driver = fs::canonicalize(node.join("device/driver")).ok()?;
        if driver.file_name()? != "amdgpu" {
            return None;
        }
        let node = dev.join(node.file_name()?);
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&node)
            .ok()?;
        Some(node)
    })
}

pub fn driver(node: Option<&Path>) -> &'static str {
    if node.is_some() { "radv" } else { "lvp" }
}

/// Both shipped drivers support Vulkan 1.3 and compute level 0. Software
/// rendering does not satisfy Android's hardware Vulkan level 1 guarantee.
pub fn feature_xmls(node: Option<&Path>) -> Vec<(String, String)> {
    [("level", u32::from(node.is_some())), ("version", 4_206_592), ("compute", 0)]
        .into_iter()
        .map(|(feature, version)| {
            let name = format!("android.hardware.vulkan.{feature}");
            let xml = format!("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<permissions>\n    <feature name=\"{name}\" version=\"{version}\" />\n</permissions>\n");
            (format!("{name}.xml"), xml)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn selection_scans_all_nodes() {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        for (nodes, expected) in [
            (vec![(128, "amdgpu")], Some(128)),
            (vec![(128, "i915"), (129, "amdgpu")], Some(129)),
            (vec![(128, "amdgpu"), (129, "i915")], Some(128)),
            (vec![(128, "i915")], None),
            (vec![], None),
            (vec![(128, "unreadable"), (129, "amdgpu")], Some(129)),
            (vec![(128, "unreadable")], None),
            (vec![(128, "amdgpu-unreadable"), (129, "amdgpu")], Some(129)),
            (vec![(128, "amdgpu-unreadable")], None),
        ] {
            let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
                "../../out/test/render-nodes-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(root.join("dev")).unwrap();
            for (number, driver) in nodes {
                fs::write(root.join(format!("dev/renderD{number}")), "").unwrap();
                let device = root.join(format!("renderD{number}/device"));
                fs::create_dir_all(&device).unwrap();
                if driver == "amdgpu-unreadable" {
                    fs::set_permissions(
                        root.join(format!("dev/renderD{number}")),
                        fs::Permissions::from_mode(0o0),
                    )
                    .unwrap();
                }
                let target = root.join(driver.strip_suffix("-unreadable").unwrap_or(driver));
                // A dangling link models a node whose driver cannot be read.
                if driver != "unreadable" {
                    fs::create_dir_all(&target).unwrap();
                }
                symlink(target, device.join("driver")).unwrap();
            }
            assert_eq!(
                select_node(&root, &root.join("dev")),
                expected.map(|n| root.join(format!("dev/renderD{n}")))
            );
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn vulkan_features_match_the_driver() {
        for (node, vk, level) in [
            (Some(Path::new("/dev/dri/renderD129")), "radv", 1),
            (None, "lvp", 0),
        ] {
            assert_eq!(driver(node), vk);
            let xmls = feature_xmls(node);
            assert_eq!(xmls.len(), 3);
            for (feature, version) in [("level", level), ("version", 4_206_592), ("compute", 0)] {
                let name = format!("android.hardware.vulkan.{feature}");
                assert!(xmls.iter().any(|(file, xml)| file == &format!("{name}.xml")
                    && xml.contains(&format!("name=\"{name}\" version=\"{version}\""))));
            }
        }
    }
}

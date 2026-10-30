//! One render node and Vulkan feature set for the whole Android session.

use std::fs;
use std::path::{Path, PathBuf};

/// Prefer RADV on hybrid hosts, then ANV on i915. Unsupported nodes must not
/// hide a supported GPU later in sysfs. Return the matching HAL with its node.
pub fn select_node(drm: &Path, dev: &Path) -> Option<(PathBuf, &'static str)> {
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
    let mut supported: Vec<_> = nodes
        .into_iter()
        .filter_map(|node| {
            let driver = fs::canonicalize(node.join("device/driver")).ok()?;
            let (priority, hal) = match driver.file_name()?.to_str()? {
                "amdgpu" => (0, "radv"),
                "i915" => (1, "anv"),
                _ => return None,
            };
            let node = dev.join(node.file_name()?);
            fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&node)
                .ok()?;
            Some((priority, node, hal))
        })
        .collect();
    supported.sort_by_key(|(priority, _, _)| *priority);
    supported
        .into_iter()
        .next()
        .map(|(_, node, hal)| (node, hal))
}

/// All shipped drivers support Vulkan 1.3 and compute level 0. Software
/// rendering does not satisfy Android's hardware Vulkan level 1 guarantee.
pub fn feature_xmls(driver: &str) -> Vec<(String, String)> {
    [("level", u32::from(driver != "lvp")), ("version", 4_206_592), ("compute", 0)]
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
            (vec![(128, "amdgpu")], Some((128, "radv"))),
            (vec![(128, "i915"), (129, "amdgpu")], Some((129, "radv"))),
            (vec![(128, "amdgpu"), (129, "i915")], Some((128, "radv"))),
            (vec![(128, "i915")], Some((128, "anv"))),
            (vec![(128, "nouveau"), (129, "i915")], Some((129, "anv"))),
            (vec![(128, "i915-unreadable")], None),
            (vec![], None),
            (
                vec![(128, "unreadable"), (129, "amdgpu")],
                Some((129, "radv")),
            ),
            (vec![(128, "unreadable")], None),
            (
                vec![(128, "amdgpu-unreadable"), (129, "amdgpu")],
                Some((129, "radv")),
            ),
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
                if driver.ends_with("-unreadable") {
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
                expected.map(|(n, hal)| (root.join(format!("dev/renderD{n}")), hal))
            );
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn vulkan_features_match_the_driver() {
        for (vk, level) in [("radv", 1), ("anv", 1), ("lvp", 0)] {
            let xmls = feature_xmls(vk);
            assert_eq!(xmls.len(), 3);
            for (feature, version) in [("level", level), ("version", 4_206_592), ("compute", 0)] {
                let name = format!("android.hardware.vulkan.{feature}");
                assert!(xmls.iter().any(|(file, xml)| file == &format!("{name}.xml")
                    && xml.contains(&format!("name=\"{name}\" version=\"{version}\""))));
            }
        }
    }
}

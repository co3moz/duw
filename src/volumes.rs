//! Mounted volumes and attribution of the existing scan. Queried on demand;
//! no additional filesystem walk is needed.

use crate::tree::{ExtStat, Kind, Tree, NO_EXT, ROOT};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use sysinfo::{DiskKind, Disks};

#[derive(Serialize)]
pub struct Volume {
    pub name: String,
    pub mount_point: String,
    pub file_system: String,
    pub kind: &'static str,
    pub removable: bool,
    pub total: u64,
    pub available: u64,
    pub used: u64,
    pub contains_root: bool,
    pub scanned_size: u64,
    pub scanned_alloc: u64,
    pub types: Vec<ExtStat>,
    #[serde(skip)]
    mount_path: PathBuf,
}

#[derive(Serialize)]
pub struct VolumeSummary {
    pub volumes: Vec<Volume>,
    pub root: String,
    pub scanning: bool,
    pub version: u64,
    pub unattributed_alloc: u64,
}

pub fn inventory() -> Vec<Volume> {
    Disks::new_with_refreshed_list()
        .list()
        .iter()
        .filter(|d| d.total_space() > 0)
        .map(|d| {
            let total = d.total_space();
            let available = d.available_space().min(total);
            Volume {
                name: d.name().to_string_lossy().into_owned(),
                mount_point: d.mount_point().to_string_lossy().into_owned(),
                file_system: d.file_system().to_string_lossy().into_owned(),
                kind: match d.kind() {
                    DiskKind::SSD => "SSD",
                    DiskKind::HDD => "HDD",
                    DiskKind::Unknown(_) => "Unknown",
                },
                removable: d.is_removable(),
                total,
                available,
                used: total - available,
                contains_root: false,
                scanned_size: 0,
                scanned_alloc: 0,
                types: Vec::new(),
                mount_path: normalized(d.mount_point()),
            }
        })
        .collect()
}

fn normalized(path: &Path) -> PathBuf {
    dunce::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

pub fn summarize(tree: &Tree, root: &Path, mut volumes: Vec<Volume>) -> VolumeSummary {
    // The server root was canonicalized at startup. Inventory paths were
    // resolved before taking the tree lock; aggregation does no filesystem IO.
    let root_path = root.to_path_buf();
    let mounts: Vec<_> = volumes.iter().map(|v| v.mount_path.clone()).collect();
    let root_volume = mounts
        .iter()
        .enumerate()
        .filter(|(_, mount)| root_path.starts_with(mount))
        .max_by_key(|(_, mount)| mount.components().count())
        .map(|(index, _)| index);
    if let Some(index) = root_volume {
        volumes[index].contains_root = true;
    }
    // Resolve only mount paths. The walk counts each entry's own bytes once,
    // without constructing a path per file or counting folder rollups twice.
    let mut boundaries = HashMap::new();
    for (index, mount) in mounts.iter().enumerate() {
        if let Ok(rel) = mount.strip_prefix(&root_path) {
            let rel = rel
                .components()
                .map(|part| part.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            if let Some(id) = tree.find_rel_path(&rel) {
                boundaries.insert(id, index);
            }
        }
    }
    let mut types: Vec<HashMap<u32, (u64, u64, u64)>> =
        (0..volumes.len()).map(|_| HashMap::new()).collect();
    let mut unattributed_alloc = 0u64;
    let mut stack = vec![(ROOT, root_volume)];
    while let Some((id, inherited)) = stack.pop() {
        let node = &tree.nodes[id as usize];
        if node.removed {
            continue;
        }
        let owner = boundaries.get(&id).copied().or_else(|| {
            // Don't charge unlisted foreign mounts to the root disk.
            if node.foreign && inherited == root_volume {
                None
            } else {
                inherited
            }
        });
        if let Some(index) = owner {
            volumes[index].scanned_size += node.self_size;
            volumes[index].scanned_alloc += node.self_alloc;
            if node.kind == Kind::File {
                let stat = types[index].entry(node.ext).or_default();
                stat.0 += node.self_size;
                stat.1 += node.self_alloc;
                stat.2 += 1;
            }
        } else {
            unattributed_alloc += node.self_alloc;
        }
        stack.extend(node.children.iter().map(|&child| (child, owner)));
    }
    for (volume, stats) in volumes.iter_mut().zip(types) {
        volume.types = stats
            .into_iter()
            .map(|(ext, (size, alloc, count))| ExtStat {
                ext: if ext == NO_EXT {
                    String::new()
                } else {
                    tree.exts[ext as usize].clone()
                },
                size,
                alloc,
                count,
            })
            .collect();
        volume
            .types
            .sort_unstable_by_key(|s| std::cmp::Reverse(s.alloc));
    }
    volumes.sort_by(|a, b| {
        b.contains_root
            .cmp(&a.contains_root)
            .then_with(|| a.mount_point.cmp(&b.mount_point))
    });
    VolumeSummary {
        volumes,
        root: root.display().to_string(),
        scanning: false,
        version: tree.version,
        unattributed_alloc,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::NewEntry;

    fn volume(mount: &Path) -> Volume {
        Volume {
            name: "test".into(),
            mount_point: mount.display().to_string(),
            file_system: "testfs".into(),
            kind: "SSD",
            removable: false,
            total: 1000,
            available: 400,
            used: 600,
            contains_root: false,
            scanned_size: 0,
            scanned_alloc: 0,
            types: Vec::new(),
            mount_path: mount.to_path_buf(),
        }
    }
    fn entry(name: &str, kind: Kind, size: u64, foreign: bool) -> NewEntry {
        NewEntry {
            name: name.into(),
            kind,
            size,
            alloc: size,
            mtime: 0,
            err: false,
            cloud: false,
            foreign,
        }
    }
    #[test]
    fn nested_mounts_and_removed_subtrees_are_not_double_counted() {
        let root = std::env::temp_dir().join("duw-volume-attribution");
        let mut tree = Tree::new("root".into(), 2, 2, 0);
        let dirs = tree.add_children(
            ROOT,
            vec![
                entry("a.txt", Kind::File, 10, false),
                entry("mounted", Kind::Dir, 3, true),
                entry("unlisted", Kind::Dir, 0, true),
                entry("gone", Kind::Dir, 0, false),
            ],
        );
        tree.add_children(dirs[0], vec![entry("v.mp4", Kind::File, 80, false)]);
        tree.add_children(dirs[1], vec![entry("b.bin", Kind::File, 20, false)]);
        tree.add_children(dirs[2], vec![entry("c.txt", Kind::File, 90, false)]);
        tree.remove(dirs[2]);
        let summary = summarize(
            &tree,
            &root,
            vec![volume(&root), volume(&root.join("mounted"))],
        );
        assert_eq!(summary.volumes[0].scanned_alloc, 12);
        assert_eq!(summary.volumes[1].scanned_alloc, 83);
        assert_eq!(summary.unattributed_alloc, 20);
        assert_eq!(summary.volumes[0].types[0].ext, "txt");
        assert_eq!(summary.volumes[1].types[0].ext, "mp4");
        assert_eq!(summary.volumes[0].types[0].count, 1);
    }
    #[test]
    fn deepest_mount_and_path_component_boundaries() {
        let base = std::env::temp_dir().join("duw-volume-boundaries");
        let tree = Tree::new("root".into(), 7, 7, 0);
        let summary = summarize(
            &tree,
            &base.join("nested/project"),
            vec![volume(&base), volume(&base.join("nested"))],
        );
        assert_eq!(
            summary.volumes[0].mount_point,
            base.join("nested").display().to_string()
        );
        assert_eq!(summary.volumes[0].scanned_alloc, 7);
        assert_eq!(summary.volumes[1].scanned_alloc, 0);
        let summary = summarize(
            &tree,
            &base.join("nested-other"),
            vec![volume(&base.join("nested"))],
        );
        assert!(!summary.volumes[0].contains_root);
        assert_eq!(summary.unattributed_alloc, 7);
    }
}

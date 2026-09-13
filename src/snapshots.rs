//! Saved scans, so the current tree can be compared against an earlier one.
//!
//! A snapshot is a flat list of leaf entries with scan-root relative paths,
//! stored as JSON under the platform's data directory. Directories are left
//! out because their sizes are just sums of the leaves.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::tree::{Kind, SnapEntry};

#[derive(Serialize, Deserialize)]
pub struct Snapshot {
    pub name: String,
    /// Scan root at the time the snapshot was taken, for display only.
    pub root: String,
    /// Unix seconds.
    pub created: u64,
    pub entries: Vec<SnapEntry>,
}

#[derive(Serialize)]
pub struct SnapshotMeta {
    pub name: String,
    pub root: String,
    pub created: u64,
    pub entries: u64,
    pub bytes: u64,
}

impl SnapshotMeta {
    pub fn of(snapshot: &Snapshot) -> Self {
        SnapshotMeta {
            name: snapshot.name.clone(),
            root: snapshot.root.clone(),
            created: snapshot.created,
            entries: snapshot.entries.len() as u64,
            bytes: snapshot.entries.iter().map(|e| e.size).sum(),
        }
    }
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[derive(Serialize)]
pub struct Change {
    pub path: String,
    pub kind: Kind,
    pub old: u64,
    pub new: u64,
    pub delta: i64,
    pub added: bool,
    pub removed: bool,
}

#[derive(Serialize)]
pub struct DiffResult {
    /// Bytes that appeared or grew.
    pub added_bytes: u64,
    /// Bytes that disappeared or shrank.
    pub removed_bytes: u64,
    /// `added_bytes - removed_bytes`.
    pub net: i64,
    pub total_changes: u64,
    pub changes: Vec<Change>,
}

/// Where snapshots live. Created on first use.
pub fn dir() -> std::io::Result<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
    };
    let dir = base
        .unwrap_or_else(std::env::temp_dir)
        .join("duw")
        .join("snapshots");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// A snapshot name becomes a file name, so it may not contain separators or
/// traversal, and it is kept reasonably short.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name != "."
        && name != ".."
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || " -_.".contains(c))
}

fn path_for(name: &str) -> std::io::Result<PathBuf> {
    if !valid_name(name) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid snapshot name",
        ));
    }
    Ok(dir()?.join(format!("{name}.json")))
}

pub fn save(snapshot: &Snapshot) -> std::io::Result<()> {
    let path = path_for(&snapshot.name)?;
    let data = serde_json::to_vec(snapshot).map_err(std::io::Error::other)?;
    std::fs::write(path, data)
}

pub fn load(name: &str) -> std::io::Result<Snapshot> {
    let path = path_for(name)?;
    let data = std::fs::read(path)?;
    serde_json::from_slice(&data).map_err(std::io::Error::other)
}

pub fn delete(name: &str) -> std::io::Result<()> {
    std::fs::remove_file(path_for(name)?)
}

pub fn list() -> std::io::Result<Vec<SnapshotMeta>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir()?)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        // A file that no longer parses is not worth failing the whole list for.
        let Ok(data) = std::fs::read(&path) else {
            continue;
        };
        let Ok(snapshot) = serde_json::from_slice::<Snapshot>(&data) else {
            continue;
        };
        out.push(SnapshotMeta::of(&snapshot));
    }
    out.sort_unstable_by_key(|m| std::cmp::Reverse(m.created));
    Ok(out)
}

/// Compares the current entries against a saved snapshot, biggest absolute
/// change first.
pub fn diff(old: &[SnapEntry], new: &[SnapEntry], limit: usize) -> DiffResult {
    let old_map: HashMap<&str, &SnapEntry> = old.iter().map(|e| (e.path.as_str(), e)).collect();
    let new_map: HashMap<&str, &SnapEntry> = new.iter().map(|e| (e.path.as_str(), e)).collect();

    let mut changes = Vec::new();
    let mut added_bytes = 0u64;
    let mut removed_bytes = 0u64;

    for (path, n) in &new_map {
        match old_map.get(path) {
            Some(o) if o.size != n.size => {
                let delta = n.size as i64 - o.size as i64;
                if delta > 0 {
                    added_bytes += delta as u64;
                } else {
                    removed_bytes += -delta as u64;
                }
                changes.push(Change {
                    path: (*path).to_string(),
                    kind: n.kind,
                    old: o.size,
                    new: n.size,
                    delta,
                    added: false,
                    removed: false,
                });
            }
            None => {
                added_bytes += n.size;
                changes.push(Change {
                    path: (*path).to_string(),
                    kind: n.kind,
                    old: 0,
                    new: n.size,
                    delta: n.size as i64,
                    added: true,
                    removed: false,
                });
            }
            _ => {}
        }
    }

    for (path, o) in &old_map {
        if !new_map.contains_key(path) {
            removed_bytes += o.size;
            changes.push(Change {
                path: (*path).to_string(),
                kind: o.kind,
                old: o.size,
                new: 0,
                delta: -(o.size as i64),
                added: false,
                removed: true,
            });
        }
    }

    let total_changes = changes.len() as u64;
    changes.sort_unstable_by_key(|c| std::cmp::Reverse(c.delta.unsigned_abs()));
    changes.truncate(limit);

    DiffResult {
        added_bytes,
        removed_bytes,
        net: added_bytes as i64 - removed_bytes as i64,
        total_changes,
        changes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, size: u64) -> SnapEntry {
        SnapEntry {
            path: path.into(),
            kind: Kind::File,
            size,
            alloc: size,
            mtime: 0,
        }
    }

    #[test]
    fn diff_reports_added_removed_and_grown() {
        let old = vec![entry("a", 100), entry("b", 200), entry("c", 300)];
        let new = vec![entry("a", 100), entry("b", 500), entry("d", 50)];
        let d = diff(&old, &new, 100);

        assert_eq!(d.added_bytes, 350);
        assert_eq!(d.removed_bytes, 300);
        assert_eq!(d.net, 50);
        assert_eq!(d.total_changes, 3);

        let paths: Vec<&str> = d.changes.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(paths.len(), 3);
        // The two 300-byte changes tie for first; the 50-byte add is last.
        assert!(paths[..2].contains(&"b") && paths[..2].contains(&"c"));
        assert_eq!(paths[2], "d");
        assert!(d.changes.iter().find(|c| c.path == "d").unwrap().added);
        assert!(d.changes.iter().find(|c| c.path == "c").unwrap().removed);
    }

    #[test]
    fn snapshot_names_are_sanitized() {
        assert!(valid_name("before cleanup"));
        assert!(valid_name("2026-09-13"));
        assert!(!valid_name("../escape"));
        assert!(!valid_name("a/b"));
        assert!(!valid_name(""));
        assert!(!valid_name(".."));
    }
}

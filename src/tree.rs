//! In-memory arena of the scanned filesystem tree.
//!
//! The scanner is the single writer (behind an `RwLock`), HTTP handlers are
//! readers. Sizes are aggregated upwards as soon as a directory is read, so a
//! partially scanned tree is always coherent - that is what makes streaming to
//! the browser possible.

use std::collections::HashMap;

use serde::Serialize;

pub const ROOT: u32 = 0;
/// Sentinel for "this node has no extension".
pub const NO_EXT: u32 = u32::MAX;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Dir,
    File,
    Link,
    Other,
}

pub struct Node {
    pub parent: u32,
    pub name: Box<str>,
    pub kind: Kind,
    /// Size of the entry itself (0 aggregated for directories beyond their own inode).
    pub self_size: u64,
    pub self_alloc: u64,
    /// Size of the whole subtree, including `self_size`.
    pub total_size: u64,
    pub total_alloc: u64,
    pub files: u32,
    pub dirs: u32,
    pub mtime: i64,
    pub ext: u32,
    pub depth: u16,
    /// Directory has had its own `read_dir` completed.
    pub read: bool,
    /// Reading this entry failed.
    pub err: bool,
    pub children: Vec<u32>,
}

/// A freshly discovered entry, handed over by the scanner.
pub struct NewEntry {
    pub name: String,
    pub kind: Kind,
    pub size: u64,
    pub alloc: u64,
    pub mtime: i64,
    pub err: bool,
}

#[derive(Default, Clone, Serialize)]
pub struct Stats {
    pub files: u64,
    pub dirs: u64,
    pub size: u64,
    pub alloc: u64,
    pub errors: u64,
    pub skipped: u64,
    pub hardlinks: u64,
}

pub struct Tree {
    pub nodes: Vec<Node>,
    pub exts: Vec<String>,
    ext_ids: HashMap<Box<str>, u32>,
    /// Bumped on every mutation so the UI knows when to refetch.
    pub version: u64,
    pub stats: Stats,
    /// Directory the scanner most recently finished reading.
    pub current: String,
    pub errors: Vec<ScanError>,
}

#[derive(Clone, Serialize)]
pub struct ScanError {
    pub path: String,
    pub message: String,
}

const MAX_KEPT_ERRORS: usize = 200;

impl Tree {
    pub fn new(root_name: String, size: u64, alloc: u64, mtime: i64) -> Self {
        let root = Node {
            parent: ROOT,
            name: root_name.into_boxed_str(),
            kind: Kind::Dir,
            self_size: size,
            self_alloc: alloc,
            total_size: size,
            total_alloc: alloc,
            files: 0,
            dirs: 0,
            mtime,
            ext: NO_EXT,
            depth: 0,
            read: false,
            err: false,
            children: Vec::new(),
        };
        Tree {
            nodes: vec![root],
            exts: Vec::new(),
            ext_ids: HashMap::new(),
            version: 0,
            stats: Stats::default(),
            current: String::new(),
            errors: Vec::new(),
        }
    }

    fn intern_ext(&mut self, ext: &str) -> u32 {
        if let Some(id) = self.ext_ids.get(ext) {
            return *id;
        }
        let id = self.exts.len() as u32;
        self.exts.push(ext.to_string());
        self.ext_ids.insert(ext.into(), id);
        id
    }

    /// Insert the children of `parent`, returning the ids of the child
    /// directories in the same order they appeared in `entries`.
    pub fn add_children(&mut self, parent: u32, entries: Vec<NewEntry>) -> Vec<u32> {
        let depth = self.nodes[parent as usize].depth.saturating_add(1);
        let mut d_size = 0u64;
        let mut d_alloc = 0u64;
        let mut d_files = 0u32;
        let mut d_dirs = 0u32;
        let mut dir_ids = Vec::new();
        let first = self.nodes.len() as u32;

        for e in entries {
            let id = self.nodes.len() as u32;
            let ext = if e.kind == Kind::File {
                match extension_of(&e.name) {
                    Some(x) => self.intern_ext(&x),
                    None => NO_EXT,
                }
            } else {
                NO_EXT
            };
            if e.kind == Kind::Dir {
                d_dirs += 1;
                dir_ids.push(id);
            } else {
                d_files += 1;
            }
            d_size += e.size;
            d_alloc += e.alloc;
            self.nodes.push(Node {
                parent,
                name: e.name.into_boxed_str(),
                kind: e.kind,
                self_size: e.size,
                self_alloc: e.alloc,
                total_size: e.size,
                total_alloc: e.alloc,
                files: 0,
                dirs: 0,
                mtime: e.mtime,
                ext,
                depth,
                read: false,
                err: e.err,
                children: Vec::new(),
            });
        }

        let last = self.nodes.len() as u32;
        {
            let p = &mut self.nodes[parent as usize];
            p.children.extend(first..last);
            p.read = true;
        }
        self.propagate(parent, d_size, d_alloc, d_files, d_dirs);

        self.stats.files += d_files as u64;
        self.stats.dirs += d_dirs as u64;
        self.stats.size += d_size;
        self.stats.alloc += d_alloc;
        self.version += 1;
        dir_ids
    }

    /// Mark a directory as read even though it produced no children (empty or
    /// permission denied).
    pub fn mark_read(&mut self, id: u32, err: bool) {
        let n = &mut self.nodes[id as usize];
        n.read = true;
        n.err |= err;
        self.version += 1;
    }

    pub fn record_error(&mut self, path: String, message: String) {
        self.stats.errors += 1;
        if self.errors.len() < MAX_KEPT_ERRORS {
            self.errors.push(ScanError { path, message });
        }
    }

    fn propagate(&mut self, from: u32, size: u64, alloc: u64, files: u32, dirs: u32) {
        let mut id = from;
        loop {
            let n = &mut self.nodes[id as usize];
            n.total_size += size;
            n.total_alloc += alloc;
            n.files += files;
            n.dirs += dirs;
            if id == ROOT {
                break;
            }
            id = n.parent;
        }
    }

    pub fn get(&self, id: u32) -> Option<&Node> {
        self.nodes.get(id as usize)
    }

    /// Root -> `id` chain, root included.
    pub fn breadcrumb(&self, id: u32) -> Vec<Crumb> {
        let mut chain = Vec::new();
        let mut cur = id;
        loop {
            let n = &self.nodes[cur as usize];
            chain.push(Crumb {
                id: cur,
                name: n.name.to_string(),
            });
            if cur == ROOT {
                break;
            }
            cur = n.parent;
        }
        chain.reverse();
        chain
    }

    /// Path of `id` relative to the scan root (empty for the root itself).
    pub fn rel_path(&self, id: u32) -> String {
        let mut parts = Vec::new();
        let mut cur = id;
        while cur != ROOT {
            let n = &self.nodes[cur as usize];
            parts.push(n.name.as_ref());
            cur = n.parent;
        }
        parts.reverse();
        parts.join("/")
    }

    fn ext_name(&self, ext: u32) -> Option<&str> {
        if ext == NO_EXT {
            None
        } else {
            self.exts.get(ext as usize).map(|s| s.as_str())
        }
    }

    fn entry_of(&self, id: u32) -> Entry {
        let n = &self.nodes[id as usize];
        Entry {
            id,
            name: n.name.to_string(),
            kind: n.kind,
            size: n.total_size,
            alloc: n.total_alloc,
            files: n.files,
            dirs: n.dirs,
            mtime: n.mtime,
            read: n.read,
            err: n.err,
            ext: self.ext_name(n.ext).map(|s| s.to_string()),
        }
    }

    /// Direct children of `id`, largest first, capped at `limit`. Anything past
    /// the cap is folded into `other` so the UI never has to render 50k rows.
    pub fn children_view(&self, id: u32, by_alloc: bool, limit: usize) -> Option<ChildrenView> {
        let node = self.get(id)?;
        let mut ids: Vec<u32> = node.children.clone();
        let key = |t: &Tree, i: u32| {
            let n = &t.nodes[i as usize];
            if by_alloc {
                n.total_alloc
            } else {
                n.total_size
            }
        };
        ids.sort_unstable_by(|a, b| {
            key(self, *b).cmp(&key(self, *a)).then_with(|| {
                self.nodes[*a as usize]
                    .name
                    .cmp(&self.nodes[*b as usize].name)
            })
        });

        let shown: Vec<Entry> = ids.iter().take(limit).map(|i| self.entry_of(*i)).collect();
        let mut other = Rollup::default();
        for i in ids.iter().skip(limit) {
            let n = &self.nodes[*i as usize];
            other.count += 1;
            other.size += n.total_size;
            other.alloc += n.total_alloc;
        }

        Some(ChildrenView {
            children: shown,
            other,
        })
    }

    /// A nested slice of the tree for the treemap: at most `depth` levels, the
    /// `limit` biggest entries per level, and never more than `budget` nodes in
    /// total so a pathological directory cannot blow up the response.
    pub fn subtree(
        &self,
        id: u32,
        by_alloc: bool,
        depth: u16,
        limit: usize,
        budget: &mut usize,
    ) -> Option<SubtreeNode> {
        let n = self.get(id)?;
        let mut out = SubtreeNode {
            id,
            name: n.name.to_string(),
            kind: n.kind,
            size: n.total_size,
            alloc: n.total_alloc,
            children: Vec::new(),
        };
        if n.kind != Kind::Dir || depth == 0 || *budget == 0 {
            return Some(out);
        }

        let mut ids = n.children.clone();
        let metric = |t: &Tree, i: u32| {
            let c = &t.nodes[i as usize];
            if by_alloc {
                c.total_alloc
            } else {
                c.total_size
            }
        };
        ids.sort_unstable_by_key(|i| std::cmp::Reverse(metric(self, *i)));

        for child in ids.into_iter().take(limit) {
            if *budget == 0 {
                break;
            }
            if metric(self, child) == 0 {
                break;
            }
            *budget -= 1;
            if let Some(c) = self.subtree(child, by_alloc, depth - 1, limit, budget) {
                out.children.push(c);
            }
        }
        Some(out)
    }

    /// Aggregate the whole subtree of `id` by file extension.
    pub fn by_extension(&self, id: u32) -> Vec<ExtStat> {
        let mut acc: HashMap<u32, ExtAcc> = HashMap::new();
        self.for_each_file(id, |n| {
            let e = acc.entry(n.ext).or_default();
            e.size += n.self_size;
            e.alloc += n.self_alloc;
            e.count += 1;
        });
        let mut out: Vec<ExtStat> = acc
            .into_iter()
            .map(|(ext, a)| ExtStat {
                ext: self.ext_name(ext).unwrap_or("").to_string(),
                size: a.size,
                alloc: a.alloc,
                count: a.count,
            })
            .collect();
        out.sort_unstable_by(|a, b| b.size.cmp(&a.size).then_with(|| a.ext.cmp(&b.ext)));
        out
    }

    /// The `limit` largest files anywhere under `id`.
    pub fn largest_files(&self, id: u32, by_alloc: bool, limit: usize) -> Vec<LargeFile> {
        let mut best: Vec<(u64, u32)> = Vec::new();
        let mut floor = 0u64;
        // Bounded insertion: files are usually far more numerous than `limit`,
        // so most candidates are rejected by `floor` without any work.
        let mut visit = |idx: u32, n: &Node| {
            let v = if by_alloc { n.self_alloc } else { n.self_size };
            if best.len() == limit && v <= floor {
                return;
            }
            let pos = best.partition_point(|(s, _)| *s > v);
            best.insert(pos, (v, idx));
            if best.len() > limit {
                best.pop();
            }
            if best.len() == limit {
                floor = best[best.len() - 1].0;
            }
        };
        let mut stack = vec![id];
        while let Some(cur) = stack.pop() {
            let n = &self.nodes[cur as usize];
            if n.kind == Kind::Dir {
                stack.extend(n.children.iter().copied());
            } else if cur != id {
                visit(cur, n);
            }
        }

        best.into_iter()
            .map(|(size, idx)| LargeFile {
                id: idx,
                path: self.rel_path(idx),
                size,
                alloc: self.nodes[idx as usize].self_alloc,
                mtime: self.nodes[idx as usize].mtime,
            })
            .collect()
    }

    fn for_each_file<F: FnMut(&Node)>(&self, id: u32, mut f: F) {
        let mut stack = vec![id];
        while let Some(cur) = stack.pop() {
            let n = &self.nodes[cur as usize];
            if n.kind == Kind::Dir {
                stack.extend(n.children.iter().copied());
            } else {
                f(n);
            }
        }
    }
}

#[derive(Default)]
struct ExtAcc {
    size: u64,
    alloc: u64,
    count: u64,
}

#[derive(Serialize)]
pub struct Crumb {
    pub id: u32,
    pub name: String,
}

#[derive(Serialize)]
pub struct Entry {
    pub id: u32,
    pub name: String,
    pub kind: Kind,
    pub size: u64,
    pub alloc: u64,
    pub files: u32,
    pub dirs: u32,
    pub mtime: i64,
    pub read: bool,
    pub err: bool,
    pub ext: Option<String>,
}

#[derive(Default, Serialize)]
pub struct Rollup {
    pub count: u64,
    pub size: u64,
    pub alloc: u64,
}

pub struct ChildrenView {
    pub children: Vec<Entry>,
    pub other: Rollup,
}

#[derive(Serialize)]
pub struct SubtreeNode {
    pub id: u32,
    pub name: String,
    pub kind: Kind,
    pub size: u64,
    pub alloc: u64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<SubtreeNode>,
}

#[derive(Serialize)]
pub struct ExtStat {
    pub ext: String,
    pub size: u64,
    pub alloc: u64,
    pub count: u64,
}

#[derive(Serialize)]
pub struct LargeFile {
    pub id: u32,
    pub path: String,
    pub size: u64,
    pub alloc: u64,
    pub mtime: i64,
}

/// Lowercased extension of a file name, or `None` for dotfiles and names
/// without a usable suffix.
fn extension_of(name: &str) -> Option<String> {
    let (stem, ext) = name.rsplit_once('.')?;
    if stem.is_empty() || ext.is_empty() || ext.len() > 16 {
        return None;
    }
    if !ext
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return None;
    }
    Some(ext.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, kind: Kind, size: u64) -> NewEntry {
        NewEntry {
            name: name.to_string(),
            kind,
            size,
            alloc: size,
            mtime: 0,
            err: false,
        }
    }

    #[test]
    fn extensions() {
        assert_eq!(extension_of("a.TXT").as_deref(), Some("txt"));
        assert_eq!(extension_of(".bashrc"), None);
        assert_eq!(extension_of("Makefile"), None);
        assert_eq!(extension_of("x.tar.gz").as_deref(), Some("gz"));
    }

    #[test]
    fn sizes_aggregate_upwards() {
        let mut t = Tree::new("root".into(), 0, 0, 0);
        let dirs = t.add_children(
            ROOT,
            vec![entry("a", Kind::Dir, 0), entry("f.txt", Kind::File, 100)],
        );
        assert_eq!(dirs.len(), 1);
        t.add_children(dirs[0], vec![entry("b.bin", Kind::File, 900)]);

        assert_eq!(t.nodes[ROOT as usize].total_size, 1000);
        assert_eq!(t.nodes[ROOT as usize].files, 2);
        assert_eq!(t.nodes[ROOT as usize].dirs, 1);
        assert_eq!(t.nodes[dirs[0] as usize].total_size, 900);
        assert_eq!(t.rel_path(dirs[0]), "a");
    }

    #[test]
    fn children_view_rolls_up_the_tail() {
        let mut t = Tree::new("root".into(), 0, 0, 0);
        let items: Vec<NewEntry> = (0..10)
            .map(|i| entry(&format!("f{i}"), Kind::File, (i + 1) * 10))
            .collect();
        t.add_children(ROOT, items);
        let v = t.children_view(ROOT, false, 3).unwrap();
        assert_eq!(v.children.len(), 3);
        assert_eq!(v.children[0].size, 100);
        assert_eq!(v.other.count, 7);
        assert_eq!(v.other.size, 10 + 20 + 30 + 40 + 50 + 60 + 70);
    }

    #[test]
    fn largest_files_are_ranked() {
        let mut t = Tree::new("root".into(), 0, 0, 0);
        let dirs = t.add_children(ROOT, vec![entry("d", Kind::Dir, 0)]);
        t.add_children(
            dirs[0],
            vec![
                entry("small", Kind::File, 5),
                entry("big", Kind::File, 5000),
            ],
        );
        let top = t.largest_files(ROOT, false, 1);
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].path, "d/big");
    }
}

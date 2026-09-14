//! In-memory arena of the scanned filesystem tree.
//!
//! The scanner is the single writer (behind an `RwLock`), HTTP handlers are
//! readers. Sizes are aggregated upwards as soon as a directory is read, so a
//! partially scanned tree is always coherent - that is what makes streaming to
//! the browser possible.

use std::cmp::Ordering;
use std::collections::HashMap;

use serde::{Deserialize, Serialize};

pub const ROOT: u32 = 0;
/// Sentinel for "this node has no extension".
pub const NO_EXT: u32 = u32::MAX;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
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
    /// Moved to the trash from the UI; its subtree is no longer reachable.
    pub removed: bool,
    /// Filtered out as cloud-backed: its bytes live in the cloud, not here.
    pub cloud: bool,
    pub children: Vec<u32>,
}

/// How the child list is ordered.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SortKey {
    #[default]
    Size,
    Name,
    Mtime,
    Count,
}

/// A freshly discovered entry, handed over by the scanner.
pub struct NewEntry {
    pub name: String,
    pub kind: Kind,
    pub size: u64,
    pub alloc: u64,
    pub mtime: i64,
    pub err: bool,
    pub cloud: bool,
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
            removed: false,
            cloud: false,
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
    /// directories that should be walked, in the order they appeared in
    /// `entries`. Cloud placeholders are left out: they are listed, but their
    /// contents are never opened.
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
                if !e.cloud {
                    dir_ids.push(id);
                }
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
                // A filtered directory is never descended into, so calling it
                // unread would leave a permanent "scanning…" marker on it.
                read: e.cloud,
                err: e.err,
                removed: false,
                cloud: e.cloud,
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

    /// Detaches `id` after it was moved to the trash, subtracting its subtree
    /// from every ancestor so the totals stay coherent. Returns false for the
    /// root, an already removed node, or a node whose ancestor is gone.
    pub fn remove(&mut self, id: u32) -> bool {
        if id == ROOT || self.nodes[id as usize].removed {
            return false;
        }
        let parent = self.nodes[id as usize].parent;
        let mut cur = parent;
        loop {
            if self.nodes[cur as usize].removed {
                return false;
            }
            if cur == ROOT {
                break;
            }
            cur = self.nodes[cur as usize].parent;
        }

        if let Some(pos) = self.nodes[parent as usize]
            .children
            .iter()
            .position(|&c| c == id)
        {
            self.nodes[parent as usize].children.remove(pos);
        }

        let n = &self.nodes[id as usize];
        let (size, alloc, files, dirs) = if n.kind == Kind::Dir {
            (n.total_size, n.total_alloc, n.files, n.dirs + 1)
        } else {
            (n.total_size, n.total_alloc, 1, 0)
        };
        self.nodes[id as usize].removed = true;
        self.subtract(parent, size, alloc, files, dirs);

        self.stats.files = self.stats.files.saturating_sub(files as u64);
        self.stats.dirs = self.stats.dirs.saturating_sub(dirs as u64);
        self.stats.size = self.stats.size.saturating_sub(size);
        self.stats.alloc = self.stats.alloc.saturating_sub(alloc);
        self.version += 1;
        true
    }

    /// Detaches every child of a directory so its contents can be walked
    /// again. Totals and global stats shrink to just the directory itself; the
    /// node stays addressable for the UI.
    pub fn reset_children(&mut self, id: u32) -> bool {
        if self.get(id).is_none() || self.nodes[id as usize].kind != Kind::Dir {
            return false;
        }
        let children = std::mem::take(&mut self.nodes[id as usize].children);
        for child in children {
            self.remove(child);
        }
        let n = &mut self.nodes[id as usize];
        n.read = false;
        n.err = false;
        self.version += 1;
        true
    }

    /// Re-applies an entry's own stat values after a rescan. Only the node's
    /// contribution changes; its children are left alone.
    pub fn restat(
        &mut self,
        id: u32,
        size: u64,
        alloc: u64,
        mtime: i64,
        err: bool,
        cloud: bool,
    ) -> bool {
        if self.get(id).is_none() {
            return false;
        }
        let n = &mut self.nodes[id as usize];
        let (old_size, old_alloc) = (n.self_size, n.self_alloc);
        n.self_size = size;
        n.self_alloc = alloc;
        n.mtime = mtime;
        n.err = err;
        n.cloud = cloud;

        if size > old_size {
            self.propagate(id, size - old_size, 0, 0, 0);
        } else if size < old_size {
            self.subtract(id, old_size - size, 0, 0, 0);
        }
        if alloc > old_alloc {
            self.propagate(id, 0, alloc - old_alloc, 0, 0);
        } else if alloc < old_alloc {
            self.subtract(id, 0, old_alloc - alloc, 0, 0);
        }
        if size >= old_size {
            self.stats.size += size - old_size;
        } else {
            self.stats.size = self.stats.size.saturating_sub(old_size - size);
        }
        if alloc >= old_alloc {
            self.stats.alloc += alloc - old_alloc;
        } else {
            self.stats.alloc = self.stats.alloc.saturating_sub(old_alloc - alloc);
        }
        self.version += 1;
        true
    }

    /// Forgets recorded errors under `prefix` (an absolute path) so a rescan
    /// reports fresh ones instead of piling up stale counts.
    pub fn clear_errors_under(&mut self, prefix: &str) {
        let child = if prefix.ends_with(std::path::MAIN_SEPARATOR) {
            prefix.to_string()
        } else {
            format!("{prefix}{}", std::path::MAIN_SEPARATOR)
        };
        let before = self.errors.len();
        self.errors
            .retain(|e| e.path != prefix && !e.path.starts_with(&child));
        let removed = (before - self.errors.len()) as u64;
        self.stats.errors = self.stats.errors.saturating_sub(removed);
    }

    fn subtract(&mut self, from: u32, size: u64, alloc: u64, files: u32, dirs: u32) {
        let mut id = from;
        loop {
            let n = &mut self.nodes[id as usize];
            n.total_size = n.total_size.saturating_sub(size);
            n.total_alloc = n.total_alloc.saturating_sub(alloc);
            n.files = n.files.saturating_sub(files);
            n.dirs = n.dirs.saturating_sub(dirs);
            if id == ROOT {
                break;
            }
            id = n.parent;
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
        let node = self.nodes.get(id as usize)?;
        let mut cur = id;
        loop {
            let ancestor = &self.nodes[cur as usize];
            if ancestor.removed {
                return None;
            }
            if cur == ROOT {
                return Some(node);
            }
            cur = ancestor.parent;
        }
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
            cloud: n.cloud,
            ext: self.ext_name(n.ext).map(|s| s.to_string()),
        }
    }

    /// Direct children of `id`, sorted as requested, capped at `limit`.
    /// Anything past the cap is folded into `other` so the UI never has to
    /// render 50k rows.
    pub fn children_view(
        &self,
        id: u32,
        by_alloc: bool,
        sort: SortKey,
        ascending: bool,
        limit: usize,
    ) -> Option<ChildrenView> {
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
            let ord = match sort {
                SortKey::Size => key(self, *a).cmp(&key(self, *b)),
                SortKey::Name => self.nodes[*a as usize]
                    .name
                    .cmp(&self.nodes[*b as usize].name),
                SortKey::Mtime => self.nodes[*a as usize]
                    .mtime
                    .cmp(&self.nodes[*b as usize].mtime),
                SortKey::Count => (self.nodes[*a as usize].files + self.nodes[*a as usize].dirs)
                    .cmp(&(self.nodes[*b as usize].files + self.nodes[*b as usize].dirs)),
            };
            let ord = if ascending { ord } else { ord.reverse() };
            ord.then_with(|| {
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
        self.subtree_with_sizes(id, by_alloc, depth, limit, budget, None)
    }

    /// Filter the complete scope before applying the map's display limits.
    pub fn filtered_subtree(
        &self,
        id: u32,
        filter: &SearchFilter<'_>,
        by_alloc: bool,
        depth: u16,
        limit: usize,
        budget: &mut usize,
    ) -> Option<SubtreeNode> {
        self.get(id)?;
        let mut sizes: HashMap<u32, (u64, u64)> = HashMap::new();
        let mut stack = vec![(id, false)];
        while let Some((cur, visited)) = stack.pop() {
            let n = &self.nodes[cur as usize];
            if !visited && n.kind == Kind::Dir {
                stack.push((cur, true));
                stack.extend(n.children.iter().map(|&child| (child, false)));
                continue;
            }
            // The scope itself is a container, not a search result.
            let value = if cur != id && filter.matches(self, n, by_alloc) {
                (n.total_size, n.total_alloc)
            } else {
                n.children
                    .iter()
                    .filter_map(|child| sizes.get(child))
                    .fold((0, 0), |(size, alloc), &(s, a)| (size + s, alloc + a))
            };
            if value != (0, 0) || cur == id {
                sizes.insert(cur, value);
            }
        }
        self.subtree_with_sizes(id, by_alloc, depth, limit, budget, Some(&sizes))
    }

    fn subtree_with_sizes(
        &self,
        id: u32,
        by_alloc: bool,
        depth: u16,
        limit: usize,
        budget: &mut usize,
        sizes: Option<&HashMap<u32, (u64, u64)>>,
    ) -> Option<SubtreeNode> {
        let n = self.get(id)?;
        let value = |id: u32| match sizes {
            Some(sizes) => sizes.get(&id).copied().unwrap_or_default(),
            None => {
                let n = &self.nodes[id as usize];
                (n.total_size, n.total_alloc)
            }
        };
        let (size, alloc) = value(id);
        let mut out = SubtreeNode {
            id,
            name: n.name.to_string(),
            kind: n.kind,
            size,
            alloc,
            mtime: n.mtime,
            children: Vec::new(),
        };
        if n.kind != Kind::Dir || depth == 0 || *budget == 0 {
            return Some(out);
        }

        let mut ids = n.children.clone();
        let metric = |i: u32| {
            let (size, alloc) = value(i);
            if by_alloc {
                alloc
            } else {
                size
            }
        };
        ids.sort_unstable_by_key(|i| std::cmp::Reverse(metric(*i)));

        for child in ids.into_iter().take(limit) {
            if *budget == 0 {
                break;
            }
            if metric(child) == 0 {
                break;
            }
            *budget -= 1;
            if let Some(c) =
                self.subtree_with_sizes(child, by_alloc, depth - 1, limit, budget, sizes)
            {
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

    /// Entries anywhere under `id` that match `filter`, biggest first. Matching
    /// nodes are collected up to `cap` before sorting, which keeps a query that
    /// matches half the tree from growing without bound.
    pub fn search(
        &self,
        id: u32,
        filter: &SearchFilter<'_>,
        by_alloc: bool,
        limit: usize,
        cap: usize,
    ) -> Vec<SearchHit> {
        let mut hits = Vec::new();
        let mut stack: Vec<u32> = match self.get(id) {
            Some(n) => n.children.clone(),
            None => return hits,
        };
        while let Some(cur) = stack.pop() {
            let n = &self.nodes[cur as usize];
            if filter.matches(self, n, by_alloc) {
                hits.push(SearchHit {
                    id: cur,
                    parent: n.parent,
                    name: n.name.to_string(),
                    kind: n.kind,
                    path: self.rel_path(cur),
                    size: n.total_size,
                    alloc: n.total_alloc,
                    mtime: n.mtime,
                    ext: self.ext_name(n.ext).map(|s| s.to_string()),
                });
                if hits.len() >= cap {
                    break;
                }
            }
            if n.kind == Kind::Dir {
                stack.extend(n.children.iter().copied());
            }
        }

        let key = |h: &SearchHit| if by_alloc { h.alloc } else { h.size };
        hits.sort_unstable_by(|a, b| key(b).cmp(&key(a)).then_with(|| a.name.cmp(&b.name)));
        hits.truncate(limit);
        hits
    }

    /// Files in lexicographic path order, for saving and diffing snapshots.
    pub fn sorted_files(&self) -> SortedFiles<'_> {
        SortedFiles::new(self)
    }

    /// Files under `id` worth considering as duplicate candidates: real files
    /// of at least `min_size` whose bytes are actually on this disk. Paths are
    /// built during the walk rather than looked up per file.
    ///
    /// Cloud placeholders are always excluded. Reading one would make the sync
    /// filter download it, which is the opposite of what a disk usage tool
    /// should do.
    pub fn duplicate_candidates(&self, id: u32, min_size: u64) -> Vec<Candidate> {
        let mut out = Vec::new();
        if self.get(id).is_some() {
            // Seed the prefix with the scope's own path so paths stay relative
            // to the scan root no matter which folder is being scanned. The
            // duplicate hasher rebuilds absolute paths from them.
            let mut prefix = self.rel_path(id);
            self.collect_candidates(id, min_size, &mut prefix, &mut out);
        }
        out
    }

    fn collect_candidates(
        &self,
        id: u32,
        min_size: u64,
        prefix: &mut String,
        out: &mut Vec<Candidate>,
    ) {
        for &child in &self.nodes[id as usize].children {
            let n = &self.nodes[child as usize];
            let mark = prefix.len();
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(&n.name);

            match n.kind {
                Kind::Dir => self.collect_candidates(child, min_size, prefix, out),
                Kind::File if !n.cloud && !n.err && n.self_size >= min_size => {
                    out.push(Candidate {
                        id: child,
                        size: n.self_size,
                        path: prefix.clone(),
                    })
                }
                _ => {}
            }

            prefix.truncate(mark);
        }
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
    pub cloud: bool,
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

/// A file the duplicate scanner may need to read.
pub struct Candidate {
    pub id: u32,
    pub size: u64,
    /// Path relative to the scan root.
    pub path: String,
}

#[derive(Serialize)]
pub struct SubtreeNode {
    pub id: u32,
    pub name: String,
    pub kind: Kind,
    pub size: u64,
    pub alloc: u64,
    pub mtime: i64,
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

/// Filter for [`Tree::search`]. An empty query or extension list and the
/// extreme size bounds mean "no restriction".
pub struct SearchFilter<'a> {
    /// Lowercased substring the name must contain.
    pub query: &'a str,
    /// Lowercased extensions a file must have; empty means any.
    pub exts: &'a [String],
    pub min: u64,
    pub max: u64,
    /// Only entries modified at or before this unix time.
    pub max_mtime: Option<i64>,
}

impl SearchFilter<'_> {
    fn matches(&self, tree: &Tree, n: &Node, by_alloc: bool) -> bool {
        if !self.query.is_empty() && !contains_fold(&n.name, self.query) {
            return false;
        }
        if !self.exts.is_empty() {
            let ext = tree.ext_name(n.ext).unwrap_or("");
            if !self.exts.iter().any(|e| e == ext) {
                return false;
            }
        }
        let size = if by_alloc {
            n.total_alloc
        } else {
            n.total_size
        };
        if size < self.min || size > self.max {
            return false;
        }
        if let Some(t) = self.max_mtime {
            if n.mtime > t {
                return false;
            }
        }
        true
    }
}

#[derive(Serialize)]
pub struct SearchHit {
    pub id: u32,
    pub parent: u32,
    pub name: String,
    pub kind: Kind,
    pub path: String,
    pub size: u64,
    pub alloc: u64,
    pub mtime: i64,
    pub ext: Option<String>,
}

/// One file entry as seen by saving and diffing: the path relative to the scan
/// root plus the metadata a snapshot keeps.
#[derive(Clone, Copy)]
pub struct FileInfo {
    pub kind: Kind,
    pub size: u64,
    pub alloc: u64,
    pub mtime: i64,
}

/// A stream of file entries in path order. The live tree and a saved snapshot
/// both implement it so a diff can merge the two without building maps.
pub trait FileSource {
    /// Loads the next entry; false once the source is exhausted.
    fn advance(&mut self) -> std::io::Result<bool>;
    /// Path of the current entry (empty until the first `advance`).
    fn path(&self) -> &str;
    fn info(&self) -> FileInfo;
}

struct Frame {
    /// Children of the directory, sorted by name.
    children: Vec<u32>,
    next: usize,
    /// Path length to restore when this frame is popped.
    restore: usize,
}

/// Orders two children of the same directory the way their full paths sort
/// byte-wise. A directory is compared as if its name ended with `/`, since
/// that is where its children's paths continue: `a.txt` must come before the
/// contents of `a`, because `.` sorts before `/`.
fn cmp_children(tree: &Tree, a: u32, b: u32) -> Ordering {
    let na = &tree.nodes[a as usize];
    let nb = &tree.nodes[b as usize];
    na.name
        .bytes()
        .chain((na.kind == Kind::Dir).then_some(b'/'))
        .cmp(
            nb.name
                .bytes()
                .chain((nb.kind == Kind::Dir).then_some(b'/')),
        )
}

/// Streams the tree's files in byte-lexicographic path order: a depth-first
/// walk where each directory's children are ordered with the implicit `/` that
/// separates them from their contents. Saving and diffing share this order.
pub struct SortedFiles<'a> {
    tree: &'a Tree,
    stack: Vec<Frame>,
    path: String,
    /// Length the path is truncated to before the next entry, undoing the
    /// previous file name.
    file_mark: Option<usize>,
    info: FileInfo,
}

impl<'a> SortedFiles<'a> {
    pub fn new(tree: &'a Tree) -> Self {
        let root = &tree.nodes[ROOT as usize];
        let mut children = root.children.clone();
        children.sort_unstable_by(|a, b| cmp_children(tree, *a, *b));
        SortedFiles {
            tree,
            stack: vec![Frame {
                children,
                next: 0,
                restore: 0,
            }],
            path: String::new(),
            file_mark: None,
            info: FileInfo {
                kind: Kind::File,
                size: 0,
                alloc: 0,
                mtime: 0,
            },
        }
    }

    fn next_file(&mut self) -> bool {
        if let Some(mark) = self.file_mark.take() {
            self.path.truncate(mark);
        }
        let tree = self.tree;
        loop {
            let Some(frame) = self.stack.last_mut() else {
                return false;
            };
            if frame.next >= frame.children.len() {
                let frame = self.stack.pop().unwrap();
                self.path.truncate(frame.restore);
                continue;
            }
            let id = frame.children[frame.next];
            frame.next += 1;
            let node = &tree.nodes[id as usize];
            if node.kind == Kind::Dir {
                let restore = self.path.len();
                if !self.path.is_empty() {
                    self.path.push('/');
                }
                self.path.push_str(&node.name);
                let mut children = node.children.clone();
                children.sort_unstable_by(|a, b| cmp_children(tree, *a, *b));
                self.stack.push(Frame {
                    children,
                    next: 0,
                    restore,
                });
            } else {
                let mark = self.path.len();
                if !self.path.is_empty() {
                    self.path.push('/');
                }
                self.path.push_str(&node.name);
                self.file_mark = Some(mark);
                self.info = FileInfo {
                    kind: node.kind,
                    size: node.self_size,
                    alloc: node.self_alloc,
                    mtime: node.mtime,
                };
                return true;
            }
        }
    }
}

impl FileSource for SortedFiles<'_> {
    fn advance(&mut self) -> std::io::Result<bool> {
        Ok(self.next_file())
    }

    fn path(&self) -> &str {
        &self.path
    }

    fn info(&self) -> FileInfo {
        self.info
    }
}

/// ASCII case-insensitive `contains`, avoiding an allocation per entry.
fn contains_fold(haystack: &str, needle: &str) -> bool {
    let hay = haystack.as_bytes();
    let ned = needle.as_bytes();
    if ned.is_empty() {
        return true;
    }
    if ned.len() > hay.len() {
        return false;
    }
    hay.windows(ned.len())
        .any(|w| w.iter().zip(ned).all(|(a, b)| a.eq_ignore_ascii_case(b)))
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

    #[test]
    fn sorted_files_handle_equal_lengths_and_directory_prefixes() {
        let mut t = Tree::new("root".into(), 0, 0, 0);
        let dirs = t.add_children(
            ROOT,
            vec![
                entry("a", Kind::Dir, 0),
                entry("a.b", Kind::Dir, 0),
                entry("aaaaa", Kind::Dir, 0),
                entry("bb", Kind::File, 1),
                entry("z", Kind::File, 1),
                entry("zzzzz", Kind::File, 1),
            ],
        );
        for dir in dirs {
            t.add_children(dir, vec![entry("x", Kind::File, 1)]);
        }
        let mut source = t.sorted_files();
        let mut paths = Vec::new();
        while source.advance().unwrap() {
            paths.push(source.path().to_string());
        }
        assert_eq!(paths, ["a.b/x", "a/x", "aaaaa/x", "bb", "z", "zzzzz"]);
        let children = &t.nodes[ROOT as usize].children;
        for &a in children {
            for &b in children {
                for &c in children {
                    if cmp_children(&t, a, b).is_lt() && cmp_children(&t, b, c).is_lt() {
                        assert!(cmp_children(&t, a, c).is_lt());
                    }
                }
            }
        }
    }

    #[test]
    fn removed_subtrees_are_not_addressable() {
        let mut t = Tree::new("root".into(), 0, 0, 0);
        let dir = t.add_children(ROOT, vec![entry("sub", Kind::Dir, 0)])[0];
        t.add_children(dir, vec![entry("file", Kind::File, 1)]);
        let file = t.nodes[dir as usize].children[0];
        t.remove(dir);
        assert!(t.get(dir).is_none());
        assert!(t.get(file).is_none());
        assert!(t
            .children_view(dir, false, SortKey::Size, false, 10)
            .is_none());
        assert!(t.duplicate_candidates(dir, 1).is_empty());
    }

    #[test]
    fn reset_and_restat_keep_totals_coherent() {
        let mut t = Tree::new("root".into(), 0, 0, 0);
        let dir = t.add_children(ROOT, vec![entry("sub", Kind::Dir, 4)])[0];
        t.add_children(
            dir,
            vec![entry("a", Kind::File, 10), entry("b", Kind::File, 20)],
        );
        assert_eq!(t.nodes[ROOT as usize].total_size, 34);

        // Dropping the children leaves the directory's own size behind.
        assert!(t.reset_children(dir));
        assert_eq!(t.nodes[ROOT as usize].total_size, 4);
        assert_eq!(t.nodes[dir as usize].files, 0);
        assert_eq!(t.stats.files, 0);
        assert_eq!(t.stats.size, 4);
        assert_eq!(t.stats.dirs, 1);

        // Re-adding replaces the old contents without double counting.
        t.add_children(dir, vec![entry("c", Kind::File, 7)]);
        assert_eq!(t.nodes[ROOT as usize].total_size, 11);
        assert_eq!(t.stats.files, 1);

        // A restat only moves the node's own contribution.
        assert!(t.restat(dir, 6, 8, 123, false, false));
        assert_eq!(t.nodes[ROOT as usize].total_size, 13);
        assert_eq!(t.nodes[ROOT as usize].total_alloc, 15);
        assert_eq!(t.nodes[dir as usize].mtime, 123);
        assert_eq!(t.stats.size, 13);
    }

    #[test]
    fn clear_errors_under_prunes_the_subtree() {
        let mut t = Tree::new("root".into(), 0, 0, 0);
        let sep = std::path::MAIN_SEPARATOR;
        let sub = format!("root{sep}sub");
        t.record_error(format!("{sub}{sep}file"), "denied".into());
        t.record_error(sub.clone(), "denied".into());
        t.record_error(format!("root{sep}other"), "denied".into());
        t.clear_errors_under(&sub);
        assert_eq!(t.errors.len(), 1);
        assert_eq!(t.errors[0].path, format!("root{sep}other"));
        assert_eq!(t.stats.errors, 1);
    }

    #[test]
    fn map_filters_before_depth_and_child_limits() {
        let mut t = Tree::new("root".into(), 0, 0, 0);
        let mut entries = (0..65)
            .map(|i| entry(&format!("big{i}.bin"), Kind::File, 1000))
            .collect::<Vec<_>>();
        entries.push(entry("deep", Kind::Dir, 0));
        let mut dir = t.add_children(ROOT, entries)[0];
        for _ in 0..4 {
            dir = t.add_children(dir, vec![entry("nested", Kind::Dir, 0)])[0];
        }
        t.add_children(dir, vec![entry("needle.txt", Kind::File, 20)]);
        let exts = vec!["txt".into()];
        let filter = SearchFilter {
            query: "needle",
            exts: &exts,
            min: 0,
            max: u64::MAX,
            max_mtime: None,
        };
        let hits = t.search(ROOT, &filter, false, 500, 20_000);
        assert_eq!(hits.len(), 1);
        let map = t
            .filtered_subtree(ROOT, &filter, false, 3, 60, &mut 4000)
            .unwrap();
        assert_eq!(map.size, 20);
        assert_eq!(map.children.len(), 1);
        assert_eq!(map.children[0].name, "deep");
        assert_eq!(map.children[0].children[0].children[0].size, 20);
    }

    fn entry(name: &str, kind: Kind, size: u64) -> NewEntry {
        NewEntry {
            name: name.to_string(),
            kind,
            size,
            alloc: size,
            mtime: 0,
            err: false,
            cloud: false,
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
        let v = t
            .children_view(ROOT, false, SortKey::Size, false, 3)
            .unwrap();
        assert_eq!(v.children.len(), 3);
        assert_eq!(v.children[0].size, 100);
        assert_eq!(v.other.count, 7);
        assert_eq!(v.other.size, 10 + 20 + 30 + 40 + 50 + 60 + 70);
    }

    #[test]
    fn cloud_directories_are_listed_but_not_walked() {
        let mut t = Tree::new("root".into(), 0, 0, 0);
        let mut placeholder = entry("iCloud", Kind::Dir, 0);
        placeholder.cloud = true;
        let dirs = t.add_children(ROOT, vec![placeholder, entry("local", Kind::Dir, 0)]);

        // Only the real directory is handed back for walking, and the walker
        // relies on that list lining up with the paths it collected.
        assert_eq!(dirs.len(), 1);
        assert_eq!(t.nodes[dirs[0] as usize].name.as_ref(), "local");

        let view = t
            .children_view(ROOT, false, SortKey::Size, false, 10)
            .unwrap();
        let cloud = view.children.iter().find(|c| c.name == "iCloud").unwrap();
        assert!(cloud.cloud);
        // Marked read so the UI does not show it as still being scanned.
        assert!(cloud.read);
    }

    #[test]
    fn duplicate_candidates_skip_cloud_and_small_files() {
        let mut t = Tree::new("root".into(), 0, 0, 0);
        let mut remote = entry("remote.bin", Kind::File, 5_000_000);
        remote.cloud = true;

        let dirs = t.add_children(
            ROOT,
            vec![
                entry("sub", Kind::Dir, 0),
                entry("local.bin", Kind::File, 5_000_000),
                remote,
                entry("tiny.bin", Kind::File, 10),
            ],
        );
        t.add_children(dirs[0], vec![entry("deep.bin", Kind::File, 5_000_000)]);

        let mut found: Vec<String> = t
            .duplicate_candidates(ROOT, 1000)
            .into_iter()
            .map(|c| c.path)
            .collect();
        found.sort();

        // The cloud placeholder is absent: reading it would download it.
        assert_eq!(
            found,
            vec!["local.bin".to_string(), "sub/deep.bin".to_string()]
        );
    }

    #[test]
    fn duplicate_candidate_paths_are_root_relative_for_sub_scopes() {
        let mut t = Tree::new("root".into(), 0, 0, 0);
        let sub = t.add_children(ROOT, vec![entry("sub", Kind::Dir, 0)]);
        let nested = t.add_children(
            sub[0],
            vec![
                entry("nested", Kind::Dir, 0),
                entry("a.bin", Kind::File, 5000),
            ],
        );
        t.add_children(nested[0], vec![entry("deep.bin", Kind::File, 5000)]);

        // Scanning "sub/nested" must still produce paths that can be resolved
        // from the scan root, not just the scope's bare file name.
        let found: Vec<String> = t
            .duplicate_candidates(nested[0], 1)
            .into_iter()
            .map(|c| c.path)
            .collect();
        assert_eq!(found, vec!["sub/nested/deep.bin".to_string()]);
    }

    #[test]
    fn removing_a_subtree_subtracts_from_ancestors() {
        let mut t = Tree::new("root".into(), 0, 0, 0);
        let dirs = t.add_children(
            ROOT,
            vec![entry("d", Kind::Dir, 0), entry("keep.bin", Kind::File, 100)],
        );
        let d = dirs[0];
        let inner = t.add_children(
            d,
            vec![entry("a.bin", Kind::File, 900), entry("sub", Kind::Dir, 0)],
        );
        let sub = inner[0];
        t.add_children(sub, vec![entry("deep.bin", Kind::File, 5000)]);

        assert_eq!(t.nodes[ROOT as usize].total_size, 6000);
        assert_eq!(t.nodes[ROOT as usize].files, 3);
        assert_eq!(t.nodes[ROOT as usize].dirs, 2);
        assert_eq!(t.stats.size, 6000);
        assert_eq!(t.stats.files, 3);
        assert_eq!(t.stats.dirs, 2);

        assert!(t.remove(sub));
        assert_eq!(t.nodes[ROOT as usize].total_size, 1000);
        assert_eq!(t.nodes[ROOT as usize].files, 2);
        assert_eq!(t.nodes[ROOT as usize].dirs, 1);
        assert_eq!(t.nodes[d as usize].total_size, 900);
        assert_eq!(t.stats.size, 1000);
        assert_eq!(t.stats.files, 2);
        assert_eq!(t.stats.dirs, 1);

        // Removing the same entry twice, the root, or a descendant of a
        // removed directory must all be refused.
        assert!(!t.remove(sub));
        assert!(!t.remove(ROOT));
        let deep = t.nodes[sub as usize].children[0];
        assert!(!t.remove(deep));
        assert_eq!(t.stats.size, 1000);
    }

    #[test]
    fn search_matches_name_ext_size_and_age() {
        let mut t = Tree::new("root".into(), 0, 0, 0);
        let dirs = t.add_children(
            ROOT,
            vec![
                entry("photos", Kind::Dir, 0),
                entry("old.txt", Kind::File, 10),
            ],
        );
        let mut pic = entry("holiday.PNG", Kind::File, 5000);
        pic.mtime = 1000;
        let mut recent = entry("recent.png", Kind::File, 7000);
        recent.mtime = 9000;
        t.add_children(
            dirs[0],
            vec![pic, recent, entry("notes.txt", Kind::File, 10)],
        );

        let exts = vec!["png".to_string()];
        let f = SearchFilter {
            query: "holi",
            exts: &exts,
            min: 0,
            max: u64::MAX,
            max_mtime: None,
        };
        let hits = t.search(ROOT, &f, false, 10, 100);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "photos/holiday.PNG");
        assert_eq!(hits[0].parent, dirs[0]);

        let none: Vec<String> = Vec::new();
        // recent.png is large enough but too new, so only the old directory
        // qualifies: directories match on their aggregate size and mtime.
        let f = SearchFilter {
            query: "",
            exts: &none,
            min: 6000,
            max: u64::MAX,
            max_mtime: Some(500),
        };
        let hits = t.search(ROOT, &f, false, 10, 100);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "photos");

        let f = SearchFilter {
            query: "",
            exts: &none,
            min: 6000,
            max: u64::MAX,
            max_mtime: Some(9500),
        };
        let hits = t.search(ROOT, &f, false, 10, 100);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].name, "photos");
        assert_eq!(hits[1].name, "recent.png");

        // An extension filter never matches directories, even by name.
        let f = SearchFilter {
            query: "photos",
            exts: &exts,
            min: 0,
            max: u64::MAX,
            max_mtime: None,
        };
        assert!(t.search(ROOT, &f, false, 10, 100).is_empty());
    }

    #[test]
    fn sorted_files_follow_byte_order() {
        let mut t = Tree::new("root".into(), 0, 0, 0);
        let dirs = t.add_children(
            ROOT,
            vec![
                entry("a", Kind::Dir, 0),
                entry("a.txt", Kind::File, 10),
                entry("a0", Kind::File, 20),
            ],
        );
        let inner = t.add_children(
            dirs[0],
            vec![entry("b", Kind::Dir, 0), entry("b.txt", Kind::File, 30)],
        );
        t.add_children(inner[0], vec![entry("c.txt", Kind::File, 40)]);

        let mut files = t.sorted_files();
        let mut paths = Vec::new();
        while files.advance().unwrap() {
            paths.push(files.path().to_string());
        }
        // `.` sorts before `/`, so a file next to a directory comes first; the
        // directory's own contents continue after the separator.
        assert_eq!(paths, vec!["a.txt", "a/b.txt", "a/b/c.txt", "a0"]);
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

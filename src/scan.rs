//! Parallel filesystem walker.
//!
//! Directories are read on a rayon pool; each finished `read_dir` takes the
//! tree write lock just long enough to insert its children. Every insert
//! aggregates sizes up to the root, so the browser can render a coherent view
//! of a scan that is still running.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use globset::GlobSet;

use crate::fsext;
use crate::tree::{Kind, NewEntry, Tree, ROOT};

pub struct ScanOpts {
    pub root: PathBuf,
    pub one_file_system: bool,
    pub dereference: bool,
    pub count_links: bool,
    pub max_depth: Option<u16>,
    pub exclude: Option<GlobSet>,
    pub threads: Option<usize>,
    pub local_only: bool,
}

pub struct Scanner {
    pub tree: Arc<RwLock<Tree>>,
    opts: ScanOpts,
    cancel: AtomicBool,
    done: AtomicBool,
    /// Set while a subtree rescan is running; the initial walk uses `done`.
    busy: AtomicBool,
    started: Instant,
    /// Start of the current rescan, so elapsed time keeps moving.
    rescan_start: Mutex<Option<Instant>>,
    /// Wall time of finished walks; only meaningful once `done` is set.
    final_ms: AtomicU64,
    seen_links: Arc<Mutex<HashSet<(u64, u64)>>>,
    root_device: u64,
}

impl Scanner {
    /// Stat the root and build a tree containing just that node.
    pub fn new(opts: ScanOpts) -> std::io::Result<Arc<Self>> {
        fsext::init(&opts.root);
        let md = fs::metadata(&opts.root)?;
        if !md.is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "path is not a directory",
            ));
        }
        let (size, alloc) = fsext::sizes(&md);
        let name = display_root(&opts.root);
        let tree = Tree::new(name, size, alloc, fsext::mtime(&md));
        Ok(Arc::new(Scanner {
            tree: Arc::new(RwLock::new(tree)),
            root_device: fsext::device(&md),
            opts,
            cancel: AtomicBool::new(false),
            done: AtomicBool::new(false),
            busy: AtomicBool::new(false),
            started: Instant::now(),
            rescan_start: Mutex::new(None),
            final_ms: AtomicU64::new(0),
            seen_links: Arc::new(Mutex::new(HashSet::new())),
        }))
    }

    pub fn is_done(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }

    /// True from construction until the initial walk finishes, and again while
    /// a subtree rescan is running.
    pub fn is_busy(&self) -> bool {
        !self.is_done() || self.busy.load(Ordering::Acquire)
    }

    /// Time spent scanning; stops advancing once every walk is over.
    pub fn elapsed_ms(&self) -> u64 {
        if let Some(start) = *self.rescan_start.lock().unwrap() {
            return self.final_ms.load(Ordering::Relaxed) + start.elapsed().as_millis() as u64;
        }
        if self.is_done() {
            self.final_ms.load(Ordering::Relaxed)
        } else {
            self.started.elapsed().as_millis() as u64
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Re-walks one node in the background: a directory gets its children
    /// replaced, a file is merely re-stat'ed. Returns false when another walk
    /// owns the scanner. `done` runs on the rescan thread once the tree has
    /// been updated and before the scanner is marked idle again.
    pub fn rescan(
        self: &Arc<Self>,
        id: u32,
        path: PathBuf,
        done: impl FnOnce() + Send + 'static,
    ) -> bool {
        if !self.is_done() || self.busy.swap(true, Ordering::AcqRel) {
            return false;
        }
        self.cancel.store(false, Ordering::Relaxed);
        *self.rescan_start.lock().unwrap() = Some(Instant::now());

        let me = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name("duw-rescan".into())
            .spawn(move || {
                me.rescan_walk(id, &path);
                me.finish_rescan(done);
            });
        if spawned.is_err() {
            self.finish_rescan(|| {});
            return false;
        }
        true
    }

    fn finish_rescan(&self, done: impl FnOnce()) {
        {
            let mut t = self.tree.write().unwrap();
            t.current.clear();
            t.version += 1;
        }
        if let Some(start) = self.rescan_start.lock().unwrap().take() {
            self.final_ms
                .fetch_add(start.elapsed().as_millis() as u64, Ordering::Relaxed);
        }
        done();
        self.busy.store(false, Ordering::Release);
    }

    /// The synchronous half of `rescan`: refresh the node's own stat and, for
    /// a directory, walk its contents again.
    fn rescan_walk(self: &Arc<Self>, id: u32, path: &Path) {
        let Some((kind, depth)) = self.tree.read().unwrap().get(id).map(|n| (n.kind, n.depth))
        else {
            return;
        };
        if let Some(prefix) = path.to_str() {
            self.tree.write().unwrap().clear_errors_under(prefix);
        }

        let mut filtered = false;
        let stat = if self.opts.dereference {
            fs::metadata(path)
        } else {
            fs::symlink_metadata(path)
        };
        match stat {
            Ok(md) => {
                let cloud = if kind == Kind::Dir {
                    self.opts.local_only && fsext::is_cloud_backed(&md, path)
                } else {
                    fsext::is_cloud_backed(&md, path)
                };
                filtered = cloud && self.opts.local_only;
                let (size, alloc) = if filtered { (0, 0) } else { fsext::sizes(&md) };
                self.tree
                    .write()
                    .unwrap()
                    .restat(id, size, alloc, fsext::mtime(&md), false, cloud);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.tree.write().unwrap().remove(id);
                return;
            }
            // The walk below records what actually went wrong.
            Err(_) => {}
        }

        if kind != Kind::Dir {
            return;
        }
        {
            let mut t = self.tree.write().unwrap();
            t.reset_children(id);
            if filtered {
                // Never descended into, so it must not keep a "scanning" mark.
                t.mark_read(id, false);
                return;
            }
        }

        let mut builder = rayon::ThreadPoolBuilder::new();
        if let Some(n) = self.opts.threads {
            builder = builder.num_threads(n.max(1));
        }
        let pool = match builder.build() {
            Ok(p) => p,
            Err(_) => return,
        };
        // Hard links are deduped within the rescanned subtree only, so a link
        // that crosses its boundary can be counted twice until the next full
        // scan. Tracking links per node would cost more than it is worth here.
        let links = Arc::new(Mutex::new(HashSet::new()));
        let path = path.to_path_buf();
        let me = Arc::clone(self);
        pool.scope(move |s| {
            let inner = Arc::clone(&me);
            s.spawn(move |s| inner.walk(id, path, depth, Vec::new(), links, s));
        });
    }

    /// Blocks until the whole tree has been walked. Meant to be called from a
    /// dedicated thread.
    pub fn run(self: &Arc<Self>) {
        let mut builder = rayon::ThreadPoolBuilder::new();
        if let Some(n) = self.opts.threads {
            builder = builder.num_threads(n.max(1));
        }
        let pool = match builder.build() {
            Ok(p) => p,
            Err(_) => {
                self.finish();
                return;
            }
        };

        let root_path = self.opts.root.clone();
        let me = Arc::clone(self);
        pool.scope(move |s| {
            let inner = Arc::clone(&me);
            let links = Arc::clone(&me.seen_links);
            s.spawn(move |s| inner.walk(ROOT, root_path, 0, Vec::new(), links, s));
        });

        self.finish();
        let mut t = self.tree.write().unwrap();
        t.current.clear();
        t.version += 1;
    }

    fn finish(&self) {
        self.final_ms
            .store(self.started.elapsed().as_millis() as u64, Ordering::Relaxed);
        self.done.store(true, Ordering::Release);
    }

    fn walk(
        self: Arc<Self>,
        id: u32,
        path: PathBuf,
        depth: u16,
        mut ancestors: Vec<PathBuf>,
        links: Arc<Mutex<HashSet<(u64, u64)>>>,
        scope: &rayon::Scope<'_>,
    ) {
        if self.is_cancelled() {
            return;
        }

        if self.opts.dereference {
            let resolved = match fs::canonicalize(&path) {
                Ok(resolved) => resolved,
                Err(e) => {
                    let mut t = self.tree.write().unwrap();
                    t.mark_read(id, true);
                    t.record_error(path.display().to_string(), e.to_string());
                    return;
                }
            };
            if ancestors.contains(&resolved) {
                let mut t = self.tree.write().unwrap();
                t.mark_read(id, true);
                t.stats.skipped += 1;
                t.record_error(path.display().to_string(), "symbolic link cycle".into());
                return;
            }
            ancestors.push(resolved);
        }

        let rd = match fs::read_dir(&path) {
            Ok(rd) => rd,
            Err(e) => {
                let mut t = self.tree.write().unwrap();
                t.mark_read(id, true);
                t.record_error(path.display().to_string(), e.to_string());
                return;
            }
        };

        let mut entries: Vec<NewEntry> = Vec::new();
        let mut subdirs: Vec<PathBuf> = Vec::new();
        let mut errors: Vec<(String, String)> = Vec::new();
        let mut skipped = 0u64;
        let mut hardlinks = 0u64;
        let recurse = self.opts.max_depth.is_none_or(|max| depth < max);

        for de in rd {
            if self.is_cancelled() {
                break;
            }
            let de = match de {
                Ok(de) => de,
                Err(e) => {
                    errors.push((path.display().to_string(), e.to_string()));
                    continue;
                }
            };
            let child = de.path();
            let name = de.file_name().to_string_lossy().into_owned();

            if self.excluded(&name, &child) {
                skipped += 1;
                continue;
            }

            // `DirEntry::metadata` does not follow symlinks and is free on
            // Windows, where std already cached the FindFirstFile results.
            let md = if self.opts.dereference {
                fs::metadata(&child).or_else(|_| de.metadata())
            } else {
                de.metadata()
            };
            let md = match md {
                Ok(md) => md,
                Err(e) => {
                    errors.push((child.display().to_string(), e.to_string()));
                    entries.push(NewEntry {
                        name,
                        kind: Kind::Other,
                        size: 0,
                        alloc: 0,
                        mtime: 0,
                        err: true,
                        cloud: false,
                    });
                    continue;
                }
            };

            let ft = md.file_type();
            let kind = if ft.is_dir() {
                Kind::Dir
            } else if ft.is_symlink() {
                Kind::Link
            } else if ft.is_file() {
                Kind::File
            } else {
                Kind::Other
            };

            // Cloud placeholders report their full size but occupy nothing
            // here. Files are always classified, whether or not the filter is
            // on, because the duplicate scanner must never read one: that
            // would make the sync filter download it. Directories only need
            // the check when the filter is on, and it costs a syscall.
            let cloud = if kind == Kind::Dir {
                self.opts.local_only && fsext::is_cloud_backed(&md, &child)
            } else {
                fsext::is_cloud_backed(&md, &child)
            };
            let filtered = cloud && self.opts.local_only;

            if kind == Kind::Dir {
                if self.opts.one_file_system && fsext::device(&md) != self.root_device {
                    skipped += 1;
                    continue;
                }
                if recurse && !filtered {
                    subdirs.push(child);
                }
            }

            if filtered {
                entries.push(NewEntry {
                    name,
                    kind,
                    size: 0,
                    alloc: 0,
                    mtime: fsext::mtime(&md),
                    err: false,
                    cloud: true,
                });
                continue;
            }

            let (mut size, mut alloc) = fsext::sizes(&md);
            if kind == Kind::File && !self.opts.count_links {
                if let Some(key) = fsext::hardlink_key(&md) {
                    if !links.lock().unwrap().insert(key) {
                        // Already counted through another link.
                        size = 0;
                        alloc = 0;
                        hardlinks += 1;
                    }
                }
            }

            entries.push(NewEntry {
                name,
                kind,
                size,
                alloc,
                mtime: fsext::mtime(&md),
                err: false,
                cloud,
            });
        }

        let dir_ids = {
            let mut t = self.tree.write().unwrap();
            let ids = t.add_children(id, entries);
            t.stats.skipped += skipped;
            t.stats.hardlinks += hardlinks;
            for (p, m) in errors {
                t.record_error(p, m);
            }
            t.current = path.display().to_string();
            ids
        };

        if !recurse {
            // These directories sit at the depth limit and will never be
            // walked, so mark them read: otherwise the UI shows a permanent
            // "scanning…" marker on them.
            let mut t = self.tree.write().unwrap();
            for id in &dir_ids {
                t.mark_read(*id, false);
            }
            return;
        }
        if self.is_cancelled() {
            return;
        }
        debug_assert_eq!(dir_ids.len(), subdirs.len());
        for (child_id, child_path) in dir_ids.into_iter().zip(subdirs) {
            let me = Arc::clone(&self);
            let links = Arc::clone(&links);
            let ancestors = ancestors.clone();
            scope.spawn(move |s| me.walk(child_id, child_path, depth + 1, ancestors, links, s));
        }
    }

    fn excluded(&self, name: &str, path: &Path) -> bool {
        match &self.opts.exclude {
            Some(set) => set.is_match(name) || set.is_match(path),
            None => false,
        }
    }
}

/// A readable label for the root node: the last path component, falling back to
/// the whole path for drive roots such as `C:\`.
fn display_root(path: &Path) -> String {
    match path.file_name() {
        Some(n) => n.to_string_lossy().into_owned(),
        None => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn dereference_stops_at_ancestor_cycle() {
        let root = std::env::temp_dir().join(format!("duw-cycle-test-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("file"), b"hello").unwrap();
        std::os::unix::fs::symlink(&root, root.join("back")).unwrap();
        let scanner = Scanner::new(ScanOpts {
            root: root.clone(),
            one_file_system: false,
            dereference: true,
            count_links: false,
            max_depth: None,
            exclude: None,
            threads: Some(1),
            local_only: false,
        })
        .unwrap();
        scanner.run();
        let t = scanner.tree.read().unwrap();
        assert_eq!(t.stats.files, 1);
        assert_eq!(t.stats.skipped, 1);
        assert!(t.errors.iter().any(|e| e.message == "symbolic link cycle"));
        fs::remove_file(root.join("back")).unwrap();
        fs::remove_file(root.join("file")).unwrap();
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn rescan_replaces_a_subtrees_contents() {
        let root = std::env::temp_dir().join(format!("duw-rescan-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let sub = root.join("sub");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("old.txt"), b"old").unwrap();

        let scanner = Scanner::new(ScanOpts {
            root: root.clone(),
            one_file_system: false,
            dereference: false,
            count_links: false,
            max_depth: None,
            exclude: None,
            threads: Some(1),
            local_only: false,
        })
        .unwrap();
        scanner.run();

        let sub_id = scanner.tree.read().unwrap().nodes[ROOT as usize].children[0];
        let before = scanner.tree.read().unwrap().stats.size;

        fs::write(sub.join("new.txt"), b"new!").unwrap();
        assert!(scanner.rescan(sub_id, sub.clone(), || {}));
        while scanner.is_busy() {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        {
            let t = scanner.tree.read().unwrap();
            assert_eq!(t.stats.files, 2);
            assert_eq!(t.stats.size, before + 4);
            let names: Vec<&str> = t.nodes[sub_id as usize]
                .children
                .iter()
                .map(|&c| t.nodes[c as usize].name.as_ref())
                .collect();
            assert!(names.contains(&"old.txt") && names.contains(&"new.txt"));
        }

        fs::remove_file(sub.join("old.txt")).unwrap();
        assert!(scanner.rescan(sub_id, sub.clone(), || {}));
        while scanner.is_busy() {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        {
            let t = scanner.tree.read().unwrap();
            assert_eq!(t.stats.files, 1);
            assert_eq!(t.stats.size, before + 1);
        }

        fs::remove_dir_all(&root).unwrap();
    }
}

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
    started: Instant,
    /// Wall time of the finished scan; only meaningful once `done` is set.
    final_ms: AtomicU64,
    seen_links: Mutex<HashSet<(u64, u64)>>,
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
            started: Instant::now(),
            final_ms: AtomicU64::new(0),
            seen_links: Mutex::new(HashSet::new()),
        }))
    }

    pub fn is_done(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }

    /// Time spent scanning; stops advancing once the walk is over.
    pub fn elapsed_ms(&self) -> u64 {
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
            s.spawn(move |s| inner.walk(ROOT, root_path, 0, s));
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

    fn walk(self: Arc<Self>, id: u32, path: PathBuf, depth: u16, scope: &rayon::Scope<'_>) {
        if self.is_cancelled() {
            return;
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
            // here. Files are always classified, whether or not --local-only
            // was given, because the duplicate scanner must never read one:
            // that would make the sync filter download it. Directories only
            // need the check when the filter is on, and it costs a syscall.
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
                    if !self.seen_links.lock().unwrap().insert(key) {
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

        if !recurse || self.is_cancelled() {
            return;
        }
        debug_assert_eq!(dir_ids.len(), subdirs.len());
        for (child_id, child_path) in dir_ids.into_iter().zip(subdirs) {
            let me = Arc::clone(&self);
            scope.spawn(move |s| me.walk(child_id, child_path, depth + 1, s));
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

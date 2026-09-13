//! Duplicate detection.
//!
//! Three stages, each one only looking at what survived the previous:
//!
//! 1. refresh metadata and group by current size,
//! 2. hash a small window at each end of the file, which separates files that
//!    merely share a header,
//! 3. hash the whole file.
//!
//! In a typical tree the first stage decides almost everything, so the number
//! of files actually read is a small fraction of the number scanned. Digests
//! are recomputed on each run so a rescan also observes changed file contents.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use rayon::prelude::*;
use serde::Serialize;

use crate::fsext;
use crate::tree::{Candidate, Tree};

/// Bytes read from each end of a file during the cheap stage.
const WINDOW: u64 = 16 * 1024;
/// At or below this size the window stage is pointless, so hash the whole file.
const SMALL_FILE: u64 = 64 * 1024;
const READ_BUF: usize = 256 * 1024;

pub type Digest = [u8; 32];

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    /// Never run for the current scope.
    Idle,
    Grouping,
    Windowing,
    Hashing,
    Done,
    Cancelled,
}

#[derive(Clone, Serialize)]
pub struct DupeProgress {
    pub phase: Phase,
    pub scope: u32,
    pub min_size: u64,
    /// Files that survived the size grouping.
    pub candidates: u64,
    pub read: u64,
    pub bytes_read: u64,
    pub bytes_total: u64,
    pub groups: u64,
    /// Bytes that would come back if every group kept one copy.
    pub wasted: u64,
    pub elapsed_ms: u64,
    /// Bumped on every new run so the UI can tell results apart.
    pub generation: u64,
}

impl Default for DupeProgress {
    fn default() -> Self {
        DupeProgress {
            phase: Phase::Idle,
            scope: 0,
            min_size: 0,
            candidates: 0,
            read: 0,
            bytes_read: 0,
            bytes_total: 0,
            groups: 0,
            wasted: 0,
            elapsed_ms: 0,
            generation: 0,
        }
    }
}

#[derive(Clone, Serialize)]
pub struct DupeFile {
    pub id: u32,
    pub path: String,
}

#[derive(Clone, Serialize)]
pub struct DupeGroup {
    pub size: u64,
    /// `(copies - 1) * size`, what deleting all but one would free.
    pub wasted: u64,
    pub files: Vec<DupeFile>,
}

pub struct Dupes {
    tree: Arc<RwLock<Tree>>,
    root: PathBuf,
    progress: Mutex<DupeProgress>,
    groups: RwLock<Vec<DupeGroup>>,
    /// Incremented per run; a worker whose generation is stale gives up.
    generation: AtomicU64,
}

impl Dupes {
    pub fn new(tree: Arc<RwLock<Tree>>, root: PathBuf) -> Arc<Self> {
        Arc::new(Dupes {
            tree,
            root,
            progress: Mutex::new(DupeProgress::default()),
            groups: RwLock::new(Vec::new()),
            generation: AtomicU64::new(0),
        })
    }

    pub fn progress(&self) -> DupeProgress {
        self.progress.lock().unwrap().clone()
    }

    pub fn groups(&self) -> Vec<DupeGroup> {
        self.groups.read().unwrap().clone()
    }

    /// Starts a run in the background, replacing whatever was running. Returns
    /// the generation number of the new run.
    pub fn start(self: &Arc<Self>, scope: u32, min_size: u64) -> u64 {
        let generation = {
            let mut p = self.progress.lock().unwrap();
            let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
            *p = DupeProgress {
                phase: Phase::Grouping,
                scope,
                min_size,
                generation,
                ..DupeProgress::default()
            };
            self.groups.write().unwrap().clear();
            generation
        };

        let me = Arc::clone(self);
        // The work is blocking file IO, so it never touches the async runtime.
        if std::thread::Builder::new()
            .name("duw-dupes".into())
            .spawn(move || me.run(scope, min_size, generation))
            .is_err()
        {
            self.update(generation, |p| p.phase = Phase::Cancelled);
        }

        generation
    }

    pub fn cancel(&self) {
        // Moving the generation on is enough: the worker checks it constantly.
        let mut p = self.progress.lock().unwrap();
        p.generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        if p.phase != Phase::Done {
            p.phase = Phase::Cancelled;
        }
    }

    /// Drops removed files, including descendants of trashed directories,
    /// and publishes a new revision so the UI refetches its result list.
    pub fn forget_removed(&self) {
        let tree = self.tree.read().unwrap();
        let mut p = self.progress.lock().unwrap();
        p.generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let mut groups = self.groups.write().unwrap();
        groups.retain_mut(|g| {
            g.files.retain(|f| tree.get(f.id).is_some());
            g.wasted = g.size * g.files.len().saturating_sub(1) as u64;
            g.files.len() > 1
        });
        let total_wasted: u64 = groups.iter().map(|g| g.wasted).sum();
        let group_count = groups.len() as u64;
        drop(groups);

        p.groups = group_count;
        p.wasted = total_wasted;
        match p.phase {
            // The inputs changed under the running scan, so it cannot finish
            // coherently. Marking it cancelled also stops the worker, whose
            // generation no longer matches.
            Phase::Grouping | Phase::Windowing | Phase::Hashing => {
                p.phase = Phase::Cancelled;
            }
            // Force the UI to refetch the now smaller result list.
            Phase::Done | Phase::Cancelled | Phase::Idle => {}
        }
    }

    fn stale(&self, generation: u64) -> bool {
        self.generation.load(Ordering::SeqCst) != generation
    }

    fn run(self: Arc<Self>, scope: u32, min_size: u64, generation: u64) {
        let started = Instant::now();
        let outcome = self.work(scope, min_size, generation, started);

        let mut p = self.progress.lock().unwrap();
        if p.generation != generation {
            return;
        }
        p.phase = if outcome {
            Phase::Done
        } else {
            Phase::Cancelled
        };
        p.elapsed_ms = started.elapsed().as_millis() as u64;
    }

    /// Returns false when the run was superseded or cancelled.
    fn work(&self, scope: u32, min_size: u64, generation: u64, started: Instant) -> bool {
        // Refresh sizes before grouping: the filesystem can change after the walk.
        let candidates = {
            let t = self.tree.read().unwrap();
            t.duplicate_candidates(scope, 1)
        };
        if self.stale(generation) {
            return false;
        }

        let mut by_size: HashMap<u64, Vec<Candidate>> = HashMap::new();
        for mut c in candidates {
            if self.stale(generation) {
                return false;
            }
            let path = self.path_of(&c);
            let Ok(md) = std::fs::metadata(&path) else {
                continue;
            };
            if !md.is_file() || fsext::is_cloud_backed(&md, &path) || md.len() < min_size.max(1) {
                continue;
            }
            c.size = md.len();
            by_size.entry(c.size).or_default().push(c);
        }
        by_size.retain(|_, group| group.len() > 1);

        let candidate_count: u64 = by_size.values().map(|g| g.len() as u64).sum();
        let bytes_total: u64 = by_size.iter().map(|(size, g)| size * g.len() as u64).sum();
        self.update(generation, |p| {
            p.candidates = candidate_count;
            p.bytes_total = bytes_total;
            p.phase = Phase::Windowing;
        });

        // Stage 2: split each size group by the digest of its first and last
        // window. Files small enough that the windows would cover them are
        // passed straight through to stage 3.
        let mut refined: Vec<Vec<Candidate>> = Vec::new();
        for (size, group) in by_size {
            if self.stale(generation) {
                return false;
            }
            if size <= SMALL_FILE {
                refined.push(group);
                continue;
            }
            for bucket in self.split_by(group, generation, |this, c| {
                this.window_digest(c, generation)
            }) {
                refined.push(bucket);
            }
        }
        if self.stale(generation) {
            return false;
        }

        self.update(generation, |p| p.phase = Phase::Hashing);

        // Stage 3: full digests decide the real groups.
        let mut groups: Vec<DupeGroup> = Vec::new();
        for group in refined {
            if self.stale(generation) {
                return false;
            }
            let size = group[0].size;
            for bucket in
                self.split_by(group, generation, |this, c| this.full_digest(c, generation))
            {
                let wasted = size * (bucket.len() as u64 - 1);
                groups.push(DupeGroup {
                    size,
                    wasted,
                    files: bucket
                        .into_iter()
                        .map(|c| DupeFile {
                            id: c.id,
                            path: c.path,
                        })
                        .collect(),
                });
            }
        }
        if self.stale(generation) {
            return false;
        }

        groups.sort_unstable_by(|a, b| b.wasted.cmp(&a.wasted).then(b.size.cmp(&a.size)));
        let total_wasted: u64 = groups.iter().map(|g| g.wasted).sum();
        let group_count = groups.len() as u64;

        {
            // Checking the generation while holding the same lock `start`
            // uses to clear the results keeps a superseded run from overwriting
            // the new one's (empty) list. `start` bumps the generation before
            // it clears, so a worker that gets the lock afterwards sees a
            // mismatched generation and gives up.
            let mut g = self.groups.write().unwrap();
            if self.stale(generation) {
                return false;
            }
            *g = groups;
        }
        self.update(generation, |p| {
            p.groups = group_count;
            p.wasted = total_wasted;
            p.elapsed_ms = started.elapsed().as_millis() as u64;
        });
        true
    }

    /// Hashes a group in parallel and returns the sub-groups that still have
    /// more than one member. Files that cannot be read drop out silently:
    /// a file we cannot open is not a duplicate we can report on.
    fn split_by<F>(&self, group: Vec<Candidate>, generation: u64, digest: F) -> Vec<Vec<Candidate>>
    where
        F: Fn(&Self, &Candidate) -> Option<Digest> + Sync,
    {
        let digested: Vec<(Digest, Candidate)> = group
            .into_par_iter()
            .filter_map(|c| {
                if self.stale(generation) {
                    return None;
                }
                digest(self, &c).map(|d| (d, c))
            })
            .collect();

        let mut buckets: HashMap<Digest, Vec<Candidate>> = HashMap::new();
        for (d, c) in digested {
            buckets.entry(d).or_default().push(c);
        }
        buckets.into_values().filter(|b| b.len() > 1).collect()
    }

    fn path_of(&self, c: &Candidate) -> PathBuf {
        let mut p = self.root.clone();
        for part in c.path.split('/') {
            p.push(part);
        }
        p
    }

    fn window_digest(&self, c: &Candidate, generation: u64) -> Option<Digest> {
        let d = hash_windows(&self.path_of(c), c.size)?;
        self.record_read(generation, WINDOW * 2);
        Some(d)
    }

    fn full_digest(&self, c: &Candidate, generation: u64) -> Option<Digest> {
        let d = hash_file(&self.path_of(c), c.size, || self.stale(generation))?;
        self.record_read(generation, c.size);
        Some(d)
    }

    fn record_read(&self, generation: u64, bytes: u64) {
        let mut p = self.progress.lock().unwrap();
        // A superseded run must not pollute the counters of the one that
        // replaced it.
        if p.generation == generation {
            p.read += 1;
            p.bytes_read += bytes;
        }
    }

    fn update<F: FnOnce(&mut DupeProgress)>(&self, generation: u64, f: F) {
        let mut p = self.progress.lock().unwrap();
        if p.generation == generation {
            f(&mut p);
        }
    }
}

/// Digest of the first and last `WINDOW` bytes, with the size mixed in so that
/// a short file cannot collide with the head of a longer one.
fn hash_windows(path: &Path, size: u64) -> Option<Digest> {
    let before = std::fs::metadata(path).ok()?;
    if before.len() != size || fsext::is_cloud_backed(&before, path) {
        return None;
    }
    let mut file = File::open(path).ok()?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(&size.to_le_bytes());

    let mut buf = vec![0u8; WINDOW as usize];
    read_exact_at(&mut file, 0, &mut buf)?;
    hasher.update(&buf);

    read_exact_at(&mut file, size - WINDOW, &mut buf)?;
    hasher.update(&buf);

    unchanged(&before, &file.metadata().ok()?).then(|| *hasher.finalize().as_bytes())
}

fn read_exact_at(file: &mut File, offset: u64, buf: &mut [u8]) -> Option<()> {
    file.seek(SeekFrom::Start(offset)).ok()?;
    file.read_exact(buf).ok()
}

fn hash_file(path: &Path, size: u64, cancelled: impl Fn() -> bool) -> Option<Digest> {
    let before = std::fs::metadata(path).ok()?;
    if before.len() != size || fsext::is_cloud_backed(&before, path) {
        return None;
    }
    let mut file = File::open(path).ok()?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; READ_BUF];
    loop {
        if cancelled() {
            return None;
        }
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => hasher.update(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        };
    }
    unchanged(&before, &file.metadata().ok()?).then(|| *hasher.finalize().as_bytes())
}

fn unchanged(before: &std::fs::Metadata, after: &std::fs::Metadata) -> bool {
    before.len() == after.len() && before.modified().ok() == after.modified().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::{Kind, NewEntry, ROOT};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Fixture {
        root: PathBuf,
        dupes: Arc<Dupes>,
        dir: u32,
    }

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "duw-dupes-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&root).unwrap();
            std::fs::create_dir(root.join("sub")).unwrap();
            let mut tree = Tree::new("root".into(), 0, 0, 0);
            let entry = |name: &str, kind, size| NewEntry {
                name: name.into(),
                kind,
                size,
                alloc: size,
                mtime: 0,
                err: false,
                cloud: false,
            };
            let dir = tree.add_children(ROOT, vec![entry("sub", Kind::Dir, 0)])[0];
            tree.add_children(
                dir,
                vec![entry("a", Kind::File, 4), entry("b", Kind::File, 4)],
            );
            for name in ["a", "b"] {
                std::fs::write(root.join("sub").join(name), b"aaaa").unwrap();
            }
            let dupes = Dupes::new(Arc::new(RwLock::new(tree)), root.clone());
            Self { root, dupes, dir }
        }
        fn run(&self, min: u64) {
            let generation = self.dupes.progress().generation;
            assert!(self.dupes.work(ROOT, min, generation, Instant::now()));
            self.dupes.progress.lock().unwrap().phase = Phase::Done;
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            for name in ["a", "b"] {
                let _ = std::fs::remove_file(self.root.join("sub").join(name));
            }
            let _ = std::fs::remove_dir(self.root.join("sub"));
            let _ = std::fs::remove_dir(&self.root);
        }
    }

    #[test]
    fn rescan_observes_content_changes_and_new_sizes() {
        let f = Fixture::new();
        f.run(1);
        assert_eq!(f.dupes.groups().len(), 1);
        std::fs::write(f.root.join("sub/b"), b"bbbb").unwrap();
        f.run(1);
        assert!(f.dupes.groups().is_empty());
        for name in ["a", "b"] {
            std::fs::write(f.root.join("sub").join(name), b"larger").unwrap();
        }
        f.run(6);
        assert_eq!(f.dupes.groups()[0].size, 6);
    }

    #[test]
    fn removing_folder_clears_descendants_and_publishes_revision() {
        let f = Fixture::new();
        f.run(1);
        let generation = f.dupes.progress().generation;
        f.dupes.tree.write().unwrap().remove(f.dir);
        f.dupes.forget_removed();
        assert!(f.dupes.groups().is_empty());
        let p = f.dupes.progress();
        assert_eq!(p.wasted, 0);
        assert_eq!(p.groups, 0);
        assert!(p.generation > generation);
    }

    #[test]
    fn cancelling_invalidates_worker_updates_and_hash_reads() {
        let f = Fixture::new();
        f.dupes.cancel();
        f.dupes.update(0, |p| p.phase = Phase::Done);
        assert_eq!(f.dupes.progress().phase, Phase::Cancelled);
        assert!(hash_file(&f.root.join("sub/a"), 4, || true).is_none());
    }
}

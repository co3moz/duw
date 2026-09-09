//! Duplicate detection.
//!
//! Three stages, each one only looking at what survived the previous:
//!
//! 1. group by size, which is free because the tree already knows every size,
//! 2. hash a small window at each end of the file, which separates files that
//!    merely share a header,
//! 3. hash the whole file.
//!
//! In a typical tree the first stage decides almost everything, so the number
//! of files actually read is a small fraction of the number scanned. Digests
//! are cached per node, so lowering the size threshold from the UI only reads
//! the files that were not candidates before.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use rayon::prelude::*;
use serde::Serialize;

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
    /// Full-file digests, keyed by node id. Valid for the life of the process:
    /// the tree it refers to is a snapshot too.
    full: Mutex<HashMap<u32, Digest>>,
    /// Digests of the head and tail windows, same lifetime.
    window: Mutex<HashMap<u32, Digest>>,
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
            full: Mutex::new(HashMap::new()),
            window: Mutex::new(HashMap::new()),
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
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;

        {
            let mut p = self.progress.lock().unwrap();
            *p = DupeProgress {
                phase: Phase::Grouping,
                scope,
                min_size,
                generation,
                ..DupeProgress::default()
            };
        }
        self.groups.write().unwrap().clear();

        let me = Arc::clone(self);
        // The work is blocking file IO, so it never touches the async runtime.
        let _ = std::thread::Builder::new()
            .name("duw-dupes".into())
            .spawn(move || me.run(scope, min_size, generation));

        generation
    }

    pub fn cancel(&self) {
        // Moving the generation on is enough: the worker checks it constantly.
        self.generation.fetch_add(1, Ordering::SeqCst);
        let mut p = self.progress.lock().unwrap();
        if p.phase != Phase::Done {
            p.phase = Phase::Cancelled;
        }
    }

    fn stale(&self, generation: u64) -> bool {
        self.generation.load(Ordering::SeqCst) != generation
    }

    fn run(self: Arc<Self>, scope: u32, min_size: u64, generation: u64) {
        let started = Instant::now();
        let outcome = self.work(scope, min_size, generation, started);

        if self.stale(generation) {
            return;
        }
        let mut p = self.progress.lock().unwrap();
        p.phase = if outcome {
            Phase::Done
        } else {
            Phase::Cancelled
        };
        p.elapsed_ms = started.elapsed().as_millis() as u64;
    }

    /// Returns false when the run was superseded or cancelled.
    fn work(&self, scope: u32, min_size: u64, generation: u64, started: Instant) -> bool {
        // Stage 1: size groups. Everything needed is already in the tree, so
        // this costs no IO at all.
        let candidates = {
            let t = self.tree.read().unwrap();
            t.duplicate_candidates(scope, min_size.max(1))
        };
        if self.stale(generation) {
            return false;
        }

        let mut by_size: HashMap<u64, Vec<Candidate>> = HashMap::new();
        for c in candidates {
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
            for bucket in self.split_by(group, generation, |this, c| this.window_digest(c)) {
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
            for bucket in self.split_by(group, generation, |this, c| this.full_digest(c)) {
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

        *self.groups.write().unwrap() = groups;
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

    fn window_digest(&self, c: &Candidate) -> Option<Digest> {
        if let Some(d) = self.window.lock().unwrap().get(&c.id) {
            return Some(*d);
        }
        let d = hash_windows(&self.path_of(c), c.size)?;
        self.window.lock().unwrap().insert(c.id, d);
        self.record_read(WINDOW * 2);
        Some(d)
    }

    fn full_digest(&self, c: &Candidate) -> Option<Digest> {
        if let Some(d) = self.full.lock().unwrap().get(&c.id) {
            return Some(*d);
        }
        let d = hash_file(&self.path_of(c))?;
        self.full.lock().unwrap().insert(c.id, d);
        self.record_read(c.size);
        Some(d)
    }

    fn record_read(&self, bytes: u64) {
        let mut p = self.progress.lock().unwrap();
        p.read += 1;
        p.bytes_read += bytes;
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
    let mut file = File::open(path).ok()?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(&size.to_le_bytes());

    let mut buf = vec![0u8; WINDOW as usize];
    read_exact_at(&mut file, 0, &mut buf)?;
    hasher.update(&buf);

    read_exact_at(&mut file, size - WINDOW, &mut buf)?;
    hasher.update(&buf);

    Some(*hasher.finalize().as_bytes())
}

fn read_exact_at(file: &mut File, offset: u64, buf: &mut [u8]) -> Option<()> {
    file.seek(SeekFrom::Start(offset)).ok()?;
    file.read_exact(buf).ok()
}

fn hash_file(path: &Path) -> Option<Digest> {
    let mut file = File::open(path).ok()?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; READ_BUF];
    loop {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => hasher.update(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        };
    }
    Some(*hasher.finalize().as_bytes())
}

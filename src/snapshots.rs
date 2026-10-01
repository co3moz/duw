//! Saved scans, stored as a compact sorted binary table.
//!
//! The file starts with a fixed header - magic, format version, creation time,
//! entry count and byte totals - followed by the table itself. Table records
//! are sorted by path and prefix-compressed against the previous path, so a
//! filesystem tree stays small and, more importantly, can be read and diffed
//! as a stream: listing only reads the header, and comparing a snapshot with
//! the live tree is a linear merge that never builds a map.
//!
//! All integers in the body are LEB128 varints; the header is little-endian.
//! Version 2 adds per-entry error, cloud and foreign-filesystem flags. The
//! saved tree also includes directories (with a trailing `/`) and the root
//! (an empty path). Version 1 and legacy JSON snapshots remain readable.

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

#[cfg(test)]
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

use crate::tree::{FileInfo, FileSource, Kind, NewEntry, Tree, ROOT};

/// Human-readable magic at the very start of every snapshot file.
pub const MAGIC: [u8; 16] = *b"duw snapshot\0\0\0\0";
/// Bumped whenever the table layout changes in a way readers must know about.
pub const FORMAT_VERSION: u16 = 2;
/// Fixed part of the header; variable-length strings follow it.
const HEADER_LEN: usize = 64;
/// Refuses absurd path lengths early when a file is corrupt.
const MAX_PATH: usize = 64 * 1024;

#[cfg(test)]
static TEST_DIR: OnceLock<PathBuf> = OnceLock::new();
#[cfg(test)]
static TEST_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
pub(crate) fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    TEST_DIR.get_or_init(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".test-snapshots"));
    TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Fixed-size part of the file, before the root and app-version strings.
struct Header {
    format_version: u16,
    flags: u16,
    created: u64,
    entry_count: u64,
    total_size: u64,
    total_alloc: u64,
    root: String,
    app_version: String,
}

impl Header {
    fn len(&self) -> usize {
        HEADER_LEN + self.root.len() + self.app_version.len()
    }
}

#[derive(Serialize)]
pub struct SnapshotMeta {
    pub name: String,
    pub root: String,
    pub created: u64,
    pub entries: u64,
    /// Sum of the saved entry sizes (older snapshots contain only files).
    pub bytes: u64,
    /// Size of the snapshot file itself on disk.
    pub file_bytes: u64,
}

impl SnapshotMeta {
    fn from_header(name: &str, header: &Header, file_bytes: u64) -> Self {
        SnapshotMeta {
            name: name.to_string(),
            root: header.root.clone(),
            created: header.created,
            entries: header.entry_count,
            bytes: header.total_size,
            file_bytes,
        }
    }
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
    #[cfg(test)]
    if let Some(test_dir) = TEST_DIR.get() {
        std::fs::create_dir_all(test_dir)?;
        return Ok(test_dir.clone());
    }

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

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn path_for(name: &str, ext: &str) -> io::Result<PathBuf> {
    if !valid_name(name) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid snapshot name",
        ));
    }
    Ok(dir()?.join(format!("{name}.{ext}")))
}

/// Returns the binary path for `name`, converting a legacy JSON snapshot in
/// place the first time it is touched.
fn ensure(name: &str) -> io::Result<PathBuf> {
    let binary = path_for(name, "duws")?;
    if binary.exists() {
        return Ok(binary);
    }
    let legacy = path_for(name, "json")?;
    if legacy.exists() {
        migrate_legacy(&legacy, name)?;
        return Ok(binary);
    }
    Err(io::Error::new(io::ErrorKind::NotFound, "no such snapshot"))
}

/// Saves the current tree, streaming entries straight to disk.
pub fn save(
    name: &str,
    root: &str,
    created: u64,
    files: &mut dyn FileSource,
) -> io::Result<SnapshotMeta> {
    let path = path_for(name, "duws")?;
    let file_bytes = write_at(&path, root, created, files)?;
    let mut file = File::open(&path)?;
    let header = read_header(&mut file)?;
    Ok(SnapshotMeta::from_header(name, &header, file_bytes))
}

/// Header-only metadata: this is what makes listing O(1) per snapshot.
pub fn meta(name: &str) -> io::Result<SnapshotMeta> {
    let path = ensure(name)?;
    meta_at(&path, name)
}

/// Restores a scan without consulting the scanned filesystem. Older snapshots
/// have only file records; their parent directories are reconstructed with
/// zero own sizes and unknown modification times.
pub fn load(name: &str) -> io::Result<(SnapshotMeta, Tree)> {
    let path = ensure(name)?;
    load_at(&path, name)
}

fn load_at(path: &Path, name: &str) -> io::Result<(SnapshotMeta, Tree)> {
    let mut reader = Reader::open_path(path)?;
    let meta = SnapshotMeta::from_header(
        name,
        &reader.header,
        reader.file.get_ref().metadata()?.len(),
    );
    let root_path = Path::new(&meta.root);
    let root_name = root_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| meta.root.clone());
    let mut tree = Tree::new(root_name.clone(), 0, 0, 0);
    let mut nodes: HashMap<(u32, String), u32> = HashMap::new();
    let mut previous: Option<String> = None;
    let mut total_size = 0u64;
    let mut total_alloc = 0u64;
    while reader.advance_entry()? {
        let path = reader.path();
        if previous.as_deref().is_some_and(|prev| prev >= path) {
            return Err(invalid("corrupt snapshot: paths are not strictly sorted"));
        }
        let info = reader.info();
        total_size = total_size
            .checked_add(info.size)
            .ok_or_else(|| invalid("corrupt snapshot: size overflow"))?;
        total_alloc = total_alloc
            .checked_add(info.alloc)
            .ok_or_else(|| invalid("corrupt snapshot: allocation overflow"))?;
        if path.is_empty() {
            if previous.is_some() || info.kind != Kind::Dir {
                return Err(invalid("corrupt snapshot: invalid root record"));
            }
            tree = Tree::new(root_name.clone(), info.size, info.alloc, info.mtime);
            tree.nodes[ROOT as usize].err = info.err;
            tree.nodes[ROOT as usize].cloud = info.cloud;
            tree.nodes[ROOT as usize].foreign = info.foreign;
        } else {
            let relative = if info.kind == Kind::Dir {
                path.strip_suffix('/').unwrap_or(path)
            } else {
                path
            };
            let parts: Vec<&str> = relative.split('/').collect();
            for part in &parts {
                // Each component must remain a filename when node_path joins
                // it to the saved root for rescan, reveal and trash actions.
                let mut components = Path::new(part).components();
                if part.is_empty()
                    || part.contains('\0')
                    || !matches!(components.next(), Some(std::path::Component::Normal(_)))
                    || components.next().is_some()
                    || (cfg!(windows) && part.contains(':'))
                {
                    return Err(invalid("corrupt snapshot: invalid relative path"));
                }
            }
            let mut parent = ROOT;
            for (index, part) in parts.iter().enumerate() {
                let last = index + 1 == parts.len();
                let key = (parent, (*part).to_string());
                if let Some(&id) = nodes.get(&key) {
                    if last || tree.nodes[id as usize].kind != Kind::Dir {
                        return Err(invalid("corrupt snapshot: conflicting entries"));
                    }
                    parent = id;
                    continue;
                }
                if tree.nodes.len() >= u32::MAX as usize {
                    return Err(invalid("snapshot has too many entries"));
                }
                let id = tree.nodes.len() as u32;
                tree.add_children(
                    parent,
                    vec![NewEntry {
                        name: (*part).to_string(),
                        kind: if last { info.kind } else { Kind::Dir },
                        size: if last { info.size } else { 0 },
                        alloc: if last { info.alloc } else { 0 },
                        mtime: if last { info.mtime } else { 0 },
                        err: last && info.err,
                        cloud: last && info.cloud,
                        foreign: last && info.foreign,
                    }],
                );
                nodes.insert(key, id);
                parent = id;
            }
        }
        previous = Some(path.to_string());
    }
    if total_size != meta.bytes || total_alloc != reader.header.total_alloc {
        return Err(invalid("corrupt snapshot: totals do not match"));
    }
    for node in &mut tree.nodes {
        if node.kind == Kind::Dir {
            node.read = true;
        }
    }
    tree.version = 1;
    Ok((meta, tree))
}

pub fn delete(name: &str) -> io::Result<()> {
    let binary = path_for(name, "duws")?;
    let legacy = path_for(name, "json")?;
    let mut removed = false;
    if binary.exists() {
        std::fs::remove_file(&binary)?;
        removed = true;
    }
    if legacy.exists() {
        std::fs::remove_file(&legacy)?;
        removed = true;
    }
    if removed {
        Ok(())
    } else {
        Err(io::Error::new(io::ErrorKind::NotFound, "no such snapshot"))
    }
}

pub fn list() -> std::io::Result<Vec<SnapshotMeta>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir()?)? {
        let entry = entry?;
        let path = entry.path();
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        let Some(name) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let meta = match ext {
            "duws" => meta_at(&path, name),
            // Snapshots written before the binary format are converted once so
            // later listings stay header-only.
            "json" => migrate_legacy(&path, name),
            _ => continue,
        };
        if let Ok(meta) = meta {
            out.push(meta);
        }
    }
    out.sort_unstable_by_key(|m| std::cmp::Reverse(m.created));
    Ok(out)
}

/// Compares a saved snapshot with the live tree, biggest absolute change
/// first.
pub fn diff(name: &str, live: &Tree, limit: usize) -> io::Result<DiffResult> {
    let path = ensure(name)?;
    let mut old = Reader::open_path(&path)?;
    let mut new = live.sorted_files();
    diff_sources(&mut old, &mut new, limit)
}

/// Streaming merge of two path-ordered sources. Only the `limit` biggest
/// changes are remembered, so memory does not grow with the entry count.
pub fn diff_sources(
    old: &mut dyn FileSource,
    new: &mut dyn FileSource,
    limit: usize,
) -> io::Result<DiffResult> {
    let mut heap: BinaryHeap<Ranked> = BinaryHeap::new();
    let mut added_bytes = 0u64;
    let mut removed_bytes = 0u64;
    let mut total_changes = 0u64;

    let mut a = old.advance()?;
    let mut b = new.advance()?;
    while a || b {
        match (a, b) {
            (true, false) => {
                let info = old.info();
                removed_bytes += info.size;
                total_changes += 1;
                record(
                    &mut heap,
                    limit,
                    Change {
                        path: old.path().to_string(),
                        kind: info.kind,
                        old: info.size,
                        new: 0,
                        delta: -(info.size as i64),
                        added: false,
                        removed: true,
                    },
                );
                a = old.advance()?;
            }
            (false, true) => {
                let info = new.info();
                added_bytes += info.size;
                total_changes += 1;
                record(
                    &mut heap,
                    limit,
                    Change {
                        path: new.path().to_string(),
                        kind: info.kind,
                        old: 0,
                        new: info.size,
                        delta: info.size as i64,
                        added: true,
                        removed: false,
                    },
                );
                b = new.advance()?;
            }
            (true, true) => match old.path().cmp(new.path()) {
                Ordering::Less => {
                    let info = old.info();
                    removed_bytes += info.size;
                    total_changes += 1;
                    record(
                        &mut heap,
                        limit,
                        Change {
                            path: old.path().to_string(),
                            kind: info.kind,
                            old: info.size,
                            new: 0,
                            delta: -(info.size as i64),
                            added: false,
                            removed: true,
                        },
                    );
                    a = old.advance()?;
                }
                Ordering::Greater => {
                    let info = new.info();
                    added_bytes += info.size;
                    total_changes += 1;
                    record(
                        &mut heap,
                        limit,
                        Change {
                            path: new.path().to_string(),
                            kind: info.kind,
                            old: 0,
                            new: info.size,
                            delta: info.size as i64,
                            added: true,
                            removed: false,
                        },
                    );
                    b = new.advance()?;
                }
                Ordering::Equal => {
                    let old_info = old.info();
                    let new_info = new.info();
                    if old_info.size != new_info.size {
                        let delta = new_info.size as i64 - old_info.size as i64;
                        if delta > 0 {
                            added_bytes += delta as u64;
                        } else {
                            removed_bytes += -delta as u64;
                        }
                        total_changes += 1;
                        record(
                            &mut heap,
                            limit,
                            Change {
                                path: new.path().to_string(),
                                kind: new_info.kind,
                                old: old_info.size,
                                new: new_info.size,
                                delta,
                                added: false,
                                removed: false,
                            },
                        );
                    }
                    a = old.advance()?;
                    b = new.advance()?;
                }
            },
            (false, false) => break,
        }
    }

    let mut changes: Vec<Change> = heap.into_iter().map(|r| r.change).collect();
    changes.sort_unstable_by(|a, b| {
        b.delta
            .unsigned_abs()
            .cmp(&a.delta.unsigned_abs())
            .then_with(|| a.path.cmp(&b.path))
    });

    Ok(DiffResult {
        added_bytes,
        removed_bytes,
        net: added_bytes as i64 - removed_bytes as i64,
        total_changes,
        changes,
    })
}

/// Keeps the `limit` changes with the largest absolute delta. `BinaryHeap` is
/// a max-heap, so ordering by reversed `abs` makes `peek` the smallest.
struct Ranked {
    abs: u64,
    change: Change,
}

impl PartialEq for Ranked {
    fn eq(&self, other: &Self) -> bool {
        self.abs == other.abs
    }
}
impl Eq for Ranked {}
impl PartialOrd for Ranked {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Ranked {
    fn cmp(&self, other: &Self) -> Ordering {
        other.abs.cmp(&self.abs)
    }
}

fn record(heap: &mut BinaryHeap<Ranked>, limit: usize, change: Change) {
    if limit == 0 {
        return;
    }
    let abs = change.delta.unsigned_abs();
    if heap.len() < limit {
        heap.push(Ranked { abs, change });
    } else if let Some(min) = heap.peek() {
        if abs > min.abs {
            heap.pop();
            heap.push(Ranked { abs, change });
        }
    }
}

/// A snapshot opened for streaming. Implements [`FileSource`] so the live tree
/// and a saved snapshot are interchangeable in a diff.
struct Reader {
    file: BufReader<File>,
    remaining: u64,
    path: String,
    suffix: Vec<u8>,
    info: FileInfo,
    header: Header,
}

impl Reader {
    fn open_path(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let mut file = BufReader::new(file);
        let header = read_header(&mut file)?;
        file.seek(SeekFrom::Start(header.len() as u64))?;
        Ok(Reader {
            file,
            remaining: header.entry_count,
            header,
            path: String::new(),
            suffix: Vec::new(),
            info: FileInfo {
                kind: Kind::File,
                size: 0,
                alloc: 0,
                mtime: 0,
                err: false,
                cloud: false,
                foreign: false,
            },
        })
    }
    fn advance_entry(&mut self) -> io::Result<bool> {
        if self.remaining == 0 {
            return Ok(false);
        }
        let shared = read_varint(&mut self.file)? as usize;
        if shared > self.path.len() || !self.path.is_char_boundary(shared) {
            return Err(invalid("corrupt snapshot: bad prefix"));
        }
        let suffix_len = read_varint(&mut self.file)? as usize;
        if suffix_len > MAX_PATH {
            return Err(invalid("corrupt snapshot: path too long"));
        }
        self.suffix.resize(suffix_len, 0);
        self.file.read_exact(&mut self.suffix)?;
        let suffix =
            std::str::from_utf8(&self.suffix).map_err(|_| invalid("corrupt snapshot: bad path"))?;
        self.path.truncate(shared);
        self.path.push_str(suffix);
        if self.path.len() > MAX_PATH {
            return Err(invalid("corrupt snapshot: path too long"));
        }

        let size = read_varint(&mut self.file)?;
        let alloc = read_varint(&mut self.file)?;
        let mtime = unzigzag(read_varint(&mut self.file)?);
        let kind = kind_from_byte(read_u8(&mut self.file)?);
        let flags = if self.header.format_version >= 2 {
            read_u8(&mut self.file)?
        } else {
            0
        };
        if flags & !7 != 0 {
            return Err(invalid("corrupt snapshot: unknown entry flags"));
        }
        self.info = FileInfo {
            kind,
            size,
            alloc,
            mtime,
            err: flags & 1 != 0,
            cloud: flags & 2 != 0,
            foreign: flags & 4 != 0,
        };
        self.remaining -= 1;
        Ok(true)
    }
}

impl FileSource for Reader {
    fn advance(&mut self) -> io::Result<bool> {
        // Diff remains file-only even when the snapshot also records folders.
        while self.advance_entry()? {
            if self.info.kind != Kind::Dir {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn path(&self) -> &str {
        &self.path
    }

    fn info(&self) -> FileInfo {
        self.info
    }
}

fn meta_at(path: &Path, name: &str) -> io::Result<SnapshotMeta> {
    let file_bytes = std::fs::metadata(path)?.len();
    let mut file = File::open(path)?;
    let header = read_header(&mut file)?;
    Ok(SnapshotMeta::from_header(name, &header, file_bytes))
}

/// Writes a snapshot to `path` and returns its size. The header is written
/// first with empty totals and patched at the end, so the table streams.
fn write_at(path: &Path, root: &str, created: u64, files: &mut dyn FileSource) -> io::Result<u64> {
    let tmp = path.with_extension("duws.tmp");
    let result = write_body(&tmp, root, created, files);
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if path.exists() {
        let _ = std::fs::remove_file(path);
    }
    std::fs::rename(&tmp, path)?;
    Ok(std::fs::metadata(path)?.len())
}

fn write_body(tmp: &Path, root: &str, created: u64, files: &mut dyn FileSource) -> io::Result<()> {
    let file = File::create(tmp)?;
    let mut w = BufWriter::new(file);
    let provisional = Header {
        format_version: FORMAT_VERSION,
        flags: 0,
        created,
        entry_count: 0,
        total_size: 0,
        total_alloc: 0,
        root: root.to_string(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
    };
    write_header(&mut w, &provisional)?;

    let mut count = 0u64;
    let mut total_size = 0u64;
    let mut total_alloc = 0u64;
    let mut prev = String::new();
    while files.advance()? {
        let path = files.path();
        let info = files.info();
        if count > 0 && path < prev.as_str() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "snapshot source is not sorted by path",
            ));
        }
        let shared = common_prefix(prev.as_bytes(), path.as_bytes());
        let suffix = &path.as_bytes()[shared..];
        write_varint(&mut w, shared as u64)?;
        write_varint(&mut w, suffix.len() as u64)?;
        w.write_all(suffix)?;
        write_varint(&mut w, info.size)?;
        write_varint(&mut w, info.alloc)?;
        write_varint(&mut w, zigzag(info.mtime))?;
        w.write_all(&[kind_byte(info.kind)])?;
        let flags =
            u8::from(info.err) | (u8::from(info.cloud) << 1) | (u8::from(info.foreign) << 2);
        w.write_all(&[flags])?;
        count += 1;
        total_size += info.size;
        total_alloc += info.alloc;
        prev.clear();
        prev.push_str(path);
    }
    w.flush()?;

    // Patch entry_count/total_size/total_alloc (offsets 32/40/48).
    let mut file = w.into_inner().map_err(|e| e.into_error())?;
    file.seek(SeekFrom::Start(32))?;
    file.write_all(&count.to_le_bytes())?;
    file.write_all(&total_size.to_le_bytes())?;
    file.write_all(&total_alloc.to_le_bytes())?;
    file.sync_all()?;
    Ok(())
}

fn write_header<W: Write>(w: &mut W, header: &Header) -> io::Result<()> {
    let root = header.root.as_bytes();
    let version = header.app_version.as_bytes();
    let root_len = u16::try_from(root.len()).map_err(|_| invalid("root path too long"))?;
    let version_len = u16::try_from(version.len()).map_err(|_| invalid("version too long"))?;
    let header_len = header.len();

    let mut buf = [0u8; HEADER_LEN];
    buf[0..16].copy_from_slice(&MAGIC);
    buf[16..18].copy_from_slice(&header.format_version.to_le_bytes());
    buf[18..20].copy_from_slice(&header.flags.to_le_bytes());
    buf[20..24].copy_from_slice(&(header_len as u32).to_le_bytes());
    buf[24..32].copy_from_slice(&header.created.to_le_bytes());
    buf[32..40].copy_from_slice(&header.entry_count.to_le_bytes());
    buf[40..48].copy_from_slice(&header.total_size.to_le_bytes());
    buf[48..56].copy_from_slice(&header.total_alloc.to_le_bytes());
    buf[56..58].copy_from_slice(&root_len.to_le_bytes());
    buf[58..60].copy_from_slice(&version_len.to_le_bytes());
    w.write_all(&buf)?;
    w.write_all(root)?;
    w.write_all(version)?;
    Ok(())
}

fn read_header<R: Read>(r: &mut R) -> io::Result<Header> {
    let mut buf = [0u8; HEADER_LEN];
    r.read_exact(&mut buf)?;
    if buf[0..16] != MAGIC {
        return Err(invalid("not a duw snapshot"));
    }
    let format_version = u16::from_le_bytes([buf[16], buf[17]]);
    if !(1..=FORMAT_VERSION).contains(&format_version) {
        return Err(invalid("unsupported snapshot format version"));
    }
    let root_len = u16::from_le_bytes([buf[56], buf[57]]) as usize;
    let version_len = u16::from_le_bytes([buf[58], buf[59]]) as usize;
    let header_len = u32::from_le_bytes([buf[20], buf[21], buf[22], buf[23]]) as usize;
    if header_len != HEADER_LEN + root_len + version_len {
        return Err(invalid("corrupt snapshot header"));
    }
    let mut root = vec![0u8; root_len];
    r.read_exact(&mut root)?;
    let mut version = vec![0u8; version_len];
    r.read_exact(&mut version)?;
    Ok(Header {
        format_version,
        flags: u16::from_le_bytes([buf[18], buf[19]]),
        created: u64::from_le_bytes(buf[24..32].try_into().unwrap()),
        entry_count: u64::from_le_bytes(buf[32..40].try_into().unwrap()),
        total_size: u64::from_le_bytes(buf[40..48].try_into().unwrap()),
        total_alloc: u64::from_le_bytes(buf[48..56].try_into().unwrap()),
        root: String::from_utf8(root).map_err(|_| invalid("corrupt snapshot: bad root"))?,
        app_version: String::from_utf8(version)
            .map_err(|_| invalid("corrupt snapshot: bad version"))?,
    })
}

fn common_prefix(a: &[u8], b: &[u8]) -> usize {
    let max = a.len().min(b.len());
    let mut i = 0;
    while i < max && a[i] == b[i] {
        i += 1;
    }
    // Never end inside a UTF-8 character: the count is used to truncate a
    // valid path, so back off to the start of the character.
    while i > 0 && i < a.len() && (a[i] & 0xc0) == 0x80 {
        i -= 1;
    }
    i
}

fn write_varint<W: Write>(w: &mut W, mut value: u64) -> io::Result<()> {
    let mut buf = [0u8; 10];
    let mut len = 0;
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        buf[len] = byte;
        len += 1;
        if value == 0 {
            break;
        }
    }
    w.write_all(&buf[..len])
}

fn read_varint<R: Read>(r: &mut R) -> io::Result<u64> {
    let mut result = 0u64;
    for i in 0..10 {
        let byte = read_u8(r)?;
        if i == 9 && byte > 1 {
            return Err(invalid("corrupt snapshot: varint overflow"));
        }
        result |= ((byte & 0x7f) as u64) << (7 * i);
        if byte & 0x80 == 0 {
            return Ok(result);
        }
    }
    Err(invalid("corrupt snapshot: varint overflow"))
}

fn read_u8<R: Read>(r: &mut R) -> io::Result<u8> {
    let mut byte = [0u8; 1];
    r.read_exact(&mut byte)?;
    Ok(byte[0])
}

/// mtime is stored zigzagged so negative timestamps (before 1970) stay small.
fn zigzag(value: i64) -> u64 {
    ((value << 1) ^ (value >> 63)) as u64
}

fn unzigzag(value: u64) -> i64 {
    ((value >> 1) as i64) ^ -((value & 1) as i64)
}

fn kind_byte(kind: Kind) -> u8 {
    match kind {
        Kind::Dir => 0,
        Kind::File => 1,
        Kind::Link => 2,
        Kind::Other => 3,
    }
}

fn kind_from_byte(byte: u8) -> Kind {
    match byte {
        0 => Kind::Dir,
        1 => Kind::File,
        2 => Kind::Link,
        _ => Kind::Other,
    }
}

/// Snapshots written by older versions, kept only so they can be converted.
#[derive(Deserialize)]
struct LegacySnapshot {
    root: String,
    created: u64,
    entries: Vec<LegacyEntry>,
}

#[derive(Deserialize)]
struct LegacyEntry {
    path: String,
    kind: Kind,
    size: u64,
    alloc: u64,
    mtime: i64,
}

struct LegacyFiles {
    entries: Vec<LegacyEntry>,
    next: usize,
}

impl FileSource for LegacyFiles {
    fn advance(&mut self) -> io::Result<bool> {
        if self.next >= self.entries.len() {
            return Ok(false);
        }
        self.next += 1;
        Ok(true)
    }

    fn path(&self) -> &str {
        &self.entries[self.next - 1].path
    }

    fn info(&self) -> FileInfo {
        let e = &self.entries[self.next - 1];
        FileInfo {
            kind: e.kind,
            size: e.size,
            alloc: e.alloc,
            mtime: e.mtime,
            err: false,
            cloud: false,
            foreign: false,
        }
    }
}

/// One-time conversion of a pre-binary JSON snapshot. The original file is
/// removed only after the replacement is written successfully.
fn migrate_legacy(path: &Path, name: &str) -> io::Result<SnapshotMeta> {
    let data = std::fs::read(path)?;
    let legacy: LegacySnapshot = serde_json::from_slice(&data).map_err(io::Error::other)?;
    let mut entries = legacy.entries;
    entries.sort_unstable_by(|a, b| a.path.cmp(&b.path));
    let mut files = LegacyFiles { entries, next: 0 };
    let binary = path_for(name, "duws")?;
    let file_bytes = write_at(&binary, &legacy.root, legacy.created, &mut files)?;
    let mut file = File::open(&binary)?;
    let header = read_header(&mut file)?;
    let _ = std::fs::remove_file(path);
    Ok(SnapshotMeta::from_header(name, &header, file_bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    fn entry(path: &str, size: u64) -> LegacyEntry {
        LegacyEntry {
            path: path.into(),
            kind: Kind::File,
            size,
            alloc: size,
            mtime: 0,
        }
    }

    fn source(entries: Vec<LegacyEntry>) -> LegacyFiles {
        LegacyFiles { entries, next: 0 }
    }

    struct SnapshotGuard(String);

    impl SnapshotGuard {
        fn new() -> Self {
            TEST_DIR
                .get_or_init(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".test-snapshots"));
            Self(format!(
                "test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ))
        }

        fn name(&self) -> &str {
            &self.0
        }
    }

    impl Drop for SnapshotGuard {
        fn drop(&mut self) {
            let _ = delete(&self.0);
            if let Ok(dir) = dir() {
                let _ = std::fs::remove_file(dir.join(format!("{}.json", self.0)));
                let _ = std::fs::remove_file(dir.join(format!("{}.duws.tmp", self.0)));
            }
        }
    }

    #[test]
    fn varints_round_trip() {
        for value in [0u64, 1, 127, 128, 300, 16_384, u64::MAX] {
            let mut buf = Vec::new();
            write_varint(&mut buf, value).unwrap();
            let mut cursor = buf.as_slice();
            assert_eq!(read_varint(&mut cursor).unwrap(), value);
        }
    }

    #[test]
    fn prefix_stops_at_char_boundaries() {
        assert_eq!(common_prefix("héllo".as_bytes(), "híx".as_bytes()), 1);
        assert_eq!(common_prefix("αβ".as_bytes(), "αγ".as_bytes()), 2);
        assert_eq!(common_prefix(b"abc", b"abc"), 3);
    }

    #[test]
    fn binary_round_trip_streams_entries() {
        let path = std::env::temp_dir().join(format!("duw-snap-test-{}.duws", std::process::id()));
        let mut files = source(vec![
            entry("a/b.txt", 10),
            entry("z.txt", 30),
            entry("α/β.txt", 20),
        ]);
        let bytes = write_at(&path, "C:\\root", 42, &mut files).unwrap();
        assert!(bytes > HEADER_LEN as u64);

        let mut file = File::open(&path).unwrap();
        let header = read_header(&mut file).unwrap();
        assert_eq!(header.entry_count, 3);
        assert_eq!(header.root, "C:\\root");
        assert_eq!(header.created, 42);

        let mut reader = Reader::open_path(&path).unwrap();

        // Sorted order is what the table guarantees.
        assert!(reader.advance().unwrap());
        assert_eq!(reader.path(), "a/b.txt");
        assert_eq!(reader.info().size, 10);
        assert!(reader.advance().unwrap());
        assert_eq!(reader.path(), "z.txt");
        assert!(reader.advance().unwrap());
        assert_eq!(reader.path(), "α/β.txt");
        assert_eq!(reader.info().size, 20);
        assert!(!reader.advance().unwrap());

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn diff_reports_added_removed_and_grown() {
        let mut old = source(vec![entry("a", 100), entry("b", 200), entry("c", 300)]);
        let mut new = source(vec![entry("a", 100), entry("b", 500), entry("d", 50)]);
        let d = diff_sources(&mut old, &mut new, 100).unwrap();

        assert_eq!(d.added_bytes, 350);
        assert_eq!(d.removed_bytes, 300);
        assert_eq!(d.net, 50);
        assert_eq!(d.total_changes, 3);

        let paths: Vec<&str> = d.changes.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(paths.len(), 3);
        assert!(paths[..2].contains(&"b") && paths[..2].contains(&"c"));
        assert_eq!(paths[2], "d");
        assert!(d.changes.iter().find(|c| c.path == "d").unwrap().added);
        assert!(d.changes.iter().find(|c| c.path == "c").unwrap().removed);
    }

    #[test]
    fn diff_keeps_only_the_biggest_changes() {
        let mut old = source(vec![entry("a", 10), entry("b", 20), entry("c", 30)]);
        let mut new = source(vec![entry("a", 110), entry("b", 20), entry("c", 530)]);
        let d = diff_sources(&mut old, &mut new, 1).unwrap();
        assert_eq!(d.total_changes, 2);
        assert_eq!(d.changes.len(), 1);
        assert_eq!(d.changes[0].path, "c");
        assert_eq!(d.changes[0].delta, 500);
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

    #[test]
    fn loaded_snapshots_restore_the_full_tree_and_remain_file_diff_compatible() {
        let _lock = test_lock();
        let guard = SnapshotGuard::new();
        let entry = |name: &str, kind, size| NewEntry {
            name: name.into(),
            kind,
            size,
            alloc: size * 2,
            mtime: 77,
            err: false,
            cloud: false,
            foreign: false,
        };
        let mut original = Tree::new("saved-root".into(), 3, 8, 70);
        let dirs = original.add_children(
            ROOT,
            vec![
                entry("a", Kind::Dir, 5),
                entry("a.txt", Kind::File, 10),
                entry("empty", Kind::Dir, 4),
                entry("α", Kind::Dir, 6),
            ],
        );
        original.add_children(dirs[0], vec![entry("nested.bin", Kind::File, 20)]);
        original.add_children(dirs[2], vec![entry("β", Kind::Link, 7)]);
        original.mark_read(dirs[1], false);
        let cloud_id = original.find_rel_path("a/nested.bin").unwrap();
        original.nodes[cloud_id as usize].cloud = true;
        original.nodes[dirs[2] as usize].foreign = true;
        original.nodes[dirs[1] as usize].err = true;
        save(
            guard.name(),
            "saved-root",
            42,
            &mut original.sorted_entries(),
        )
        .unwrap();

        let (meta, restored) = load(guard.name()).unwrap();
        assert_eq!(meta.root, "saved-root");
        assert_eq!(meta.bytes, original.nodes[ROOT as usize].total_size);
        assert_eq!(meta.entries as usize, original.nodes.len());
        assert_eq!(restored.stats.files, original.stats.files);
        assert_eq!(restored.stats.dirs, original.stats.dirs);
        assert_eq!(restored.stats.size, original.stats.size);
        assert_eq!(restored.stats.alloc, original.stats.alloc);
        for (id, expected) in original.nodes.iter().enumerate() {
            let path = original.rel_path(id as u32);
            let actual = &restored.nodes[restored.find_rel_path(&path).unwrap() as usize];
            assert_eq!(actual.kind, expected.kind, "{path}");
            assert_eq!(actual.self_size, expected.self_size, "{path}");
            assert_eq!(actual.self_alloc, expected.self_alloc, "{path}");
            assert_eq!(actual.total_size, expected.total_size, "{path}");
            assert_eq!(actual.total_alloc, expected.total_alloc, "{path}");
            assert_eq!(actual.files, expected.files, "{path}");
            assert_eq!(actual.dirs, expected.dirs, "{path}");
            assert_eq!(actual.mtime, expected.mtime, "{path}");
            assert_eq!(actual.err, expected.err, "{path}");
            assert_eq!(actual.cloud, expected.cloud, "{path}");
            assert_eq!(actual.foreign, expected.foreign, "{path}");
            if actual.kind == Kind::Dir {
                assert!(actual.read, "{path}");
            }
        }
        assert_eq!(
            restored.by_extension(ROOT)[0].ext,
            original.by_extension(ROOT)[0].ext
        );
        assert!(restored
            .duplicate_candidates(ROOT, 1)
            .iter()
            .all(|c| c.path != "a/nested.bin"));
        assert_eq!(diff(guard.name(), &restored, 100).unwrap().total_changes, 0);
    }

    #[test]
    fn version_one_snapshots_load_with_reconstructed_parent_directories() {
        let _lock = test_lock();
        let guard = SnapshotGuard::new();
        let mut bytes = Vec::new();
        write_header(
            &mut bytes,
            &Header {
                format_version: 1,
                flags: 0,
                created: 42,
                entry_count: 1,
                total_size: 8,
                total_alloc: 16,
                root: "old-root".into(),
                app_version: "0.1.5".into(),
            },
        )
        .unwrap();
        let path = "nested/old.txt";
        write_varint(&mut bytes, 0).unwrap();
        write_varint(&mut bytes, path.len() as u64).unwrap();
        bytes.extend_from_slice(path.as_bytes());
        write_varint(&mut bytes, 8).unwrap();
        write_varint(&mut bytes, 16).unwrap();
        write_varint(&mut bytes, zigzag(77)).unwrap();
        bytes.push(kind_byte(Kind::File));
        std::fs::write(path_for(guard.name(), "duws").unwrap(), bytes).unwrap();

        let (_, restored) = load(guard.name()).unwrap();
        assert_eq!(restored.stats.files, 1);
        assert_eq!(restored.stats.dirs, 1);
        assert_eq!(restored.stats.size, 8);
        let dir = restored.find_rel_path("nested").unwrap();
        assert!(restored.nodes[dir as usize].read);
        let file = restored.find_rel_path(path).unwrap();
        assert_eq!(restored.nodes[file as usize].mtime, 77);
        assert_eq!(diff(guard.name(), &restored, 100).unwrap().total_changes, 0);
    }

    #[test]
    fn loading_rejects_unsafe_and_conflicting_paths() {
        let _lock = test_lock();
        let guard = SnapshotGuard::new();
        for path in ["../escape", "/absolute", "a/./file", "a//file", "a\0file"] {
            save(guard.name(), "root", 0, &mut source(vec![entry(path, 1)])).unwrap();
            assert!(load(guard.name()).is_err(), "accepted {path:?}");
        }
        save(
            guard.name(),
            "root",
            0,
            &mut source(vec![entry("a", 1), entry("a/b", 2)]),
        )
        .unwrap();
        assert!(load(guard.name()).is_err());
    }

    #[test]
    fn public_snapshot_lifecycle_supports_metadata_listing_and_diff() {
        let _lock = test_lock();
        let guard = SnapshotGuard::new();
        let mut files = source(vec![entry("old.txt", 10), entry("same.bin", 4)]);
        let saved = save(guard.name(), "test-root", 42, &mut files).unwrap();
        assert_eq!(saved.name, guard.name());
        assert_eq!(saved.root, "test-root");
        assert_eq!(saved.entries, 2);
        assert_eq!(saved.bytes, 14);

        let metadata = meta(guard.name()).unwrap();
        assert_eq!(metadata.created, 42);
        assert!(list().unwrap().iter().any(|m| m.name == guard.name()));

        let mut live = Tree::new("root".into(), 0, 0, 0);
        live.add_children(
            crate::tree::ROOT,
            vec![
                crate::tree::NewEntry {
                    name: "old.txt".into(),
                    kind: Kind::File,
                    size: 20,
                    alloc: 20,
                    mtime: 0,
                    err: false,
                    cloud: false,
                    foreign: false,
                },
                crate::tree::NewEntry {
                    name: "new.txt".into(),
                    kind: Kind::File,
                    size: 7,
                    alloc: 7,
                    mtime: 0,
                    err: false,
                    cloud: false,
                    foreign: false,
                },
            ],
        );
        let diff = diff(guard.name(), &live, 10).unwrap();
        assert_eq!(diff.total_changes, 3);
        assert_eq!(diff.net, 13);
        assert!(diff
            .changes
            .iter()
            .any(|c| c.path == "old.txt" && c.old == 10 && c.new == 20));
        assert!(diff
            .changes
            .iter()
            .any(|c| c.path == "same.bin" && c.removed));
        assert!(diff.changes.iter().any(|c| c.path == "new.txt" && c.added));

        delete(guard.name()).unwrap();
        let error = match meta(guard.name()) {
            Ok(_) => panic!("deleted snapshot still exists"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn legacy_json_snapshots_are_migrated_on_first_access() {
        let _lock = test_lock();
        let guard = SnapshotGuard::new();
        let dir = dir().unwrap();
        let legacy_path = dir.join(format!("{}.json", guard.name()));
        let legacy = serde_json::json!({
            "root": "legacy-root",
            "created": 7,
            "entries": [{
                "path": "legacy.txt",
                "kind": "file",
                "size": 12,
                "alloc": 12,
                "mtime": 0
            }]
        });
        std::fs::write(&legacy_path, serde_json::to_vec(&legacy).unwrap()).unwrap();

        let metadata = meta(guard.name()).unwrap();
        assert_eq!(metadata.root, "legacy-root");
        assert_eq!(metadata.entries, 1);
        assert!(dir.join(format!("{}.duws", guard.name())).exists());
        assert!(!legacy_path.exists());
    }

    #[test]
    fn corrupt_snapshot_headers_are_rejected() {
        let _lock = test_lock();
        let guard = SnapshotGuard::new();
        let path = path_for(guard.name(), "duws").unwrap();
        std::fs::write(path, b"not a snapshot").unwrap();
        let error = match meta(guard.name()) {
            Ok(_) => panic!("corrupt snapshot was accepted"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn saving_unsorted_entries_returns_an_input_error() {
        let _lock = test_lock();
        let guard = SnapshotGuard::new();
        let mut files = source(vec![entry("z", 1), entry("a", 1)]);
        let error = match save(guard.name(), "root", 0, &mut files) {
            Ok(_) => panic!("unsorted snapshot source was accepted"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(!path_for(guard.name(), "duws").unwrap().exists());
    }

    #[test]
    fn binary_readers_reject_corrupt_headers_and_varints() {
        fn error_kind<T>(result: io::Result<T>) -> io::ErrorKind {
            match result {
                Ok(_) => panic!("corrupt input was accepted"),
                Err(error) => error.kind(),
            }
        }

        let header = Header {
            format_version: FORMAT_VERSION,
            flags: 0,
            created: 1,
            entry_count: 0,
            total_size: 0,
            total_alloc: 0,
            root: "root".into(),
            app_version: "test".into(),
        };
        let mut valid = Vec::new();
        write_header(&mut valid, &header).unwrap();

        let mut bad_magic = valid.clone();
        bad_magic[0] = b'x';
        assert_eq!(
            error_kind(read_header(&mut bad_magic.as_slice())),
            io::ErrorKind::InvalidData
        );

        let mut bad_version = valid.clone();
        bad_version[16..18].copy_from_slice(&99u16.to_le_bytes());
        assert_eq!(
            error_kind(read_header(&mut bad_version.as_slice())),
            io::ErrorKind::InvalidData
        );

        let mut bad_length = valid.clone();
        bad_length[20..24].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(
            error_kind(read_header(&mut bad_length.as_slice())),
            io::ErrorKind::InvalidData
        );

        let mut overflow = &[0x80u8; 10][..];
        assert_eq!(
            error_kind(read_varint(&mut overflow)),
            io::ErrorKind::InvalidData
        );
        assert_eq!(kind_from_byte(255), Kind::Other);
    }

    #[test]
    fn zero_diff_limit_keeps_totals_but_no_changes() {
        let mut old = source(vec![entry("a", 10)]);
        let mut new = source(vec![entry("a", 20), entry("b", 30)]);
        let diff = diff_sources(&mut old, &mut new, 0).unwrap();
        assert_eq!(diff.total_changes, 2);
        assert_eq!(diff.net, 40);
        assert!(diff.changes.is_empty());
    }
}

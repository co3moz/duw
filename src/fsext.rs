//! Platform specific metadata bits: on-disk size, device ids and hard links.

use std::fs::Metadata;
use std::path::Path;
use std::time::UNIX_EPOCH;

pub fn mtime(md: &Metadata) -> i64 {
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(unix)]
mod imp {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    pub fn init(_root: &Path) {}

    /// (apparent size, on-disk size)
    pub fn sizes(md: &Metadata) -> (u64, u64) {
        (md.len(), md.blocks() * 512)
    }

    pub fn device(md: &Metadata) -> u64 {
        md.dev()
    }

    /// `Some((dev, ino))` when the entry has more than one link and therefore
    /// needs to be counted only once.
    pub fn hardlink_key(md: &Metadata) -> Option<(u64, u64)> {
        if md.nlink() > 1 {
            Some((md.dev(), md.ino()))
        } else {
            None
        }
    }

    pub const ONE_FILE_SYSTEM_SUPPORTED: bool = true;
    pub const HARDLINK_DEDUP_SUPPORTED: bool = true;
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::os::windows::ffi::OsStrExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceW;

    static CLUSTER: AtomicU64 = AtomicU64::new(4096);

    /// Windows has no cheap per-entry allocation size (`GetCompressedFileSize`
    /// is an extra syscall per file), so on-disk size is approximated by
    /// rounding up to the volume's cluster size. That is exact for ordinary
    /// files and only wrong for compressed or sparse ones.
    pub fn init(root: &Path) {
        let mut wide: Vec<u16> = match volume_root(root) {
            Some(v) => v.encode_utf16().collect(),
            None => return,
        };
        wide.push(0);
        let mut sectors_per_cluster = 0u32;
        let mut bytes_per_sector = 0u32;
        let mut free_clusters = 0u32;
        let mut total_clusters = 0u32;
        let ok = unsafe {
            GetDiskFreeSpaceW(
                wide.as_ptr(),
                &mut sectors_per_cluster,
                &mut bytes_per_sector,
                &mut free_clusters,
                &mut total_clusters,
            )
        };
        if ok != 0 {
            let cluster = sectors_per_cluster as u64 * bytes_per_sector as u64;
            if cluster > 0 {
                CLUSTER.store(cluster, Ordering::Relaxed);
            }
        }
    }

    fn volume_root(path: &Path) -> Option<String> {
        let s = path.as_os_str().encode_wide().collect::<Vec<u16>>();
        let s = String::from_utf16(&s).ok()?;
        let s = s.strip_prefix(r"\\?\").unwrap_or(&s).to_string();
        let bytes = s.as_bytes();
        if bytes.len() >= 2 && bytes[1] == b':' {
            return Some(format!("{}:\\", bytes[0] as char));
        }
        None
    }

    pub fn sizes(md: &Metadata) -> (u64, u64) {
        let size = md.len();
        let cluster = CLUSTER.load(Ordering::Relaxed);
        let alloc = size.div_ceil(cluster) * cluster;
        (size, alloc)
    }

    pub fn device(_md: &Metadata) -> u64 {
        0
    }

    pub fn hardlink_key(_md: &Metadata) -> Option<(u64, u64)> {
        None
    }

    pub const ONE_FILE_SYSTEM_SUPPORTED: bool = false;
    pub const HARDLINK_DEDUP_SUPPORTED: bool = false;
}

#[cfg(not(any(unix, windows)))]
mod imp {
    use super::*;

    pub fn init(_root: &Path) {}

    pub fn sizes(md: &Metadata) -> (u64, u64) {
        (md.len(), md.len())
    }

    pub fn device(_md: &Metadata) -> u64 {
        0
    }

    pub fn hardlink_key(_md: &Metadata) -> Option<(u64, u64)> {
        None
    }

    pub const ONE_FILE_SYSTEM_SUPPORTED: bool = false;
    pub const HARDLINK_DEDUP_SUPPORTED: bool = false;
}

pub use imp::{
    device, hardlink_key, init, sizes, HARDLINK_DEDUP_SUPPORTED, ONE_FILE_SYSTEM_SUPPORTED,
};

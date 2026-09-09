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

    pub fn is_cloud_backed(_md: &Metadata, _path: &Path) -> bool {
        false
    }

    pub const ONE_FILE_SYSTEM_SUPPORTED: bool = true;
    pub const HARDLINK_DEDUP_SUPPORTED: bool = true;
    pub const CLOUD_DETECTION_SUPPORTED: bool = false;
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::MetadataExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::{
        FindClose, FindFirstFileW, GetDiskFreeSpaceW, WIN32_FIND_DATAW,
    };

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

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    const FILE_ATTRIBUTE_OFFLINE: u32 = 0x0000_1000;
    const FILE_ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x0004_0000;
    const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;

    /// True when the entry reports a size but its bytes are not on this disk:
    /// OneDrive, iCloud and the like keep placeholders that only look local.
    pub fn is_cloud_backed(md: &Metadata, path: &Path) -> bool {
        let attrs = md.file_attributes();

        // Dehydrated files say so in their attributes.
        const DEHYDRATED: u32 = FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS
            | FILE_ATTRIBUTE_RECALL_ON_OPEN
            | FILE_ATTRIBUTE_OFFLINE;
        if attrs & DEHYDRATED != 0 {
            return true;
        }

        // Placeholder directories carry nothing but a reparse point, so the tag
        // has to be read to tell a cloud root from a junction or a container
        // mount. Only reparse points get this extra call, and they are rare.
        //
        // Files are deliberately not classified this way: one that still has a
        // cloud tag but no recall flag has been downloaded, and its bytes do
        // sit on this disk.
        if md.is_dir() && attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return matches!(reparse_tag(path), Some(tag) if is_cloud_tag(tag));
        }
        false
    }

    /// `IO_REPARSE_TAG_CLOUD` through `IO_REPARSE_TAG_CLOUD_F`, which differ
    /// only in one nibble (0x9000_001A .. 0x9000_F01A).
    fn is_cloud_tag(tag: u32) -> bool {
        tag & 0xFFFF_0FFF == 0x9000_001A
    }

    /// Reads the reparse tag through the directory-enumeration API.
    ///
    /// This must never open the entry. Opening a cloud placeholder, even with
    /// no access rights and `FILE_FLAG_OPEN_REPARSE_POINT`, makes the sync
    /// filter hydrate it, so a tool meant to avoid downloads would trigger
    /// them. `FindFirstFileW` only reads the directory record and reports the
    /// tag in `dwReserved0`.
    fn reparse_tag(path: &Path) -> Option<u32> {
        let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        wide.push(0);

        unsafe {
            let mut data: WIN32_FIND_DATAW = std::mem::zeroed();
            let handle = FindFirstFileW(wide.as_ptr(), &mut data);
            if handle == INVALID_HANDLE_VALUE {
                return None;
            }
            FindClose(handle);

            if data.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                Some(data.dwReserved0)
            } else {
                None
            }
        }
    }

    pub const ONE_FILE_SYSTEM_SUPPORTED: bool = false;
    pub const HARDLINK_DEDUP_SUPPORTED: bool = false;
    pub const CLOUD_DETECTION_SUPPORTED: bool = true;
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

    pub fn is_cloud_backed(_md: &Metadata, _path: &Path) -> bool {
        false
    }

    pub const ONE_FILE_SYSTEM_SUPPORTED: bool = false;
    pub const HARDLINK_DEDUP_SUPPORTED: bool = false;
    pub const CLOUD_DETECTION_SUPPORTED: bool = false;
}

pub use imp::{
    device, hardlink_key, init, is_cloud_backed, sizes, CLOUD_DETECTION_SUPPORTED,
    HARDLINK_DEDUP_SUPPORTED, ONE_FILE_SYSTEM_SUPPORTED,
};

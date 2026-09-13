//! File actions the UI can ask for: opening an entry in the OS file manager and
//! moving it to the trash. Deleting is deliberately never permanent.

use std::path::Path;

/// Opens the entry's folder with the entry selected where the platform allows
/// it.
pub fn reveal(path: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;

        // explorer cannot be waited on reliably and exits with a non-zero code
        // even when it works, so the status is ignored on purpose.
        std::process::Command::new("explorer")
            .arg(format!("/select,{}", path.display()))
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()?;
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg("-R")
            .arg(path)
            .spawn()?;
        Ok(())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let dir = if path.is_dir() {
            path
        } else {
            path.parent().unwrap_or(path)
        };
        std::process::Command::new("xdg-open").arg(dir).spawn()?;
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err(std::io::Error::other(
            "revealing files is not supported on this platform",
        ))
    }
}

/// Moves the entry to the operating system's trash. Never deletes permanently,
/// so a mistake is recoverable.
pub fn to_trash(path: &Path) -> std::io::Result<()> {
    trash::delete(path).map_err(std::io::Error::other)
}

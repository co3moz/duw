//! File actions the UI can ask for: opening an entry in the OS file manager and
//! moving it to the trash. Deleting is deliberately never permanent.

use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

struct RevealCommand {
    program: &'static str,
    args: Vec<OsString>,
}

fn reveal_command(path: &Path) -> std::io::Result<RevealCommand> {
    #[cfg(windows)]
    {
        Ok(RevealCommand {
            program: "explorer",
            args: vec![format!("/select,{}", path.display()).into()],
        })
    }
    #[cfg(target_os = "macos")]
    {
        Ok(RevealCommand {
            program: "open",
            args: vec!["-R".into(), path.as_os_str().to_os_string()],
        })
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let dir = if path.is_dir() {
            path
        } else {
            path.parent().unwrap_or(path)
        };
        Ok(RevealCommand {
            program: "xdg-open",
            args: vec![dir.as_os_str().to_os_string()],
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err(std::io::Error::other(
            "revealing files is not supported on this platform",
        ))
    }
}

/// Opens the entry's folder with the entry selected where the platform allows
/// it.
pub fn reveal(path: &Path) -> std::io::Result<()> {
    let spec = reveal_command(path)?;
    let mut command = Command::new(spec.program);
    command.args(spec.args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;

        // explorer cannot be waited on reliably and exits with a non-zero code
        // even when it works, so the status is ignored on purpose.
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command.spawn()?;
    Ok(())
}

/// Moves the entry to the operating system's trash. Never deletes permanently,
/// so a mistake is recoverable.
pub fn to_trash(path: &Path) -> std::io::Result<()> {
    trash::delete(path).map_err(std::io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reveal_builds_a_platform_command_without_spawning_it() {
        let path = std::path::Path::new("parent/entry.txt");
        let command = reveal_command(path).unwrap();

        #[cfg(windows)]
        {
            assert_eq!(command.program, "explorer");
            assert_eq!(
                command.args,
                vec![OsString::from("/select,parent/entry.txt")]
            );
        }
        #[cfg(target_os = "macos")]
        {
            assert_eq!(command.program, "open");
            assert_eq!(
                command.args,
                vec![OsString::from("-R"), path.as_os_str().to_os_string()]
            );
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            assert_eq!(command.program, "xdg-open");
            assert_eq!(command.args, vec![OsString::from("parent")]);
        }
    }

    #[test]
    fn moving_a_missing_path_to_trash_returns_an_error() {
        let path = std::env::temp_dir().join(format!("duw-missing-trash-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        assert!(to_trash(&path).is_err());
    }

    #[test]
    fn moving_an_existing_path_to_trash_removes_it_from_place() {
        let path = std::env::temp_dir().join(format!("duw-existing-trash-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, b"recoverable").unwrap();
        to_trash(&path).unwrap();
        assert!(!path.exists());
    }
}

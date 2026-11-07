//! Where Storecat keeps its data.
//!
//! Default: `%LOCALAPPDATA%\io.storecat.app` on Windows, `~/Library/Application
//! Support/Storecat` on macOS, `$XDG_DATA_HOME/Storecat` on Linux. `STORECAT_DATA_DIR`
//! overrides it, which the CLI, tests and portable installs rely on.
//!
//! On Windows the installer puts the program in `%LOCALAPPDATA%\Storecat`, so data lives
//! in a folder named after the app identifier instead. That is also where the WebView
//! keeps its cache, and the folder the uninstaller's "Delete the application data" option
//! removes. Catalogues left in the program folder by 0.9.0 are moved on first start.

use std::path::{Path, PathBuf};

pub const ENV_DATA_DIR: &str = "STORECAT_DATA_DIR";

/// The data folder's name under `%LOCALAPPDATA%` on Windows: the app identifier from
/// `tauri.conf.json`.
pub const WINDOWS_DATA_FOLDER: &str = "io.storecat.app";

/// Everything Storecat writes to its data folder. Used to move a catalogue out of the
/// program folder without touching the program's own files.
const DATA_ITEMS: &[&str] = &[
    "master.db",
    "master.db-wal",
    "master.db-shm",
    "master.db-journal",
    "dupes.db",
    "dupes.db-wal",
    "dupes.db-shm",
    "dupes.db-journal",
    "volumes",
    "index",
    "logs",
    "tools",
];

/// Move a catalogue from `legacy` into `root` once, and return the folder to use.
///
/// Nothing moves when `root` already has a catalogue or `legacy` has none. Items move by
/// rename (same disk, so it is instant). If any rename fails, the ones already moved go
/// back and `legacy` is returned, so a failed move never leaves the catalogue split.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn migrate_legacy(legacy: &Path, root: &Path) -> PathBuf {
    if root.join("master.db").exists() || !legacy.join("master.db").exists() {
        return root.to_path_buf();
    }
    if std::fs::create_dir_all(root).is_err() {
        return legacy.to_path_buf();
    }
    let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
    for name in DATA_ITEMS {
        let from = legacy.join(name);
        if !from.exists() {
            continue;
        }
        let to = root.join(name);
        // Never replace anything already there (on Windows, rename would).
        if to.exists() || std::fs::rename(&from, &to).is_err() {
            for (from, to) in moved.iter().rev() {
                let _ = std::fs::rename(to, from);
            }
            return legacy.to_path_buf();
        }
        moved.push((from, to));
    }
    root.to_path_buf()
}

/// Whether `root` is the root of the drive the operating system runs from (`C:\` on most
/// Windows PCs, `/` elsewhere).
pub fn is_system_drive(root: &str) -> bool {
    let root = root.trim_end_matches(['\\', '/']);
    #[cfg(windows)]
    {
        let system = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        root.eq_ignore_ascii_case(system.trim_end_matches(['\\', '/']))
    }
    #[cfg(not(windows))]
    {
        root.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct DataDirs {
    pub root: PathBuf,
}

impl DataDirs {
    /// Resolve the data directory from the environment, falling back to the OS default.
    pub fn resolve() -> Self {
        if let Some(dir) = std::env::var_os(ENV_DATA_DIR) {
            return Self {
                root: PathBuf::from(dir),
            };
        }
        let base = dirs::data_local_dir().unwrap_or_else(|| PathBuf::from("."));
        #[cfg(windows)]
        let root = migrate_legacy(&base.join("Storecat"), &base.join(WINDOWS_DATA_FOLDER));
        #[cfg(not(windows))]
        let root = base.join("Storecat");
        Self { root }
    }

    pub fn at(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }

    pub fn master_db(&self) -> PathBuf {
        self.root.join("master.db")
    }

    pub fn dupes_db(&self) -> PathBuf {
        self.root.join("dupes.db")
    }

    pub fn volumes_dir(&self) -> PathBuf {
        self.root.join("volumes")
    }

    pub fn volume_db(&self, volume_id: i64) -> PathBuf {
        self.volumes_dir().join(format!("{volume_id}.db"))
    }

    /// Scans build the new catalogue here and rename it over the live one on success.
    pub fn volume_db_tmp(&self, volume_id: i64) -> PathBuf {
        self.volumes_dir().join(format!("{volume_id}.db.scanning"))
    }

    pub fn thumbs_db(&self, volume_id: i64) -> PathBuf {
        self.volumes_dir().join(format!("{volume_id}.thumbs.db"))
    }

    pub fn thumbs_db_tmp(&self, volume_id: i64) -> PathBuf {
        self.volumes_dir()
            .join(format!("{volume_id}.thumbs.db.scanning"))
    }

    pub fn volume_photo(&self, volume_id: i64) -> PathBuf {
        self.volumes_dir().join(format!("{volume_id}.photo.jpg"))
    }

    pub fn index_dir(&self) -> PathBuf {
        self.root.join("index")
    }

    pub fn tools_dir(&self) -> PathBuf {
        self.root.join("tools")
    }

    pub fn ensure(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(self.volumes_dir())?;
        std::fs::create_dir_all(self.index_dir())?;
        std::fs::create_dir_all(self.tools_dir())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_catalogue_moves_out_of_the_program_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let legacy = tmp.path().join("Storecat");
        let root = tmp.path().join("io.storecat.app");
        std::fs::create_dir_all(legacy.join("volumes")).unwrap();
        std::fs::create_dir_all(root.join("EBWebView")).unwrap();
        for f in [
            "master.db",
            "master.db-wal",
            "volumes/1.db",
            "storecat-desktop.exe",
        ] {
            std::fs::write(legacy.join(f), f).unwrap();
        }

        assert_eq!(migrate_legacy(&legacy, &root), root);
        assert_eq!(
            std::fs::read_to_string(root.join("master.db-wal")).unwrap(),
            "master.db-wal"
        );
        assert!(root.join("volumes/1.db").exists());
        assert!(root.join("EBWebView").exists());
        // The program's own files stay where the installer put them.
        assert!(legacy.join("storecat-desktop.exe").exists());
        assert!(!legacy.join("master.db").exists());

        // Second start: nothing left to move, the new folder is used.
        assert_eq!(migrate_legacy(&legacy, &root), root);
    }

    #[test]
    fn system_drive_is_recognised() {
        #[cfg(windows)]
        {
            let sys = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
            assert!(is_system_drive(&format!("{sys}\\")));
            assert!(is_system_drive(&sys.to_lowercase()));
            assert!(!is_system_drive("Q:\\"));
            assert!(!is_system_drive(&format!("{sys}\\Users")));
        }
        #[cfg(not(windows))]
        {
            assert!(is_system_drive("/"));
            assert!(!is_system_drive("/media/backup"));
        }
    }

    #[test]
    fn existing_catalogue_is_never_overwritten() {
        let tmp = tempfile::tempdir().unwrap();
        let legacy = tmp.path().join("Storecat");
        let root = tmp.path().join("io.storecat.app");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(legacy.join("master.db"), "old").unwrap();
        std::fs::write(root.join("master.db"), "new").unwrap();

        assert_eq!(migrate_legacy(&legacy, &root), root);
        assert_eq!(
            std::fs::read_to_string(root.join("master.db")).unwrap(),
            "new"
        );
        assert_eq!(
            std::fs::read_to_string(legacy.join("master.db")).unwrap(),
            "old"
        );
    }

    #[test]
    fn failed_move_rolls_back_to_the_old_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let legacy = tmp.path().join("Storecat");
        let root = tmp.path().join("io.storecat.app");
        std::fs::create_dir_all(legacy.join("index")).unwrap();
        std::fs::write(legacy.join("master.db"), "db").unwrap();
        std::fs::write(legacy.join("index/seg"), "seg").unwrap();
        // A file where the index folder should go makes that rename fail.
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("index"), "in the way").unwrap();

        assert_eq!(migrate_legacy(&legacy, &root), legacy);
        assert!(legacy.join("master.db").exists(), "moved items go back");
        assert!(legacy.join("index/seg").exists());
        assert!(!root.join("master.db").exists());
    }
}

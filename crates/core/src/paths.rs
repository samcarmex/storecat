//! Where Storecat keeps its data.
//!
//! Default: `%LOCALAPPDATA%\Storecat` on Windows, `~/Library/Application Support/Storecat`
//! on macOS, `$XDG_DATA_HOME/Storecat` on Linux. `STORECAT_DATA_DIR` overrides it, which
//! the CLI, tests and portable installs rely on.

use std::path::{Path, PathBuf};

pub const ENV_DATA_DIR: &str = "STORECAT_DATA_DIR";

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
        Self {
            root: base.join("Storecat"),
        }
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

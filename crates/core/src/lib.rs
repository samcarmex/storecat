//! Storecat core: the OS-agnostic catalogue, scanner, search and operations engine.
//!
//! Everything in here must stay free of platform-specific code (beyond `cfg` for file
//! metadata); volume detection and drive hardware live in `storecat-platform`.

pub mod catalog;
pub mod db;
pub mod enrich;
pub mod error;
pub mod index;
pub mod kind;
pub mod live;
pub mod model;
pub mod ops;
pub mod paths;
pub mod places;
pub mod scan;
pub mod storage;
pub mod suggest;
pub mod tools;

pub use catalog::{settings, Catalog};
pub use enrich::EnrichOptions;
pub use error::{Error, Result};
pub use live::{Change, LiveStats};
pub use model::*;
pub use paths::DataDirs;
pub use scan::ScanOptions;
pub use storecat_extract::FileMeta;

use serde::Serialize;

/// Version of the core library, reported by the desktop app and the CLI.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Build information surfaced in the About screen and `storecat --version`.
#[derive(Debug, Clone, Serialize)]
pub struct BuildInfo {
    pub core_version: &'static str,
    pub profile: &'static str,
}

/// Returns static build information.
pub fn build_info() -> BuildInfo {
    BuildInfo {
        core_version: VERSION,
        profile: if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_events_have_a_flat_phase_field() {
        let p = EnrichProgress {
            volume_id: 1,
            phase: EnrichPhase::Failed {
                message: "x".into(),
            },
            pass: 1,
            done: 0,
            total: 0,
            hashed: 0,
            metas: 0,
            thumbs: 0,
            texts: 0,
            archives: 0,
            elapsed_ms: 0,
            current: String::new(),
        };
        let v: serde_json::Value = serde_json::to_value(&p).unwrap();
        assert_eq!(v["phase"], "failed");
        assert_eq!(v["message"], "x");
        assert_eq!(v["pass"], 1);
        let s = ScanProgress {
            volume_id: 1,
            phase: ScanPhase::Walking,
            files: 0,
            dirs: 0,
            bytes: 0,
            errors: 0,
            elapsed_ms: 0,
            current: String::new(),
            indexed: 0,
            index_total: 0,
        };
        assert_eq!(serde_json::to_value(&s).unwrap()["phase"], "walking");
    }

    #[test]
    fn build_info_reports_crate_version() {
        assert_eq!(build_info().core_version, env!("CARGO_PKG_VERSION"));
    }
}

//! Local diagnostics: a small rolling log file and crash reports.
//!
//! Nothing here ever leaves the computer. Logs go to `<data>/logs/storecat.log` (rotated at
//! 5 MB, one previous file kept); a panic writes `<data>/logs/crash-<time>.txt` with the
//! message, location and backtrace. Reporting a problem means the user chooses to attach
//! one of these files to an issue. "Detailed log" in Settings raises the level to debug.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use log::{Level, LevelFilter, Log, Metadata, Record};

const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;
const MAX_CRASH_FILES: usize = 20;
pub const VERBOSE_LOG: &str = "verbose_log";

struct FileLogger {
    file: Mutex<Option<File>>,
}

static LOGGER: FileLogger = FileLogger {
    file: Mutex::new(None),
};

impl Log for FileLogger {
    fn enabled(&self, m: &Metadata) -> bool {
        // Keep dependency chatter (tantivy, tao, wry...) out unless it is a warning.
        // tantivy warns once per over-long token in extracted document text; that is
        // expected (base64 blobs, minified data) and would flood the log.
        m.level() <= log::max_level()
            && (m.level() <= Level::Warn || m.target().starts_with("storecat"))
            && !(m.level() == Level::Warn && m.target().starts_with("tantivy::postings"))
    }

    fn log(&self, r: &Record) {
        if !self.enabled(r.metadata()) {
            return;
        }
        let line = format!(
            "{} {:<5} {}: {}\n",
            stamp(),
            r.level(),
            r.target(),
            r.args()
        );
        if cfg!(debug_assertions) {
            eprint!("{line}");
        }
        if let Ok(mut f) = self.file.lock() {
            if let Some(f) = f.as_mut() {
                let _ = f.write_all(line.as_bytes());
            }
        }
    }

    fn flush(&self) {
        if let Ok(mut f) = self.file.lock() {
            if let Some(f) = f.as_mut() {
                let _ = f.flush();
            }
        }
    }
}

fn stamp() -> String {
    let now = time::OffsetDateTime::now_utc();
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}Z",
        now.year(),
        now.month() as u8,
        now.day(),
        now.hour(),
        now.minute(),
        now.second()
    )
}

pub fn logs_dir(data_root: &Path) -> PathBuf {
    data_root.join("logs")
}

/// Start logging to the data folder and install the crash handler. Call once, first.
pub fn init(data_root: &Path) {
    let dir = logs_dir(data_root);
    let _ = fs::create_dir_all(&dir);
    let path = dir.join("storecat.log");
    if fs::metadata(&path)
        .map(|m| m.len() > MAX_LOG_BYTES)
        .unwrap_or(false)
    {
        let _ = fs::rename(&path, dir.join("storecat.log.1"));
    }
    if let Ok(f) = OpenOptions::new().create(true).append(true).open(&path) {
        *LOGGER.file.lock().unwrap_or_else(|e| e.into_inner()) = Some(f);
    }
    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(LevelFilter::Info);
    }
    install_panic_hook(dir);
    log::info!(
        "Storecat {} starting on {}",
        env!("CARGO_PKG_VERSION"),
        storecat_platform::name()
    );
}

pub fn set_verbose(verbose: bool) {
    log::set_max_level(if verbose {
        LevelFilter::Debug
    } else {
        LevelFilter::Info
    });
}

fn install_panic_hook(dir: PathBuf) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let report = format!(
            "Storecat {} crash report\nwhen: {}\nplatform: {} {}\nthread: {}\n\n{}\n\nbacktrace:\n{}\n",
            env!("CARGO_PKG_VERSION"),
            stamp(),
            storecat_platform::name(),
            std::env::consts::ARCH,
            thread.name().unwrap_or("unnamed"),
            info,
            std::backtrace::Backtrace::force_capture()
        );
        let name = format!("crash-{}.txt", stamp().replace([':', ' '], "-"));
        if let Ok(mut f) = File::create(dir.join(name)) {
            let _ = f.write_all(report.as_bytes());
        }
        prune_crashes(&dir);
        log::error!("panic: {info}");
        LOGGER.flush();
        previous(info);
    }));
}

/// Keep the newest crash reports only.
fn prune_crashes(dir: &Path) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    let mut crashes: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("crash-"))
        })
        .collect();
    crashes.sort();
    while crashes.len() > MAX_CRASH_FILES {
        let _ = fs::remove_file(crashes.remove(0));
    }
}

/// Crash reports present right now, newest first.
pub fn crash_reports(data_root: &Path) -> Vec<String> {
    let dir = logs_dir(data_root);
    let Ok(rd) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut v: Vec<String> = rd
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|n| n.starts_with("crash-"))
        .collect();
    v.sort();
    v.reverse();
    v
}

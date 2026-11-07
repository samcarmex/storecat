//! Watches for drives coming and going. Polls the platform layer every two seconds
//! (cheap: one volume enumeration) and tells the UI when anything changed. A catalogued
//! drive that reappears while the app runs is rescanned silently, as the user chose, and
//! any volume with unfinished enrichment is resumed when the app starts.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use storecat_core::{now_unix, settings, Catalog};
use tauri::{AppHandle, Emitter};

const POLL: Duration = Duration::from_secs(2);
/// Don't rescan a drive that was scanned this recently (e.g. a quick unplug/replug).
const RESCAN_MIN_AGE_SECS: i64 = 10 * 60;

/// Folder volumes (network shares especially) are checked on their own, slower loop so
/// a share that times out never delays drive detection.
const FOLDER_POLL: Duration = Duration::from_secs(15);

pub fn start(app: AppHandle, catalog: Arc<Catalog>) {
    let (a, c) = (app.clone(), Arc::clone(&catalog));
    std::thread::Builder::new()
        .name("storecat-watcher".into())
        .spawn(move || run(app, catalog))
        .expect("spawn watcher thread");
    std::thread::Builder::new()
        .name("storecat-folders".into())
        .spawn(move || watch_folders(a, c))
        .expect("spawn folder watcher thread");
}

fn watch_folders(app: AppHandle, catalog: Arc<Catalog>) {
    loop {
        match catalog.refresh_folders(&storecat_platform::is_remote_path) {
            Ok(true) => {
                let _ = app.emit("volumes-changed", ());
            }
            Ok(false) => {}
            Err(e) => log::warn!("folder refresh failed: {e}"),
        }
        std::thread::sleep(FOLDER_POLL);
    }
}

fn run(app: AppHandle, catalog: Arc<Catalog>) {
    let mut seen: Option<HashSet<String>> = None;
    loop {
        let detected = storecat_platform::detect_volumes();
        let now_ids: HashSet<String> = detected.iter().map(|d| d.identity.clone()).collect();
        match catalog.refresh_detected(detected) {
            Ok(changed) => {
                if changed || seen.is_none() {
                    let _ = app.emit("volumes-changed", ());
                }
                match &seen {
                    Some(prev) => {
                        if catalog.setting_enabled(settings::AUTO_RESCAN, true) {
                            for identity in now_ids.difference(prev) {
                                maybe_rescan(&app, &catalog, identity);
                            }
                        }
                    }
                    None => resume_enrichment(&app, &catalog),
                }
                seen = Some(now_ids);
            }
            Err(e) => log::warn!("volume refresh failed: {e}"),
        }
        std::thread::sleep(POLL);
    }
}

fn maybe_rescan(app: &AppHandle, catalog: &Arc<Catalog>, identity: &str) {
    let Ok(volumes) = catalog.volumes() else {
        return;
    };
    let Some(v) = volumes.into_iter().find(|v| v.identity == identity) else {
        return;
    };
    let stale = v
        .last_scan_at
        .map(|t| now_unix() - t > RESCAN_MIN_AGE_SECS)
        .unwrap_or(true);
    if stale && !catalog.is_scanning(v.id) {
        log::info!("volume \"{}\" reconnected, rescanning", v.label);
        crate::commands::spawn_scan(app.clone(), Arc::clone(catalog), v.id);
    } else if catalog.setting_enabled(settings::AUTO_ENRICH, true) {
        if let Ok(c) = catalog.enrich_coverage(v.id) {
            if c.done < c.files {
                crate::commands::spawn_enrich(app.clone(), Arc::clone(catalog), v.id);
            }
        }
    }
}

/// On startup: pick up enrichment that was interrupted last time, for connected drives.
fn resume_enrichment(app: &AppHandle, catalog: &Arc<Catalog>) {
    if !catalog.setting_enabled(settings::AUTO_ENRICH, true) {
        return;
    }
    let Ok(volumes) = catalog.volumes() else {
        return;
    };
    for v in volumes
        .into_iter()
        .filter(|v| v.online && v.last_scan_at.is_some())
    {
        if let Ok(c) = catalog.enrich_coverage(v.id) {
            if c.done < c.files {
                log::info!(
                    "resuming enrichment of \"{}\" ({} of {} done)",
                    v.label,
                    c.done,
                    c.files
                );
                crate::commands::spawn_enrich(app.clone(), Arc::clone(catalog), v.id);
            }
        }
    }
}

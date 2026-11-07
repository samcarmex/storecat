//! Watches for drives coming and going. Polls the platform layer every two seconds
//! (cheap: one volume enumeration) and tells the UI when anything changed. A catalogued
//! drive that reappears while the app runs is rescanned silently, as the user chose. When
//! the app starts, scans that were cut off are restarted and unfinished enrichment resumes
//! (for drives that are connected; the others catch up when they are plugged in).

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
                        for identity in now_ids.difference(prev) {
                            maybe_rescan(&app, &catalog, identity);
                        }
                    }
                    None => {
                        catalog.repair_indexes();
                        resume_interrupted(&app, &catalog);
                    }
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
    // A scan cut off by unplugging always restarts; routine rescans follow the setting.
    let cut_off = catalog.scan_interrupted(v.id);
    let rescan = cut_off || (stale && catalog.setting_enabled(settings::AUTO_RESCAN, true));
    if rescan && !catalog.is_scanning(v.id) {
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

/// On startup: restart scans and pick up enrichment that were interrupted last time, for
/// connected drives. A cut-off scan restarts from the beginning (it is the quick part);
/// enrichment carries on from the last file it saved.
fn resume_interrupted(app: &AppHandle, catalog: &Arc<Catalog>) {
    let Ok(volumes) = catalog.volumes() else {
        return;
    };
    let auto_enrich = catalog.setting_enabled(settings::AUTO_ENRICH, true);
    for v in volumes.into_iter().filter(|v| v.online) {
        if catalog.scan_interrupted(v.id) {
            log::info!("restarting the interrupted scan of \"{}\"", v.label);
            crate::commands::spawn_scan(app.clone(), Arc::clone(catalog), v.id);
            continue;
        }
        if !auto_enrich || v.last_scan_at.is_none() {
            continue;
        }
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

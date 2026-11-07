//! IPC surface. Every command is a thin call into `Catalog`; errors become strings for
//! the frontend to display.

use std::path::Path;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use storecat_core::suggest::{RuleSettings, Suggestion};
use storecat_core::{
    settings, Catalog, CatalogStats, DetectedVolume, DownloadProgress, DupGroup, DupPage, DupPick,
    DupQuery, DupSummary, EnrichCoverage, EnrichOptions, EnrichPhase, EnrichProgress, FfmpegStatus,
    FolderTree, ItemDetail, ItemStatus, ItemType, Job, JobItem, JobKind, JobPlan, JobSpec,
    KindBytes, SavedSearch, ScanOptions, SearchOptions, SearchResponse, Tag, Volume, VolumePatch,
};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;

use crate::AppState;

type CmdResult<T> = Result<T, String>;

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

#[derive(Serialize)]
pub struct BuildInfo {
    app_version: &'static str,
    core_version: &'static str,
    profile: &'static str,
    platform: &'static str,
    data_dir: String,
    index_docs: u64,
    extractors: &'static [&'static str],
}

#[tauri::command]
pub fn build_info(state: State<AppState>) -> BuildInfo {
    let core = storecat_core::build_info();
    BuildInfo {
        app_version: env!("CARGO_PKG_VERSION"),
        core_version: core.core_version,
        profile: core.profile,
        platform: storecat_platform::name(),
        data_dir: state.catalog.dirs.root.to_string_lossy().into_owned(),
        index_docs: state.catalog.index_doc_count(),
        extractors: storecat_extract::available(),
    }
}

#[tauri::command]
pub fn list_volumes(state: State<AppState>) -> CmdResult<Vec<Volume>> {
    state.catalog.volumes().map_err(err)
}

#[tauri::command]
pub fn list_detected(state: State<AppState>) -> CmdResult<Vec<DetectedVolume>> {
    state.catalog.detected_uncatalogued().map_err(err)
}

#[tauri::command]
pub fn catalog_stats(state: State<AppState>) -> CmdResult<CatalogStats> {
    state.catalog.stats().map_err(err)
}

#[tauri::command]
pub fn add_detected(app: AppHandle, state: State<AppState>, identity: String) -> CmdResult<Volume> {
    let v = state.catalog.add_detected(&identity).map_err(err)?;
    let _ = app.emit("volumes-changed", ());
    Ok(v)
}

#[tauri::command]
pub fn add_folder(
    app: AppHandle,
    state: State<AppState>,
    path: String,
    label: Option<String>,
) -> CmdResult<Volume> {
    let v = state
        .catalog
        .add_folder(Path::new(&path), label.as_deref())
        .map_err(err)?;
    let _ = app.emit("volumes-changed", ());
    refresh_folders_soon(&app);
    Ok(v)
}

#[tauri::command]
pub async fn pick_and_add_folder(app: AppHandle) -> CmdResult<Option<Volume>> {
    let picked = app
        .dialog()
        .file()
        .set_title("Choose a folder or network share to catalogue")
        .blocking_pick_folder();
    let Some(path) = picked else {
        return Ok(None);
    };
    let path = path.into_path().map_err(err)?;
    let state = app.state::<AppState>();
    let v = state.catalog.add_folder(&path, None).map_err(err)?;
    let _ = app.emit("volumes-changed", ());
    refresh_folders_soon(&app);
    Ok(Some(v))
}

// ----- scanning and enrichment ---------------------------------------------------------

#[tauri::command]
pub fn start_scan(app: AppHandle, state: State<AppState>, volume_id: i64) -> CmdResult<()> {
    spawn_scan(app, Arc::clone(&state.catalog), volume_id);
    Ok(())
}

pub fn spawn_scan(app: AppHandle, catalog: Arc<Catalog>, volume_id: i64) {
    if catalog.is_scanning(volume_id) {
        return;
    }
    std::thread::Builder::new()
        .name(format!("storecat-scan-{volume_id}"))
        .spawn(move || {
            let emitter = app.clone();
            let progress = move |p: storecat_core::ScanProgress| {
                let _ = emitter.emit("scan-progress", &p);
            };
            let ok = match catalog.scan(volume_id, &ScanOptions::default(), &progress) {
                Ok(stats) => {
                    log::info!(
                        "scan of volume {volume_id} done: {} files in {} ms",
                        stats.files,
                        stats.elapsed_ms
                    );
                    true
                }
                Err(e) => {
                    log::warn!("scan of volume {volume_id} failed: {e}");
                    false
                }
            };
            let _ = app.emit("volumes-changed", ());
            if ok && catalog.setting_enabled(settings::AUTO_ENRICH, true) {
                spawn_enrich(app, catalog, volume_id);
            }
        })
        .expect("spawn scan thread");
}

/// Enrichment runs one volume at a time: it is IO and CPU heavy and the user is probably
/// still using the machine. Volumes waiting for their turn are listed in `ENRICH_QUEUE`;
/// pausing one takes it out, and its thread then gives up when the gate opens.
static ENRICH_GATE: Mutex<()> = Mutex::new(());
static ENRICH_QUEUE: Mutex<Vec<i64>> = Mutex::new(Vec::new());

fn queue() -> std::sync::MutexGuard<'static, Vec<i64>> {
    ENRICH_QUEUE.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn spawn_enrich(app: AppHandle, catalog: Arc<Catalog>, volume_id: i64) {
    if catalog.is_enriching(volume_id) {
        return;
    }
    {
        let mut q = queue();
        if q.contains(&volume_id) {
            return;
        }
        q.push(volume_id);
    }
    std::thread::Builder::new()
        .name(format!("storecat-enrich-{volume_id}"))
        .spawn(move || {
            let _gate = match ENRICH_GATE.try_lock() {
                Ok(g) => g,
                Err(_) => {
                    let _ = app.emit(
                        "enrich-progress",
                        EnrichProgress::bare(volume_id, EnrichPhase::Queued),
                    );
                    ENRICH_GATE.lock().unwrap_or_else(|e| e.into_inner())
                }
            };
            {
                let mut q = queue();
                let Some(at) = q.iter().position(|&v| v == volume_id) else {
                    return; // paused while queued
                };
                q.remove(at);
            }
            if catalog.is_enriching(volume_id) || catalog.is_scanning(volume_id) {
                return;
            }
            let emitter = app.clone();
            let progress = move |p: EnrichProgress| {
                let _ = emitter.emit("enrich-progress", &p);
            };
            let opts = EnrichOptions {
                hash_min_bytes: catalog.hash_min_bytes(),
                ..Default::default()
            };
            match catalog.enrich(volume_id, &opts, &progress) {
                Ok(s) => log::info!(
                    "enrichment of volume {volume_id} {}: {} files, {} thumbs, {} texts in {} ms",
                    if s.paused { "paused" } else { "done" },
                    s.processed,
                    s.thumbs,
                    s.texts,
                    s.elapsed_ms
                ),
                Err(e) => log::warn!("enrichment of volume {volume_id} failed: {e}"),
            }
            let _ = app.emit("volumes-changed", ());
        })
        .expect("spawn enrich thread");
}

#[tauri::command]
pub fn start_enrich(app: AppHandle, state: State<AppState>, volume_id: i64) -> CmdResult<()> {
    spawn_enrich(app, Arc::clone(&state.catalog), volume_id);
    Ok(())
}

#[tauri::command]
pub fn cancel_enrich(app: AppHandle, state: State<AppState>, volume_id: i64) {
    let was_queued = {
        let mut q = queue();
        let before = q.len();
        q.retain(|&v| v != volume_id);
        q.len() != before
    };
    if was_queued {
        let _ = app.emit(
            "enrich-progress",
            EnrichProgress::bare(volume_id, EnrichPhase::Paused),
        );
    }
    state.catalog.cancel_enrich(volume_id);
}

#[tauri::command]
pub fn enriching(state: State<AppState>) -> Vec<i64> {
    state.catalog.enriching_ids()
}

/// Forget every queued enrichment (when quitting); their threads give up when they wake.
pub fn clear_enrich_queue() {
    queue().clear();
}

/// Volumes waiting for another drive's enrichment to finish, in order.
#[tauri::command]
pub fn enrich_queued() -> Vec<i64> {
    queue().clone()
}

#[tauri::command]
pub fn scanning(state: State<AppState>) -> Vec<i64> {
    state.catalog.scanning_ids()
}

#[tauri::command]
pub fn enrich_coverage(state: State<AppState>, volume_id: i64) -> CmdResult<EnrichCoverage> {
    state.catalog.enrich_coverage(volume_id).map_err(err)
}

#[tauri::command]
pub fn reset_enrichment(
    app: AppHandle,
    state: State<AppState>,
    volume_id: i64,
    keep_hashes: bool,
) -> CmdResult<()> {
    state
        .catalog
        .reset_enrichment(volume_id, keep_hashes)
        .map_err(err)?;
    let _ = app.emit("volumes-changed", ());
    Ok(())
}

#[tauri::command]
pub fn remove_volume(app: AppHandle, state: State<AppState>, volume_id: i64) -> CmdResult<()> {
    state.catalog.remove_volume(volume_id).map_err(err)?;
    let _ = app.emit("volumes-changed", ());
    Ok(())
}

#[tauri::command]
pub fn update_volume(
    app: AppHandle,
    state: State<AppState>,
    volume_id: i64,
    patch: VolumePatch,
) -> CmdResult<Volume> {
    let v = state
        .catalog
        .update_volume(volume_id, &patch)
        .map_err(err)?;
    let _ = app.emit("volumes-changed", ());
    Ok(v)
}

// ----- search and items -----------------------------------------------------------------

#[tauri::command]
pub fn search(
    state: State<AppState>,
    query: String,
    options: Option<SearchOptions>,
) -> CmdResult<SearchResponse> {
    state
        .catalog
        .search(&query, &options.unwrap_or_default())
        .map_err(err)
}

#[tauri::command]
pub fn item_detail(
    state: State<AppState>,
    volume_id: i64,
    item_id: i64,
    item_type: ItemType,
) -> CmdResult<ItemDetail> {
    state
        .catalog
        .item_detail(volume_id, item_id, item_type)
        .map_err(err)
}

#[tauri::command]
pub fn open_item(
    app: AppHandle,
    state: State<AppState>,
    volume_id: i64,
    item_id: i64,
    item_type: ItemType,
) -> CmdResult<()> {
    let path = state
        .catalog
        .absolute_path(volume_id, item_id, item_type)
        .map_err(err)?;
    app.opener()
        .open_path(path.to_string_lossy(), None::<&str>)
        .map_err(err)
}

#[tauri::command]
pub fn reveal_item(
    app: AppHandle,
    state: State<AppState>,
    volume_id: i64,
    item_id: i64,
    item_type: ItemType,
) -> CmdResult<()> {
    let path = state
        .catalog
        .absolute_path(volume_id, item_id, item_type)
        .map_err(err)?;
    app.opener().reveal_item_in_dir(path).map_err(err)
}

// ----- duplicates ----------------------------------------------------------------------

#[tauri::command]
pub async fn duplicates(app: AppHandle, query: DupQuery) -> CmdResult<DupPage> {
    let catalog = Arc::clone(&app.state::<AppState>().catalog);
    tauri::async_runtime::spawn_blocking(move || catalog.duplicate_page(&query).map_err(err))
        .await
        .map_err(err)?
}

#[tauri::command]
pub fn duplicate_summary(state: State<AppState>) -> CmdResult<DupSummary> {
    state.catalog.duplicate_summary().map_err(err)
}

#[tauri::command]
pub async fn verify_group(app: AppHandle, hash: String) -> CmdResult<DupGroup> {
    let catalog = Arc::clone(&app.state::<AppState>().catalog);
    tauri::async_runtime::spawn_blocking(move || catalog.verify_group(&hash).map_err(err))
        .await
        .map_err(err)?
}

// ----- tools, settings, export/import ---------------------------------------------------

#[tauri::command]
pub fn ffmpeg_status(state: State<AppState>) -> FfmpegStatus {
    state.catalog.ffmpeg_status()
}

#[tauri::command]
pub async fn download_ffmpeg(app: AppHandle) -> CmdResult<FfmpegStatus> {
    let catalog = Arc::clone(&app.state::<AppState>().catalog);
    let emitter = app.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let progress = |downloaded: u64, total: Option<u64>| {
            let _ = emitter.emit(
                "tool-download",
                &DownloadProgress {
                    tool: "ffmpeg".into(),
                    downloaded,
                    total,
                    done: false,
                    error: None,
                },
            );
        };
        storecat_core::tools::download_ffmpeg(&catalog.dirs, &progress).map_err(err)
    })
    .await
    .map_err(err)?;
    let _ = app.emit(
        "tool-download",
        &DownloadProgress {
            tool: "ffmpeg".into(),
            downloaded: 0,
            total: None,
            done: true,
            error: result.as_ref().err().cloned(),
        },
    );
    result?;
    Ok(app.state::<AppState>().catalog.ffmpeg_status())
}

#[tauri::command]
pub fn get_setting(state: State<AppState>, key: String) -> CmdResult<Option<String>> {
    state.catalog.setting(&key).map_err(err)
}

#[tauri::command]
pub fn set_setting(state: State<AppState>, key: String, value: String) -> CmdResult<()> {
    state.catalog.set_setting(&key, &value).map_err(err)
}

#[tauri::command]
pub async fn export_volume(app: AppHandle, volume_id: i64) -> CmdResult<Option<String>> {
    let picked = app
        .dialog()
        .file()
        .set_title("Choose where to save the catalogue")
        .blocking_pick_folder();
    let Some(dir) = picked else { return Ok(None) };
    let dir = dir.into_path().map_err(err)?;
    let catalog = Arc::clone(&app.state::<AppState>().catalog);
    let p = catalog.export_volume(volume_id, &dir).map_err(err)?;
    Ok(Some(p.to_string_lossy().into_owned()))
}

#[tauri::command]
pub async fn import_volume(app: AppHandle) -> CmdResult<Option<Volume>> {
    let picked = app
        .dialog()
        .file()
        .set_title("Choose a Storecat catalogue (.db)")
        .add_filter("Storecat catalogue", &["db"])
        .blocking_pick_file();
    let Some(file) = picked else { return Ok(None) };
    let file = file.into_path().map_err(err)?;
    let catalog = Arc::clone(&app.state::<AppState>().catalog);
    let v = tauri::async_runtime::spawn_blocking(move || catalog.import_volume(&file).map_err(err))
        .await
        .map_err(err)??;
    let _ = app.emit("volumes-changed", ());
    Ok(Some(v))
}

// ----- Phase 3: jobs ----------------------------------------------------------------------

#[tauri::command]
pub fn should_show_main(state: State<AppState>) -> bool {
    !state
        .start_hidden
        .load(std::sync::atomic::Ordering::Relaxed)
}

#[tauri::command]
pub fn plan_job(state: State<AppState>, spec: JobSpec) -> CmdResult<JobPlan> {
    state.catalog.plan_job(&spec).map_err(err)
}

#[tauri::command]
pub fn create_job(
    app: AppHandle,
    state: State<AppState>,
    spec: JobSpec,
    start: bool,
) -> CmdResult<Job> {
    let job = state.catalog.create_job(&spec).map_err(err)?;
    if start {
        spawn_job(app.clone(), Arc::clone(&state.catalog), job.id);
    }
    let _ = app.emit("jobs-changed", ());
    Ok(job)
}

pub fn spawn_job(app: AppHandle, catalog: Arc<Catalog>, job_id: i64) {
    if catalog.is_job_running(job_id) {
        return;
    }
    std::thread::Builder::new()
        .name(format!("storecat-job-{job_id}"))
        .spawn(move || {
            let emitter = app.clone();
            let progress = move |p: storecat_core::JobProgress| {
                let _ = emitter.emit("job-progress", &p);
            };
            let result = catalog.run_job(job_id, &progress);
            let _ = app.emit("jobs-changed", ());
            match result {
                Ok(job) => {
                    log::info!("job {job_id} finished: {:?}", job.status);
                    // Keep the catalogue honest: rescan the drives the job touched.
                    if matches!(
                        job.status,
                        storecat_core::JobStatus::Done | storecat_core::JobStatus::Failed
                    ) {
                        let mut ids = Vec::new();
                        if let Some(v) = job.dest_volume_id {
                            ids.push(v);
                        }
                        if job.kind != storecat_core::JobKind::Copy {
                            if let Some(v) = job.source_volume_id {
                                ids.push(v);
                            }
                        }
                        ids.dedup();
                        for id in ids {
                            spawn_scan(app.clone(), Arc::clone(&catalog), id);
                        }
                    }
                }
                Err(e) => log::warn!("job {job_id} failed: {e}"),
            }
        })
        .expect("spawn job thread");
}

#[tauri::command]
pub fn run_job(app: AppHandle, state: State<AppState>, job_id: i64) -> CmdResult<()> {
    spawn_job(app, Arc::clone(&state.catalog), job_id);
    Ok(())
}

#[tauri::command]
pub fn pause_job(state: State<AppState>, job_id: i64) {
    state.catalog.pause_job(job_id);
}

#[tauri::command]
pub fn cancel_job(app: AppHandle, state: State<AppState>, job_id: i64) -> CmdResult<()> {
    state.catalog.cancel_job(job_id).map_err(err)?;
    let _ = app.emit("jobs-changed", ());
    Ok(())
}

#[tauri::command]
pub fn delete_job(app: AppHandle, state: State<AppState>, job_id: i64) -> CmdResult<()> {
    state.catalog.delete_job(job_id).map_err(err)?;
    let _ = app.emit("jobs-changed", ());
    Ok(())
}

#[tauri::command]
pub fn jobs(state: State<AppState>) -> CmdResult<Vec<Job>> {
    state.catalog.jobs().map_err(err)
}

#[tauri::command]
pub fn job_items(
    state: State<AppState>,
    job_id: i64,
    status: Option<ItemStatus>,
    limit: Option<usize>,
) -> CmdResult<Vec<JobItem>> {
    state
        .catalog
        .job_items(job_id, status, limit.unwrap_or(500))
        .map_err(err)
}

#[tauri::command]
pub fn running_jobs(state: State<AppState>) -> Vec<i64> {
    state.catalog.running_job_ids()
}

#[tauri::command]
pub fn sweep_spec(state: State<AppState>, dest_mount: String, kind: JobKind) -> JobSpec {
    state.catalog.sweep_spec(&dest_mount, kind)
}

#[tauri::command]
pub async fn pick_folders(app: AppHandle) -> CmdResult<Vec<String>> {
    let picked = app
        .dialog()
        .file()
        .set_title("Choose folders to back up")
        .blocking_pick_folders();
    Ok(picked
        .unwrap_or_default()
        .into_iter()
        .filter_map(|p| p.into_path().ok())
        .map(|p| p.to_string_lossy().into_owned())
        .collect())
}

#[tauri::command]
pub async fn pick_files(app: AppHandle) -> CmdResult<Vec<String>> {
    let picked = app
        .dialog()
        .file()
        .set_title("Choose files to back up")
        .blocking_pick_files();
    Ok(picked
        .unwrap_or_default()
        .into_iter()
        .filter_map(|p| p.into_path().ok())
        .map(|p| p.to_string_lossy().into_owned())
        .collect())
}

/// Keep one copy in each picked duplicate group and recycle the other copies of yours.
/// Creates and starts one job.
#[tauri::command]
pub async fn duplicate_cleanup(app: AppHandle, picks: Vec<DupPick>) -> CmdResult<Job> {
    let catalog = Arc::clone(&app.state::<AppState>().catalog);
    let cat = Arc::clone(&catalog);
    let job = tauri::async_runtime::spawn_blocking(move || -> CmdResult<Job> {
        let spec = cat.duplicate_cleanup_spec(&picks).map_err(err)?;
        cat.create_job(&spec).map_err(err)
    })
    .await
    .map_err(err)??;
    spawn_job(app.clone(), catalog, job.id);
    let _ = app.emit("jobs-changed", ());
    Ok(job)
}

// ----- Phase 3: tags, saved searches, photos ------------------------------------------------

#[tauri::command]
pub fn tags(state: State<AppState>) -> CmdResult<Vec<Tag>> {
    state.catalog.tags().map_err(err)
}

#[tauri::command]
pub fn tag_item(
    state: State<AppState>,
    volume_id: i64,
    item_id: i64,
    item_type: ItemType,
    tag: String,
) -> CmdResult<Vec<String>> {
    state
        .catalog
        .tag_item(volume_id, item_id, item_type, &tag)
        .map_err(err)
}

#[tauri::command]
pub fn untag_item(
    state: State<AppState>,
    volume_id: i64,
    item_id: i64,
    item_type: ItemType,
    tag: String,
) -> CmdResult<Vec<String>> {
    state
        .catalog
        .untag_item(volume_id, item_id, item_type, &tag)
        .map_err(err)
}

#[tauri::command]
pub fn saved_searches(state: State<AppState>) -> CmdResult<Vec<SavedSearch>> {
    state.catalog.saved_searches().map_err(err)
}

#[tauri::command]
pub fn save_search(state: State<AppState>, name: String, query: String) -> CmdResult<SavedSearch> {
    state.catalog.save_search(&name, &query).map_err(err)
}

#[tauri::command]
pub fn delete_saved_search(state: State<AppState>, id: i64) -> CmdResult<()> {
    state.catalog.delete_saved_search(id).map_err(err)
}

#[tauri::command]
pub async fn set_volume_photo(app: AppHandle, volume_id: i64) -> CmdResult<bool> {
    let picked = app
        .dialog()
        .file()
        .set_title("Choose a photo of the drive")
        .add_filter(
            "Images",
            &[
                "jpg", "jpeg", "png", "webp", "heic", "bmp", "gif", "tif", "tiff",
            ],
        )
        .blocking_pick_file();
    let Some(file) = picked else { return Ok(false) };
    let file = file.into_path().map_err(err)?;
    let catalog = Arc::clone(&app.state::<AppState>().catalog);
    catalog.set_volume_photo(volume_id, &file).map_err(err)?;
    let _ = app.emit("volumes-changed", ());
    Ok(true)
}

#[tauri::command]
pub fn clear_volume_photo(app: AppHandle, state: State<AppState>, volume_id: i64) -> CmdResult<()> {
    state.catalog.clear_volume_photo(volume_id).map_err(err)?;
    let _ = app.emit("volumes-changed", ());
    Ok(())
}

// ----- Phase 3: tray, hotkey, autostart, palette ----------------------------------------------

#[derive(Serialize)]
pub struct ShellStatus {
    tray_mode: bool,
    hotkey_enabled: bool,
    hotkey: &'static str,
    autostart: bool,
}

#[tauri::command]
pub fn shell_status(app: AppHandle, state: State<AppState>) -> ShellStatus {
    use tauri_plugin_autostart::ManagerExt;
    ShellStatus {
        tray_mode: state
            .catalog
            .setting_enabled(crate::shell::TRAY_MODE, false),
        hotkey_enabled: state
            .catalog
            .setting_enabled(crate::shell::HOTKEY_ENABLED, false),
        hotkey: "Ctrl+Shift+Space",
        autostart: app.autolaunch().is_enabled().unwrap_or(false),
    }
}

#[tauri::command]
pub fn set_tray_mode(app: AppHandle, state: State<AppState>, enabled: bool) -> CmdResult<()> {
    state
        .catalog
        .set_setting(
            crate::shell::TRAY_MODE,
            if enabled { "true" } else { "false" },
        )
        .map_err(err)?;
    if enabled {
        crate::shell::build_tray(&app).map_err(err)?;
    } else {
        crate::shell::remove_tray(&app);
    }
    Ok(())
}

#[tauri::command]
pub fn set_hotkey_enabled(app: AppHandle, state: State<AppState>, enabled: bool) -> CmdResult<()> {
    state
        .catalog
        .set_setting(
            crate::shell::HOTKEY_ENABLED,
            if enabled { "true" } else { "false" },
        )
        .map_err(err)?;
    if enabled {
        crate::shell::register_hotkey(&app).map_err(err)?;
    } else {
        crate::shell::unregister_hotkey(&app);
    }
    Ok(())
}

#[tauri::command]
pub fn set_autostart(app: AppHandle, enabled: bool) -> CmdResult<bool> {
    use tauri_plugin_autostart::ManagerExt;
    let al = app.autolaunch();
    if enabled {
        al.enable().map_err(err)?;
    } else {
        al.disable().map_err(err)?;
    }
    Ok(al.is_enabled().unwrap_or(false))
}

#[tauri::command]
pub fn hide_palette(app: AppHandle) {
    crate::shell::hide_palette(&app);
}

#[tauri::command]
pub fn open_in_main(app: AppHandle, query: Option<String>) {
    crate::shell::show_main(&app, query);
}

#[tauri::command]
pub fn show_main(app: AppHandle) {
    crate::shell::show_main(&app, None);
}

#[tauri::command]
pub fn toggle_palette(app: AppHandle) {
    crate::shell::toggle_palette(&app);
}

// ----- storage overview, suggestions ------------------------------------------------------

/// Runs `f` on a blocking thread with the shared catalogue.
async fn blocking<T: Send + 'static>(
    app: &AppHandle,
    f: impl FnOnce(&Catalog) -> storecat_core::Result<T> + Send + 'static,
) -> CmdResult<T> {
    let catalog = Arc::clone(&app.state::<AppState>().catalog);
    tauri::async_runtime::spawn_blocking(move || f(&catalog).map_err(err))
        .await
        .map_err(err)?
}

#[tauri::command]
pub async fn folder_tree(
    app: AppHandle,
    volume_id: i64,
    dir_id: Option<i64>,
) -> CmdResult<FolderTree> {
    blocking(&app, move |c| c.folder_tree(volume_id, dir_id)).await
}

#[tauri::command]
pub async fn kind_breakdown(app: AppHandle, volume_id: i64) -> CmdResult<Vec<KindBytes>> {
    blocking(&app, move |c| c.kind_breakdown(volume_id)).await
}

#[tauri::command]
pub async fn suggestions(app: AppHandle) -> CmdResult<Vec<Suggestion>> {
    blocking(&app, |c| c.suggestions()).await
}

#[tauri::command]
pub fn dismiss_suggestion(state: State<AppState>, key: String, days: Option<u32>) -> CmdResult<()> {
    state.catalog.dismiss_suggestion(&key, days).map_err(err)
}

#[tauri::command]
pub fn reset_suggestions(state: State<AppState>) -> CmdResult<()> {
    state.catalog.reset_suggestions().map_err(err)
}

#[tauri::command]
pub fn rule_settings(state: State<AppState>) -> RuleSettings {
    state.catalog.rule_settings()
}

#[tauri::command]
pub fn set_rule_settings(state: State<AppState>, rules: RuleSettings) -> CmdResult<()> {
    state.catalog.set_rule_settings(&rules).map_err(err)
}

/// Re-check folder volumes (network shares included) right away, off the UI thread.
pub fn refresh_folders_soon(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        let catalog = Arc::clone(&app.state::<AppState>().catalog);
        if catalog
            .refresh_folders(&storecat_platform::is_remote_path)
            .is_ok()
        {
            let _ = app.emit("volumes-changed", ());
        }
    });
}

// ----- diagnostics -------------------------------------------------------------------------

#[derive(Serialize)]
pub struct DiagnosticsInfo {
    logs_dir: String,
    crash_reports: Vec<String>,
    verbose: bool,
}

#[tauri::command]
pub fn diagnostics_info(state: State<AppState>) -> DiagnosticsInfo {
    let root = &state.catalog.dirs.root;
    DiagnosticsInfo {
        logs_dir: crate::diagnostics::logs_dir(root)
            .to_string_lossy()
            .into_owned(),
        crash_reports: crate::diagnostics::crash_reports(root),
        verbose: state
            .catalog
            .setting_enabled(crate::diagnostics::VERBOSE_LOG, false),
    }
}

#[tauri::command]
pub fn open_logs_folder(app: AppHandle, state: State<AppState>) -> CmdResult<()> {
    let dir = crate::diagnostics::logs_dir(&state.catalog.dirs.root);
    std::fs::create_dir_all(&dir).map_err(err)?;
    app.opener()
        .open_path(dir.to_string_lossy(), None::<&str>)
        .map_err(err)
}

#[tauri::command]
pub fn set_verbose_log(state: State<AppState>, enabled: bool) -> CmdResult<()> {
    state
        .catalog
        .set_setting(crate::diagnostics::VERBOSE_LOG, &enabled.to_string())
        .map_err(err)?;
    crate::diagnostics::set_verbose(enabled);
    Ok(())
}

const PROJECT_URL: &str = "https://github.com/samcarmex/storecat";

/// Open the project's "new issue" page with version and platform filled in. Nothing is
/// attached automatically; the user decides whether to add a log or crash report.
#[tauri::command]
pub fn report_problem(app: AppHandle) -> CmdResult<()> {
    let body = format!(
        "**What happened?**\n\n\n**What did you expect?**\n\n\n---\nStorecat {} on {} ({})",
        env!("CARGO_PKG_VERSION"),
        storecat_platform::name(),
        std::env::consts::ARCH
    );
    let url = format!(
        "{PROJECT_URL}/issues/new?body={}",
        body.bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    (b as char).to_string()
                }
                _ => format!("%{b:02X}"),
            })
            .collect::<String>()
    );
    app.opener().open_url(url, None::<&str>).map_err(err)
}

/// Release notes for a version on GitHub.
#[tauri::command]
pub fn open_release_notes(app: AppHandle, version: String) -> CmdResult<()> {
    let tag: String = version
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '.' || *c == '-')
        .collect();
    app.opener()
        .open_url(format!("{PROJECT_URL}/releases/tag/v{tag}"), None::<&str>)
        .map_err(err)
}

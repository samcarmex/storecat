//! Tauri shell for Storecat. Keeps the window and IPC glue thin; all real work
//! lives in `storecat-core` so the CLI and the app share one engine.

mod commands;
mod diagnostics;
mod live;
mod shell;
mod watcher;

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use storecat_core::{Catalog, DataDirs};
use tauri::{Manager, WindowEvent};

pub struct AppState {
    pub catalog: Arc<Catalog>,
    /// Launched with `--hidden` (autostart) while tray mode is on: keep the main window
    /// out of the way until the user asks for it.
    pub start_hidden: AtomicBool,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let dirs = DataDirs::resolve();
    diagnostics::init(&dirs.root);

    tauri::Builder::default()
        // Must come first: a second launch (Start menu, autostart, the updater's relaunch)
        // brings the running window forward instead of failing on the locked catalogue.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            shell::show_main(app, None);
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--hidden"]),
        ))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state() == tauri_plugin_global_shortcut::ShortcutState::Pressed {
                        shell::toggle_palette(app);
                    }
                })
                .build(),
        )
        // thumb://localhost/<volume_id>-<file_id> serves JPEG thumbnails straight from the
        // per-volume thumbnail database; thumb://localhost/v-<volume_id> serves the drive photo.
        .register_uri_scheme_protocol("thumb", |ctx, request| {
            let state = ctx.app_handle().state::<AppState>();
            let path = request.uri().path().replace("%2F", "/");
            let mut parts = path.trim_start_matches('/').split(['-', '/']);
            let first = parts.next().unwrap_or("");
            let bytes = if first == "v" {
                parts
                    .next()
                    .and_then(|v| v.parse::<i64>().ok())
                    .and_then(|vid| state.catalog.volume_photo(vid))
            } else {
                match (
                    first.parse::<i64>().ok(),
                    parts.next().and_then(|f| f.parse::<i64>().ok()),
                ) {
                    (Some(vid), Some(fid)) => state.catalog.thumbnail(vid, fid).ok().flatten(),
                    _ => None,
                }
            };
            match bytes {
                Some(b) => tauri::http::Response::builder()
                    .status(200)
                    .header("Content-Type", "image/jpeg")
                    .header("Cache-Control", "max-age=86400")
                    .body(b)
                    .unwrap(),
                None => tauri::http::Response::builder()
                    .status(404)
                    .body(Vec::new())
                    .unwrap(),
            }
        })
        .setup(move |app| {
            let catalog = Catalog::open(dirs.clone()).map_err(|e| {
                log::error!("failed to open catalogue: {e}");
                e
            })?;
            diagnostics::set_verbose(catalog.setting_enabled(diagnostics::VERBOSE_LOG, false));
            let tray_mode = catalog.setting_enabled(shell::TRAY_MODE, false);
            let start_hidden = tray_mode && std::env::args().any(|a| a == "--hidden");
            app.manage(AppState {
                catalog: Arc::clone(&catalog),
                start_hidden: AtomicBool::new(start_hidden),
            });
            if tray_mode {
                shell::build_tray(app.handle())?;
            }
            if catalog.setting_enabled(shell::HOTKEY_ENABLED, false) {
                if let Err(e) = shell::register_hotkey(app.handle()) {
                    log::warn!("global shortcut not registered: {e}");
                }
            }
            live::start(app.handle().clone(), Arc::clone(&catalog));
            watcher::start(app.handle().clone(), catalog);
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    api.prevent_close();
                    let state = window.state::<AppState>();
                    if state.catalog.setting_enabled(shell::TRAY_MODE, false) {
                        let _ = window.hide();
                    } else {
                        shell::quit(window.app_handle());
                    }
                }
            }
            if let WindowEvent::Focused(false) = event {
                if window.label() == "palette" {
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::build_info,
            commands::should_show_main,
            commands::list_volumes,
            commands::list_detected,
            commands::catalog_stats,
            commands::add_detected,
            commands::add_folder,
            commands::pick_and_add_folder,
            commands::start_scan,
            commands::scanning,
            commands::remove_volume,
            commands::update_volume,
            commands::search,
            commands::item_detail,
            commands::open_item,
            commands::reveal_item,
            commands::get_setting,
            commands::set_setting,
            commands::start_enrich,
            commands::cancel_enrich,
            commands::enriching,
            commands::enrich_queued,
            commands::enrich_coverage,
            commands::reset_enrichment,
            commands::duplicates,
            commands::extension_counts,
            commands::duplicate_summary,
            commands::verify_group,
            commands::verifying,
            commands::clear_volume_fault,
            commands::ffmpeg_status,
            commands::download_ffmpeg,
            commands::export_volume,
            commands::import_volume,
            commands::plan_job,
            commands::create_job,
            commands::run_job,
            commands::pause_job,
            commands::cancel_job,
            commands::delete_job,
            commands::jobs,
            commands::job_items,
            commands::running_jobs,
            commands::sweep_spec,
            commands::pick_folders,
            commands::pick_files,
            commands::duplicate_cleanup,
            commands::tags,
            commands::tag_item,
            commands::untag_item,
            commands::saved_searches,
            commands::save_search,
            commands::delete_saved_search,
            commands::set_volume_photo,
            commands::clear_volume_photo,
            commands::shell_status,
            commands::set_tray_mode,
            commands::set_hotkey_enabled,
            commands::set_autostart,
            commands::hide_palette,
            commands::toggle_palette,
            commands::open_in_main,
            commands::show_main,
            commands::folder_tree,
            commands::kind_breakdown,
            commands::suggestions,
            commands::dismiss_suggestion,
            commands::reset_suggestions,
            commands::rule_settings,
            commands::set_rule_settings,
            commands::diagnostics_info,
            commands::open_logs_folder,
            commands::set_verbose_log,
            commands::report_problem,
            commands::open_release_notes,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Storecat");
}

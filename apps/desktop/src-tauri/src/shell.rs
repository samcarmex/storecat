//! Tray icon, global shortcut and the Spotlight-style palette window.

use tauri::menu::{Menu, MenuItem};
use tauri::tray::{TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut};

pub const TRAY_MODE: &str = "tray_mode";
pub const HOTKEY_ENABLED: &str = "hotkey_enabled";
pub const TRAY_ID: &str = "storecat-tray";

pub fn hotkey() -> Shortcut {
    Shortcut::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::Space)
}

pub fn register_hotkey(app: &AppHandle) -> tauri::Result<()> {
    let gs = app.global_shortcut();
    if !gs.is_registered(hotkey()) {
        gs.register(hotkey())
            .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!(e.to_string())))?;
    }
    Ok(())
}

pub fn unregister_hotkey(app: &AppHandle) {
    let _ = app.global_shortcut().unregister(hotkey());
}

pub fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    if app.tray_by_id(TRAY_ID).is_some() {
        return Ok(());
    }
    let open = MenuItem::with_id(app, "open", "Open Storecat", true, None::<&str>)?;
    let search = MenuItem::with_id(
        app,
        "search",
        "Quick search",
        true,
        Some("Ctrl+Shift+Space"),
    )?;
    let quit = MenuItem::with_id(app, "quit", "Quit Storecat", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &search, &quit])?;
    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .tooltip("Storecat")
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => show_main(app, None),
            "search" => toggle_palette(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::DoubleClick { .. } = event {
                show_main(tray.app_handle(), None);
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

pub fn remove_tray(app: &AppHandle) {
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_visible(false);
        let _ = app.remove_tray_by_id(TRAY_ID);
    }
}

/// Bring the main window up, optionally handing it a query to search.
pub fn show_main(app: &AppHandle, query: Option<String>) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        if let Some(q) = query {
            let _ = app.emit_to("main", "set-query", q);
        }
    }
    hide_palette(app);
}

pub fn hide_palette(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("palette") {
        let _ = w.hide();
    }
}

pub fn toggle_palette(app: &AppHandle) {
    // Window creation must happen on the main thread; commands run elsewhere.
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || toggle_palette_on_main(&handle));
}

fn toggle_palette_on_main(app: &AppHandle) {
    // The palette window is declared in tauri.conf.json (hidden at startup) so it gets the
    // same IPC setup as the main window; here we only show and hide it.
    let Some(w) = app.get_webview_window("palette") else {
        log::warn!("palette window missing from the configuration");
        return;
    };
    if w.is_visible().unwrap_or(false) {
        let _ = w.hide();
    } else {
        let _ = w.center();
        let _ = w.show();
        let _ = w.set_focus();
        let _ = app.emit_to("palette", "palette-shown", ());
    }
}

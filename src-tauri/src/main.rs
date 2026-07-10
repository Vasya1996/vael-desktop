// Prevents a console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod gsi;
mod vision;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::{extract::Query, response::Html, routing::get, Router};
use serde_json::json;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, State};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt as _};
use tauri_plugin_global_shortcut::GlobalShortcutExt;
use tauri_plugin_global_shortcut::ShortcutState;
use tauri_plugin_shell::ShellExt;

/// Fixed loopback port Dota POSTs GSI to (must match the installed cfg).
const GSI_PORT: u16 = 53210;
/// Default push-to-talk key. Chosen to avoid Dota's default voice key.
const DEFAULT_PTT: &str = "F8";
/// Overlay window size when collapsed (logical px).
const OVERLAY_W: u32 = 220;
const OVERLAY_H: u32 = 56;
/// Item panel geometry (logical px): rows of 3 icons.
const PANEL_ICON_H: u32 = 44;
const PANEL_ROW_GAP: u32 = 6;
const PANEL_PAD: u32 = 10;

pub(crate) struct AppState {
    gsi_port: u16,
    gsi_token: String,
    /// Currently registered push-to-talk accelerator.
    ptt: Mutex<String>,
    /// Item panel state: Some(px the window was shifted UP by, physical) while
    /// the panel is open; None when collapsed. Used to restore geometry and to
    /// suppress position-saving during programmatic moves.
    overlay_panel: Mutex<Option<i32>>,
    /// Latest own-hero facts parsed from GSI — the oracle the CV layer cross-checks
    /// every capture against (own hero must be recognized, or the capture is discarded).
    pub(crate) own: Mutex<Option<OwnHero>>,
    /// On-demand fast-watch deadline (ms since UNIX epoch). While `now < watch_until_ms`
    /// the vision loop polls ~250 ms instead of the idle 5 s, so an open scoreboard is
    /// read within a fraction of a second. Set by the `vision_watch` command; 0 = idle.
    pub(crate) watch_until_ms: AtomicU64,
}

/// The player's own hero, from GSI. GSI is enemy-blind but always carries the local
/// player's hero/team/level — the anchor that proves a scoreboard read is aligned.
#[derive(Clone)]
pub(crate) struct OwnHero {
    pub(crate) hero: String, // dotaconstants key, e.g. "npc_dota_hero_pudge"
    pub(crate) team: String, // "radiant" | "dire"
    // Reserved for the level cross-check (level reading is deferred — see desktop/docs).
    #[allow(dead_code)]
    pub(crate) level: Option<u32>,
}

/// Milliseconds since the UNIX epoch (monotonic-enough wall clock; only used for the
/// fast-watch window, where a small skew is harmless).
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Vision-loop poll interval: ~250 ms while inside an on-demand watch window
/// (`now_ms < watch_until_ms`), otherwise the idle 5 s background cadence. Pure so it
/// can be unit-tested without the Tauri runtime.
fn next_sleep(now_ms: u64, watch_until_ms: u64) -> Duration {
    if now_ms < watch_until_ms {
        Duration::from_millis(250)
    } else {
        Duration::from_millis(5000)
    }
}

// ───────────────────────── commands ─────────────────────────

/// Start (or extend) an on-demand fast-watch window for `for_sec` seconds. The vision
/// loop then polls ~250 ms (instead of the idle 5 s) so an open scoreboard is read within
/// a fraction of a second. Fired by the server's `vision_watch` control when the coach asks
/// the player to open the scoreboard. JS `invoke("vision_watch", { forSec })` maps to `for_sec`.
#[tauri::command]
fn vision_watch(state: State<AppState>, for_sec: u64) {
    let until = now_ms().saturating_add(for_sec.saturating_mul(1000));
    state.watch_until_ms.store(until, AtomicOrdering::Relaxed);
}

#[tauri::command]
fn get_gsi_info(state: State<AppState>) -> serde_json::Value {
    let ptt = state.ptt.lock().unwrap().clone();
    json!({ "port": state.gsi_port, "token": state.gsi_token, "pttKey": ptt })
}

#[tauri::command]
fn get_settings(app: AppHandle, state: State<AppState>) -> serde_json::Value {
    let ptt = state.ptt.lock().unwrap().clone();
    let autostart = app.autolaunch().is_enabled().unwrap_or(false);
    json!({ "pttKey": ptt, "autostart": autostart, "vision": vision::is_enabled() })
}

#[tauri::command]
fn install_gsi_config(state: State<AppState>) -> Result<String, String> {
    gsi::install_config(state.gsi_port, &state.gsi_token)
}

#[tauri::command]
fn dota_installed() -> bool {
    gsi::is_dota_installed()
}

#[tauri::command]
async fn launch_dota(app: AppHandle) -> Result<(), String> {
    // Borderless window (not exclusive fullscreen) so the overlay can draw on
    // top of the game; -gamestateintegration is required for Dota to POST GSI.
    // Args after `//` are appended to the user's own Steam launch options.
    app.shell()
        .open(
            "steam://rungameid/570//-gamestateintegration%20-windowed%20-noborder",
            None,
        )
        .map_err(|e| e.to_string())
}

/// Open an external page (site account/payment) in the default browser.
/// HTTPS only — the frontend only ever passes our own site URLs.
#[tauri::command]
async fn open_url(app: AppHandle, url: String) -> Result<(), String> {
    if !url.starts_with("https://") {
        return Err("Only https URLs can be opened".into());
    }
    app.shell().open(url, None).map_err(|e| e.to_string())
}

#[tauri::command]
fn default_server_url() -> String {
    // The desktop talks only to the cloud server, so the live VPS is the default.
    // Override at build time with VAEL_SERVER_URL, or in-app via Settings.
    option_env!("VAEL_SERVER_URL")
        .unwrap_or("https://ward.134.209.80.75.sslip.io")
        .to_string()
}

/// Re-register the push-to-talk global hotkey and persist the choice.
#[tauri::command]
fn set_ptt_key(app: AppHandle, state: State<AppState>, key: String) -> Result<(), String> {
    let key = key.trim().to_string();
    if key.is_empty() {
        return Err("Empty key".into());
    }
    let current = state.ptt.lock().unwrap().clone();
    if key == current {
        return Ok(());
    }
    let gs = app.global_shortcut();
    // Register the new key FIRST: if it fails, the old hotkey stays live.
    gs.register(key.as_str()).map_err(|e| e.to_string())?;
    let _ = gs.unregister(current.as_str());
    *state.ptt.lock().unwrap() = key.clone();
    save_ptt(&key);
    Ok(())
}

/// Enable / disable launch-with-Windows.
#[tauri::command]
fn set_autostart(app: AppHandle, enabled: bool) -> Result<(), String> {
    let mgr = app.autolaunch();
    if enabled {
        mgr.enable().map_err(|e| e.to_string())
    } else {
        mgr.disable().map_err(|e| e.to_string())
    }
}

/// Persist the off-by-default CV capture flag.
#[tauri::command]
fn vision_set_enabled(enabled: bool) {
    vision::set_enabled(enabled);
}

/// Capture the Dota window once and return the top-bar (both teams' hero portraits)
/// as a base64 JPEG for the server's roster-recognition VLM. One-shot, on-demand.
#[tauri::command]
fn vision_snapshot_topbar() -> Result<String, String> {
    #[cfg(windows)]
    {
        vision::snapshot_topbar()
    }
    #[cfg(not(windows))]
    {
        Err("vision capture is Windows-only".into())
    }
}


/// Show a short transient note in the overlay (e.g. "voice failed, retry").
#[tauri::command]
fn overlay_note(app: AppHandle, text: String) {
    let _ = app.emit("coach-note", &text);
}

/// Expand the overlay with an item-icons panel. The panel opens upward when the
/// overlay sits in the lower half of the screen (so it never runs off-screen),
/// downward otherwise; the capsule itself stays where the user dragged it.
#[tauri::command]
fn show_items(
    app: AppHandle,
    state: State<AppState>,
    items: serde_json::Value,
    ttl_sec: u64,
) -> Result<(), String> {
    let overlay = app
        .get_webview_window("overlay")
        .ok_or_else(|| "no overlay window".to_string())?;

    // Re-show while open: restore collapsed geometry first so the math below
    // always starts from the capsule's true position.
    collapse_overlay(&app, &state);

    let count = items.as_array().map(|a| a.len()).unwrap_or(0);
    if count == 0 {
        return Ok(());
    }
    let rows = ((count + 2) / 3).min(2) as u32;
    let panel_h = rows * (PANEL_ICON_H + PANEL_ROW_GAP) + PANEL_PAD;

    let scale = overlay.scale_factor().map_err(|e| e.to_string())?;
    let pos = overlay.outer_position().map_err(|e| e.to_string())?;
    let panel_h_phys = (panel_h as f64 * scale) as i32;
    let total_h_phys = ((OVERLAY_H + panel_h) as f64 * scale) as i32;

    // Open upward when the expanded window would poke past the monitor bottom.
    let mut open_up = false;
    if let Ok(Some(mon)) = app.primary_monitor() {
        let bottom = mon.position().y + mon.size().height as i32;
        open_up = pos.y + total_h_phys > bottom;
    }

    let offset = if open_up { panel_h_phys } else { 0 };
    *state.overlay_panel.lock().unwrap() = Some(offset);
    if offset > 0 {
        let _ = overlay.set_position(PhysicalPosition::new(pos.x, pos.y - offset));
    }
    let _ = overlay.set_size(tauri::LogicalSize::new(
        OVERLAY_W as f64,
        (OVERLAY_H + panel_h) as f64,
    ));

    let _ = app.emit(
        "items-panel",
        json!({ "items": items, "ttlSec": ttl_sec, "dir": if open_up { "up" } else { "down" } }),
    );
    Ok(())
}

/// Collapse the item panel back to the bare capsule (no-op when collapsed).
#[tauri::command]
fn hide_items(app: AppHandle, state: State<AppState>) {
    collapse_overlay(&app, &state);
}

fn collapse_overlay(app: &AppHandle, state: &State<AppState>) {
    let Some(offset) = state.overlay_panel.lock().unwrap().take() else {
        return;
    };
    if let Some(overlay) = app.get_webview_window("overlay") {
        let _ = overlay.set_size(tauri::LogicalSize::new(OVERLAY_W as f64, OVERLAY_H as f64));
        if offset > 0 {
            if let Ok(pos) = overlay.outer_position() {
                let _ = overlay.set_position(PhysicalPosition::new(pos.x, pos.y + offset));
            }
        }
    }
}

/// The overlay frontend reports the capsule's hit rect (physical px, relative to
/// the window's client-area top-left) every ~150 ms. The Windows click-through
/// watchdog keeps the overlay transparent to clicks everywhere except inside this
/// rect, so the item icons and empty padding pass clicks straight to the game
/// while the capsule stays draggable.
#[tauri::command]
fn set_overlay_hit_rect(x: i32, y: i32, w: i32, h: i32) {
    #[cfg(windows)]
    topmost::set_hit_rect(x, y, w, h);
    #[cfg(not(windows))]
    let _ = (x, y, w, h);
}

/// Push the current coach state to the in-game overlay and tray tooltip.
#[tauri::command]
fn set_coach_state(app: AppHandle, state: String) {
    let _ = app.emit("coach-state", &state);
    let tip = match state.as_str() {
        "listen" => "Vael · слушает",
        "think" => "Vael · думает",
        "answer" => "Vael · отвечает",
        _ => "Vael · готов",
    };
    if let Some(tray) = app.tray_by_id("main") {
        let _ = tray.set_tooltip(Some(tip));
    }
}

/// Google sign-in: open the browser to the server's OAuth start, run a one-shot
/// loopback server, and resolve with the app JWT the server redirects back with.
#[tauri::command]
async fn google_login(app: AppHandle, server_base: String) -> Result<String, String> {
    use std::sync::Arc;
    use tokio::sync::oneshot;

    let (tx, rx) = oneshot::channel::<String>();
    let tx = Arc::new(std::sync::Mutex::new(Some(tx)));

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();

    let router = Router::new().route(
        "/",
        get(move |Query(params): Query<HashMap<String, String>>| {
            let tx = tx.clone();
            async move {
                if let Some(token) = params.get("token") {
                    if let Some(sender) = tx.lock().unwrap().take() {
                        let _ = sender.send(token.clone());
                    }
                    Html(close_page("Готово!"))
                } else {
                    Html(close_page("Вход не завершён"))
                }
            }
        }),
    );

    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    let base = server_base.trim_end_matches('/');
    let url = format!("{base}/auth/google/start?redirect=http://127.0.0.1:{port}/");
    app.shell().open(url, None).map_err(|e| e.to_string())?;

    let result = tokio::time::timeout(Duration::from_secs(180), rx).await;
    server.abort();
    match result {
        Ok(Ok(token)) => Ok(token),
        Ok(Err(_)) => Err("Окно входа закрыто".into()),
        Err(_) => Err("Время ожидания входа истекло".into()),
    }
}

fn close_page(message: &str) -> String {
    format!(
        "<!doctype html><meta charset=\"utf-8\"><title>Vael</title>\
         <body style=\"background:#0B0E13;color:#EAF0F8;font-family:system-ui;display:grid;place-items:center;height:100vh;margin:0\">\
         <div style=\"text-align:center\"><div style=\"font-size:18px;font-weight:600\">{message}</div>\
         <div style=\"color:#8A95A6;margin-top:8px\">Можно вернуться в приложение Vael.</div></div></body>"
    )
}

// ───────────────────────── overlay always-on-top (Windows) ─────────────────────────

/// Keeps the in-game overlay above Dota without polling/flicker. A system
/// foreground-change hook (`EVENT_SYSTEM_FOREGROUND`) fires whenever the active
/// top-level window changes; we re-raise the overlay to topmost with
/// `SWP_NOACTIVATE`, which reorders it visually but never steals focus — the
/// player's clicks and keys keep going to the game. While Dota remains the
/// active window (continuous in-game clicking) no event fires, so there is
/// nothing to flicker.
#[cfg(windows)]
mod topmost {
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicBool, AtomicI32, AtomicIsize, Ordering};
    use std::time::Duration;
    use windows::Win32::Foundation::{HWND, POINT, RECT};
    use windows::Win32::UI::Accessibility::{SetWinEventHook, HWINEVENTHOOK};
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetCursorPos, GetMessageW, GetWindowLongPtrW, GetWindowRect,
        SetWindowLongPtrW, SetWindowPos, TranslateMessage, EVENT_SYSTEM_FOREGROUND, GWL_EXSTYLE,
        HWND_TOPMOST, MSG, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, WINEVENT_OUTOFCONTEXT,
        WINEVENT_SKIPOWNPROCESS, WS_EX_TRANSPARENT,
    };

    static OVERLAY: AtomicIsize = AtomicIsize::new(0);
    // True only while the overlay is on screen (Dota running). Gates the
    // click-through watchdog so nothing toggles while the overlay is hidden.
    static ACTIVE: AtomicBool = AtomicBool::new(false);

    // The capsule's hit rectangle (physical px, relative to the overlay's
    // client-area top-left), reported by the frontend. The watchdog keeps the
    // window transparent to clicks everywhere except inside this rect.
    static HIT_X: AtomicI32 = AtomicI32::new(0);
    static HIT_Y: AtomicI32 = AtomicI32::new(0);
    static HIT_W: AtomicI32 = AtomicI32::new(0);
    static HIT_H: AtomicI32 = AtomicI32::new(0);

    unsafe extern "system" fn on_foreground(
        _hook: HWINEVENTHOOK,
        _event: u32,
        _hwnd: HWND,
        _id_object: i32,
        _id_child: i32,
        _thread: u32,
        _time: u32,
    ) {
        let raw = OVERLAY.load(Ordering::Relaxed);
        if raw != 0 {
            // NOMOVE/NOSIZE: only the z-order changes. NOACTIVATE: focus stays
            // with whatever the player is using (the game).
            let _ = SetWindowPos(
                HWND(raw as *mut c_void),
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
    }

    /// Enable/disable the click-through watchdog. Called when the overlay is
    /// shown/hidden as Dota starts/stops.
    pub fn set_active(active: bool) {
        ACTIVE.store(active, Ordering::Relaxed);
    }

    /// Frontend-reported capsule hit rect (physical px, client-relative).
    pub fn set_hit_rect(x: i32, y: i32, w: i32, h: i32) {
        HIT_X.store(x, Ordering::Relaxed);
        HIT_Y.store(y, Ordering::Relaxed);
        HIT_W.store(w, Ordering::Relaxed);
        HIT_H.store(h, Ordering::Relaxed);
    }

    /// Add/remove `WS_EX_TRANSPARENT` so clicks either pass through to the game
    /// (`on = true`) or are received by the overlay (`on = false`). Writes only on
    /// a real change to avoid needless style churn.
    fn set_passthrough(on: bool) {
        static CUR: AtomicBool = AtomicBool::new(false);
        if CUR.swap(on, Ordering::Relaxed) == on {
            return;
        }
        let raw = OVERLAY.load(Ordering::Relaxed);
        if raw == 0 {
            return;
        }
        let hwnd = HWND(raw as *mut c_void);
        unsafe {
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
            let new_ex = if on {
                ex | WS_EX_TRANSPARENT.0
            } else {
                ex & !WS_EX_TRANSPARENT.0
            };
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, new_ex as isize);
        }
    }

    /// True when the cursor sits inside the capsule's hit rect. The overlay is
    /// borderless (no non-client area), so the window rect's top-left is the
    /// client origin; add the frontend-reported rect to get screen coordinates.
    fn cursor_over_capsule() -> bool {
        let w = HIT_W.load(Ordering::Relaxed);
        let h = HIT_H.load(Ordering::Relaxed);
        if w <= 0 || h <= 0 {
            return false;
        }
        let raw = OVERLAY.load(Ordering::Relaxed);
        if raw == 0 {
            return false;
        }
        let hwnd = HWND(raw as *mut c_void);
        unsafe {
            let mut pt = POINT::default();
            let mut wr = RECT::default();
            if GetCursorPos(&mut pt).is_err() || GetWindowRect(hwnd, &mut wr).is_err() {
                return false;
            }
            let x0 = wr.left + HIT_X.load(Ordering::Relaxed);
            let y0 = wr.top + HIT_Y.load(Ordering::Relaxed);
            pt.x >= x0 && pt.x < x0 + w && pt.y >= y0 && pt.y < y0 + h
        }
    }

    /// Install the hook on a dedicated thread with its own message loop (the
    /// out-of-context callback only fires while a thread pumps messages), plus a
    /// click-through watchdog that keeps the overlay transparent to clicks
    /// everywhere except over the capsule.
    pub fn keep_on_top(overlay_hwnd: isize) {
        OVERLAY.store(overlay_hwnd, Ordering::Relaxed);
        std::thread::spawn(|| unsafe {
            SetWinEventHook(
                EVENT_SYSTEM_FOREGROUND,
                EVENT_SYSTEM_FOREGROUND,
                // Out-of-context hook (WINEVENT_OUTOFCONTEXT) → no DLL module handle;
                // windows 0.61 types this arg as Option<HMODULE>, so pass None.
                None,
                Some(on_foreground),
                0,
                0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            );
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        });

        // Click-through watchdog: the overlay receives clicks only over the
        // capsule (kept draggable) and passes them to the game everywhere else
        // (item icons, transparent padding). Driven by the GLOBAL cursor position
        // because a click-through window gets no mouse events of its own.
        std::thread::spawn(|| loop {
            std::thread::sleep(Duration::from_millis(16));
            if ACTIVE.load(Ordering::Relaxed) {
                set_passthrough(!cursor_over_capsule());
            }
        });
    }
}

// ───────────────────────── app ─────────────────────────

fn main() {
    let token = load_or_create_token();
    let initial_ptt = load_ptt();

    let shortcut_plugin = tauri_plugin_global_shortcut::Builder::new()
        .with_handler(|app, _shortcut, event| {
            // Push-to-talk: hold = listening, release = stop.
            let name = match event.state() {
                ShortcutState::Pressed => "ptt-start",
                ShortcutState::Released => "ptt-end",
            };
            let _ = app.emit(name, ());
        })
        .build();

    tauri::Builder::default()
        // Single instance MUST be registered first: a second launch (e.g. the
        // user opens Vael again while autostart already ran it) just focuses the
        // existing window instead of spawning another process — which would
        // create a second in-game overlay (the "duplicate floating icons" bug).
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.unminimize();
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, None))
        .plugin(shortcut_plugin)
        // Auto-update: the updater checks our signed feed on launch; the process
        // plugin lets the UI relaunch into the freshly installed version.
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(AppState {
            gsi_port: GSI_PORT,
            gsi_token: token.clone(),
            ptt: Mutex::new(initial_ptt.clone()),
            overlay_panel: Mutex::new(None),
            own: Mutex::new(None),
            watch_until_ms: AtomicU64::new(0),
        })
        .invoke_handler(tauri::generate_handler![
            get_gsi_info,
            get_settings,
            install_gsi_config,
            dota_installed,
            launch_dota,
            open_url,
            default_server_url,
            set_ptt_key,
            set_autostart,
            set_coach_state,
            overlay_note,
            show_items,
            hide_items,
            google_login,
            vision_set_enabled,
            vision_snapshot_topbar,
            vision_watch,
            set_overlay_hit_rect
        ])
        .setup(move |app| {
            let handle = app.handle().clone();

            // 1) Loopback GSI listener.
            {
                let h = handle.clone();
                let tok = token.clone();
                tauri::async_runtime::spawn(async move {
                    gsi::serve(h, GSI_PORT, tok).await;
                });
            }

            // 2) Register push-to-talk hotkey.
            if let Err(e) = app.global_shortcut().register(initial_ptt.as_str()) {
                eprintln!("[vael] failed to register PTT shortcut {initial_ptt}: {e}");
            }

            // 3) In-game overlay: hidden until Dota runs. It stays mouse-
            // interactive so the player can drag it anywhere; the position is
            // saved on move and restored on every show.

            // 4) dota2.exe presence watcher (also drives the overlay).
            {
                let h = handle.clone();
                tauri::async_runtime::spawn(async move {
                    watch_dota(h).await;
                });
            }

            // 4c) CV scoreboard scan loop (Windows). Flag-gated and OFF by default, so for
            // normal users this thread just sleeps and the app behaves exactly as today.
            #[cfg(windows)]
            {
                let h = handle.clone();
                std::thread::spawn(move || vision_capture_loop(h));
            }

            // 4b) Keep the overlay above the game without flicker. The
            // `alwaysOnTop` flag set at creation gets dropped when the player
            // clicks into Dota (the game takes foreground). Instead of polling,
            // a system foreground-change hook re-raises the overlay the instant
            // another window takes focus — with NOACTIVATE, so the game keeps
            // input. While Dota stays the active window (continuous clicking)
            // no event fires and nothing flickers.
            #[cfg(windows)]
            if let Some(ov) = handle.get_webview_window("overlay") {
                if let Ok(hwnd) = ov.hwnd() {
                    topmost::keep_on_top(hwnd.0 as isize);
                }
            }

            // 5) System tray.
            build_tray(&handle)?;

            // 6) WebView2 denies microphone access by default for the app origin
            // (wry only auto-allows clipboard), so push-to-talk getUserMedia would
            // fail silently. Grant mic/camera ourselves.
            #[cfg(windows)]
            if let Some(win) = handle.get_webview_window("main") {
                grant_media_permissions(&win);
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the MAIN window hides it to the tray instead of quitting.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    let _ = window.hide();
                    api.prevent_close();
                }
            }
            // Remember where the user drags the overlay. Programmatic moves
            // (item panel expanding upward) are filtered out via overlay_panel.
            if let tauri::WindowEvent::Moved(pos) = event {
                if window.label() == "overlay" {
                    let state: State<AppState> = window.state();
                    if state.overlay_panel.lock().unwrap().is_none() {
                        save_overlay_pos(pos.x, pos.y);
                    }
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running Vael");
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Открыть Vael", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Выход", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &quit])?;

    let mut builder = TrayIconBuilder::with_id("main")
        .tooltip("Vael · готов")
        .menu(&menu)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
            }
            "quit" => app.exit(0),
            _ => {}
        });
    // Use the bundle icon when present; don't panic if it's missing.
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

/// On Windows/WebView2 the microphone is denied by default for the Tauri app
/// origin — wry's permission handler only auto-allows clipboard. Without this,
/// `navigator.mediaDevices.getUserMedia({audio:true})` (push-to-talk) rejects
/// with NotAllowedError. We attach our own PermissionRequested handler that
/// allows mic/camera on every request (also fixes the "re-prompt each launch"
/// issue, since WebView2 otherwise caches the decision per user-data folder).
#[cfg(windows)]
fn grant_media_permissions(window: &tauri::WebviewWindow) {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        COREWEBVIEW2_PERMISSION_KIND_CAMERA, COREWEBVIEW2_PERMISSION_KIND_MICROPHONE,
        COREWEBVIEW2_PERMISSION_STATE_ALLOW,
    };
    use webview2_com::PermissionRequestedEventHandler;

    let _ = window.with_webview(|webview| unsafe {
        let core = match webview.controller().CoreWebView2() {
            Ok(c) => c,
            Err(_) => return,
        };
        let handler = PermissionRequestedEventHandler::create(Box::new(|_sender, args| {
            if let Some(args) = args {
                let mut kind = Default::default();
                args.PermissionKind(&mut kind)?;
                if kind == COREWEBVIEW2_PERMISSION_KIND_MICROPHONE
                    || kind == COREWEBVIEW2_PERMISSION_KIND_CAMERA
                {
                    args.SetState(COREWEBVIEW2_PERMISSION_STATE_ALLOW)?;
                }
            }
            Ok(())
        }));
        let mut token = 0_i64;
        let _ = core.add_PermissionRequested(&handler, &mut token);
    });
}

/// Poll for dota2.exe; emit `dota-status { running }` and toggle the overlay.
async fn watch_dota(app: AppHandle) {
    use sysinfo::System;
    let mut last: Option<bool> = None;
    loop {
        let sys = System::new_all();
        let running = sys
            .processes()
            .values()
            .any(|p| p.name().to_string_lossy().eq_ignore_ascii_case("dota2.exe"));
        if last != Some(running) {
            last = Some(running);
            let _ = app.emit("dota-status", json!({ "running": running }));
            if let Some(overlay) = app.get_webview_window("overlay") {
                if running {
                    position_overlay(&app, &overlay);
                    let _ = overlay.show();
                    #[cfg(windows)]
                    topmost::set_active(true);
                } else {
                    let _ = overlay.hide();
                    #[cfg(windows)]
                    topmost::set_active(false);
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(4)).await;
    }
}

/// CV scoreboard scan loop. Flag-gated (off by default → this thread only sleeps). When
/// enabled and Dota is running, it captures the window every few seconds, reads the
/// ENEMY roster off the scoreboard (validated against the GSI own hero), and emits a
/// `vision` event the WebView relays to the server. Safety: `vision::pipeline` never
/// returns a wrong hero; here we add two-frame confirmation — a hero must read `known`
/// in two consecutive scans before it ships as `known`, else it ships `unconfirmed`
/// (which the server drops). The enemy lineup is static, so this costs only ~one extra
/// scan of latency.
#[cfg(windows)]
fn vision_capture_loop(app: AppHandle) {
    use std::collections::{HashMap, HashSet};
    // Top-bar composition (recognize-and-lock): confirm counts, the lock itself, and
    // the cached anchor geometry to re-verify (not re-sweep) each scan.
    let mut tb_confirm: HashMap<String, u32> = HashMap::new();
    let mut tb_locked = false;
    let mut tb_geom: Option<vision::locate::Located> = None;
    // Plateau slow-down: some enemy slots (arcana/alt art) can stably score below
    // recognize::PEAK_MIN and so never reach `known` — `tb_locked` (all 9 confirmed)
    // may then never trigger. Track consecutive scans with no NEW confirmed hero; once
    // the composition has plateaued, drop to a slow keep-alive cadence (main loop below)
    // instead of burning a capture every tick for slots that will never confirm.
    let mut tb_stale_scans: u32 = 0;
    let mut tb_skip: u32 = 0;
    let mut last_own: Option<String> = None;
    // True while we were inside a fast-watch window on the previous iteration; used to do
    // one immediate scan the instant a watch window opens (skip the leading sleep) so the
    // first confirming frame is taken right away rather than up to 5 s later.
    let mut was_watching = false;
    loop {
        // Sleep FIRST (as before), but at the watch cadence (~250 ms) while a fast-watch
        // window is active, else the idle 5 s. On the transition into a fresh watch window
        // skip the sleep entirely so the open scoreboard is scanned immediately.
        let watch_until = app
            .state::<AppState>()
            .watch_until_ms
            .load(AtomicOrdering::Relaxed);
        let watching = now_ms() < watch_until;
        if !(watching && !was_watching) {
            std::thread::sleep(next_sleep(now_ms(), watch_until));
        }
        was_watching = watching;
        if !vision::is_enabled() {
            tb_confirm.clear();
            tb_locked = false;
            tb_geom = None;
            tb_stale_scans = 0;
            tb_skip = 0;
            last_own = None;
            continue;
        }
        let own = {
            let st = app.state::<AppState>();
            let g = st.own.lock().unwrap();
            g.clone()
        };
        let Some(own) = own else { continue };
        // New draft/match (own hero changed) → drop all temporal state so a previous game's
        // levels can never gate this one (the monotonic lock would otherwise withhold reads).
        if last_own.as_deref() != Some(own.hero.as_str()) {
            tb_confirm.clear();
            tb_locked = false;
            tb_geom = None;
            tb_stale_scans = 0;
            tb_skip = 0;
            last_own = Some(own.hero.clone());
        }

        // Top-bar composition (Phase A): the draft is static, so scan only until every
        // non-own slot has shipped `known` twice (recognize-and-lock), then stop for the
        // match. Cheap steady state: the cached anchor geometry is re-verified, not re-swept.
        if !tb_locked {
            // Plateau slow-down: once 30 consecutive scans confirmed no NEW hero, the
            // remaining slots are presumed unconfirmable (arcana/alt art) — scan only
            // every 6th iteration to keep the composition alive without full-rate cost.
            let should_scan = if tb_stale_scans >= 30 {
                tb_skip = (tb_skip + 1) % 6;
                tb_skip == 0
            } else {
                true
            };
            if should_scan {
                if let Some((mut tb_payload, loc)) = vision::scan_topbar_now(&own.hero, &own.team, tb_geom.as_ref()) {
                    tb_geom = Some(loc);
                    if let Some(obs) = tb_payload.get_mut("observations").and_then(|v| v.as_array_mut()) {
                        let frame_known: HashSet<String> = obs.iter()
                            .filter(|o| o.get("status").and_then(|s| s.as_str()) == Some("known"))
                            .filter_map(|o| o.get("hero").and_then(|h| h.as_str()).map(str::to_string))
                            .filter(|h| !h.is_empty())
                            .collect();
                        let confirmed_before = tb_confirm.values().filter(|&&c| c >= 2).count();
                        for hero in &frame_known { *tb_confirm.entry(hero.clone()).or_insert(0) += 1; }
                        tb_confirm.retain(|k, _| frame_known.contains(k));
                        let confirmed_after = tb_confirm.values().filter(|&&c| c >= 2).count();
                        let grew = confirmed_after > confirmed_before;
                        if grew {
                            tb_stale_scans = 0;
                        } else {
                            tb_stale_scans += 1;
                        }
                        let mut all_confirmed = obs.len() == 9;
                        for o in obs.iter_mut() {
                            if o.get("status").and_then(|s| s.as_str()) == Some("known") {
                                let hero = o.get("hero").and_then(|h| h.as_str()).unwrap_or("");
                                if tb_confirm.get(hero).copied().unwrap_or(0) < 2 {
                                    o["status"] = json!("unconfirmed");
                                    all_confirmed = false;
                                }
                            } else { all_confirmed = false; }
                        }
                        tb_locked = all_confirmed;
                    }
                    let _ = app.emit("vision", tb_payload);
                }
            }
        }
    }
}

/// Place the overlay: at the user's saved spot when one exists, otherwise in
/// the bottom-right corner. Either way the result is clamped fully on-screen,
/// so it can never end up cut off by an edge (scale/taskbar guesses included).
fn position_overlay(app: &AppHandle, overlay: &tauri::WebviewWindow) {
    if let Ok(Some(mon)) = app.primary_monitor() {
        let scale = mon.scale_factor();
        let size = mon.size();
        let pos = mon.position();
        let ow = (OVERLAY_W as f64 * scale) as i32;
        let oh = (OVERLAY_H as f64 * scale) as i32;
        let margin = (24.0 * scale) as i32;
        let taskbar = (56.0 * scale) as i32; // clear the Windows taskbar
        let (x, y) = load_overlay_pos().unwrap_or((
            pos.x + size.width as i32 - ow - margin,
            pos.y + size.height as i32 - oh - taskbar,
        ));
        let x = x.clamp(pos.x, pos.x + size.width as i32 - ow);
        let y = y.clamp(pos.y, pos.y + size.height as i32 - oh);
        let _ = overlay.set_position(PhysicalPosition::new(x, y));
    }
}

// ───────────────────────── persistence ─────────────────────────

pub(crate) fn config_dir() -> std::path::PathBuf {
    let dir = dirs::config_dir()
        .map(|d| d.join("vael"))
        .unwrap_or_else(|| std::env::temp_dir().join("vael"));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn load_ptt() -> String {
    let path = config_dir().join("ptt_key");
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let t = existing.trim().to_string();
        if !t.is_empty() {
            return t;
        }
    }
    DEFAULT_PTT.to_string()
}

fn save_ptt(key: &str) {
    let _ = std::fs::write(config_dir().join("ptt_key"), key);
}

/// Overlay position (physical px, "x,y"). Saved on drag, restored on show.
fn save_overlay_pos(x: i32, y: i32) {
    let _ = std::fs::write(config_dir().join("overlay_pos"), format!("{x},{y}"));
}

fn load_overlay_pos() -> Option<(i32, i32)> {
    let raw = std::fs::read_to_string(config_dir().join("overlay_pos")).ok()?;
    let (x, y) = raw.trim().split_once(',')?;
    Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
}

/// Persist a stable GSI auth token so the installed cfg keeps matching across runs.
fn load_or_create_token() -> String {
    let path = config_dir().join("gsi_token");
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let t = existing.trim().to_string();
        if !t.is_empty() {
            return t;
        }
    }
    let token = pseudo_token();
    let _ = std::fs::write(&path, &token);
    token
}

/// Loopback-only token; weak randomness is acceptable since it never leaves localhost.
fn pseudo_token() -> String {
    let mut s = String::new();
    for i in 0..4u128 {
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
            ^ (i.wrapping_mul(0x9E3779B97F4A7C15));
        s.push_str(&format!("{:016x}", n as u64));
        std::thread::sleep(Duration::from_nanos(7));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::next_sleep;
    use std::time::Duration;

    #[test]
    fn next_sleep_fast_inside_watch_window() {
        // now strictly before the deadline → fast ~250 ms watch cadence.
        assert_eq!(next_sleep(1_000, 2_000), Duration::from_millis(250));
    }

    #[test]
    fn next_sleep_idle_at_or_after_watch_window() {
        // now == deadline → idle (boundary is exclusive of fast polling).
        assert_eq!(next_sleep(2_000, 2_000), Duration::from_millis(5000));
        // now past deadline → idle 5 s.
        assert_eq!(next_sleep(9_999, 2_000), Duration::from_millis(5000));
        // watch never armed (deadline 0) → idle.
        assert_eq!(next_sleep(1, 0), Duration::from_millis(5000));
    }
}

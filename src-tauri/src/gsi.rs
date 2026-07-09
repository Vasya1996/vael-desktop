//! Local GSI receiver + config installer.
//!
//! GSI is loopback by design: Dota POSTs match state to a port WE listen on. We
//! validate the auth token, strip it, and forward the cleaned payload to the
//! WebView, which relays it to the cloud server. The handler returns 200 fast so
//! Dota never falls into retries.

use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
use serde_json::Value;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter};

#[derive(Clone)]
struct GsiState {
    app: AppHandle,
    token: String,
}

/// Start the loopback HTTP listener that receives Dota's GSI POSTs.
pub async fn serve(app: AppHandle, port: u16, token: String) {
    let state = GsiState { app, token };
    let router = Router::new().route("/", post(handle)).with_state(state);

    match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
        Ok(listener) => {
            let _ = axum::serve(listener, router).await;
        }
        Err(e) => eprintln!("[vael] GSI listener failed to bind 127.0.0.1:{port}: {e}"),
    }
}

async fn handle(State(state): State<GsiState>, Json(mut payload): Json<Value>) -> StatusCode {
    // Validate auth.token; reject spoofed posts.
    let ok = payload
        .get("auth")
        .and_then(|a| a.get("token"))
        .and_then(|t| t.as_str())
        .map(|t| t == state.token)
        .unwrap_or(false);
    if !ok {
        return StatusCode::FORBIDDEN;
    }
    // Strip auth before forwarding.
    if let Some(obj) = payload.as_object_mut() {
        obj.remove("auth");
    }
    // Snapshot the player's own hero/team/level for the CV own-row oracle (GSI is the
    // source of truth the screen read is validated against).
    update_own_hero(&state.app, &payload);
    // Heavy work (network to server) happens in the WebView; emit is cheap.
    let _ = state.app.emit("gsi", payload);
    StatusCode::OK
}

/// Mirror the local player's hero/team/level from a GSI payload into shared state.
///
/// GSI posts PARTIAL deltas (only changed blocks; see shared/gsi.ts), so a single tick may
/// carry `hero` without `player` or vice-versa. We therefore MERGE field-by-field into the
/// last-known value instead of rebuilding from each delta — otherwise a player-only delta
/// would blank the hero and a hero-only delta would blank the team. We never clear the
/// state here: a stale own hero from a previous match is harmless because the vision
/// own-row oracle simply won't find it on the new scoreboard (so it discards rather than
/// misreads), and the next match's `hero`/`player` deltas overwrite it.
fn update_own_hero(app: &AppHandle, payload: &Value) {
    use tauri::Manager;
    let hero = payload
        .get("hero")
        .and_then(|h| h.get("name"))
        .and_then(|n| n.as_str())
        .filter(|h| h.starts_with("npc_dota_hero_"));
    let team = payload
        .get("player")
        .and_then(|p| p.get("team_name"))
        .and_then(|t| t.as_str())
        .filter(|t| *t == "radiant" || *t == "dire");
    let level = payload
        .get("hero")
        .and_then(|h| h.get("level"))
        .and_then(|l| l.as_u64())
        .map(|l| l as u32);
    if hero.is_none() && team.is_none() && level.is_none() {
        return; // nothing relevant in this delta
    }
    let st = app.state::<crate::AppState>();
    let mut guard = st.own.lock().unwrap();
    let cur = guard.get_or_insert_with(|| crate::OwnHero {
        hero: String::new(),
        team: String::new(),
        level: None,
    });
    if let Some(h) = hero {
        cur.hero = h.to_string();
    }
    if let Some(t) = team {
        cur.team = t.to_string();
    }
    if let Some(l) = level {
        cur.level = Some(l);
    }
}

/// The cfg body Dota reads to know where to POST GSI.
fn config_body(port: u16, token: &str) -> String {
    format!(
        "\"Vael\"\n{{\n    \"uri\"        \"http://127.0.0.1:{port}/\"\n    \"timeout\"    \"5.0\"\n    \"buffer\"     \"0.1\"\n    \"throttle\"   \"0.1\"\n    \"heartbeat\"  \"30.0\"\n    \"data\"\n    {{\n        \"provider\"  \"1\"\n        \"map\"       \"1\"\n        \"player\"    \"1\"\n        \"hero\"      \"1\"\n        \"abilities\" \"1\"\n        \"items\"     \"1\"\n    }}\n    \"auth\"\n    {{\n        \"token\" \"{token}\"\n    }}\n}}\n"
    )
}

/// Install gamestate_integration_vael.cfg into the Dota 2 config folder.
/// Returns the written file path on success.
pub fn install_config(port: u16, token: &str) -> Result<String, String> {
    let dir = find_gsi_dir().ok_or_else(|| {
        "Could not locate the Dota 2 config folder. Is Dota 2 installed via Steam?".to_string()
    })?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    // Remove the legacy "Ward" cfg so Dota doesn't POST GSI twice after the rename.
    let _ = std::fs::remove_file(dir.join("gamestate_integration_ward.cfg"));
    let file = dir.join("gamestate_integration_vael.cfg");
    std::fs::write(&file, config_body(port, token)).map_err(|e| e.to_string())?;
    Ok(file.to_string_lossy().to_string())
}

/// Whether a Dota 2 install was located (used for the "Dota не найдена" alert).
pub fn is_dota_installed() -> bool {
    find_gsi_dir().is_some()
}

/// Locate `.../dota 2 beta/game/dota/cfg/gamestate_integration`.
fn find_gsi_dir() -> Option<PathBuf> {
    for lib in steam_library_paths() {
        let dota = lib
            .join("steamapps")
            .join("common")
            .join("dota 2 beta");
        if dota.exists() {
            return Some(
                dota.join("game")
                    .join("dota")
                    .join("cfg")
                    .join("gamestate_integration"),
            );
        }
    }
    None
}

#[cfg(windows)]
fn steam_library_paths() -> Vec<PathBuf> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let mut steam: Option<PathBuf> = None;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    if let Ok(key) = hkcu.open_subkey("Software\\Valve\\Steam") {
        if let Ok(p) = key.get_value::<String, _>("SteamPath") {
            steam = Some(PathBuf::from(p.replace('/', "\\")));
        }
    }
    // Common fallbacks.
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(s) = steam.clone() {
        roots.push(s);
    }
    for c in [
        "C:\\Program Files (x86)\\Steam",
        "C:\\Program Files\\Steam",
    ] {
        let p = PathBuf::from(c);
        if !roots.contains(&p) {
            roots.push(p);
        }
    }

    // Expand with libraryfolders.vdf entries from the primary Steam dir.
    let mut libs: Vec<PathBuf> = Vec::new();
    for root in &roots {
        if root.exists() && !libs.contains(root) {
            libs.push(root.clone());
        }
        let vdf = root.join("steamapps").join("libraryfolders.vdf");
        if let Ok(text) = std::fs::read_to_string(&vdf) {
            for path in parse_library_paths(&text) {
                if !libs.contains(&path) {
                    libs.push(path);
                }
            }
        }
    }
    libs
}

#[cfg(not(windows))]
fn steam_library_paths() -> Vec<PathBuf> {
    // Dev fallback (macOS/Linux) — best effort for local testing.
    let mut libs = Vec::new();
    if let Some(home) = dirs::home_dir() {
        libs.push(home.join(".steam/steam"));
        libs.push(home.join(".local/share/Steam"));
        libs.push(home.join("Library/Application Support/Steam"));
    }
    libs
}

/// Extract `"path" "<value>"` entries from a libraryfolders.vdf.
fn parse_library_paths(text: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for line in text.lines() {
        let parts: Vec<&str> = line.split('"').collect();
        // ["", "path", "\t\t", "D:\\SteamLibrary", ""]
        if parts.len() >= 4 && parts[1].eq_ignore_ascii_case("path") {
            let value = parts[3].replace("\\\\", "\\");
            if !value.is_empty() {
                out.push(PathBuf::from(value));
            }
        }
    }
    out
}

#[allow(dead_code)]
fn _assert_path(_: &Path) {}

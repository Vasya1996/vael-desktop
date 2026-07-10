//! Computer-vision capture layer.
//! P1-A: capture the dota2.exe window in-process, behind an off-by-default flag.

use std::path::PathBuf;

#[cfg(windows)]
pub mod capture;
pub mod color_refs;
#[cfg(windows)]
pub mod hwnd;
pub mod level;
pub mod locate;
pub mod ncc;
pub mod pipeline;
pub mod recognize;
pub mod topbar;
pub mod topbar_refs;
pub mod vlog;
pub mod zones;

/// The off-by-default flag file, beside ptt_key / overlay_pos in config_dir().
fn flag_path() -> PathBuf {
    crate::config_dir().join("vision_enabled")
}

/// Enabled only when the trimmed file body is exactly "1".
pub fn parse_enabled(s: &str) -> bool {
    s.trim() == "1"
}

/// Read the persisted flag. Default: disabled (so today's behavior is unchanged).
pub fn is_enabled() -> bool {
    std::fs::read_to_string(flag_path())
        .map(|s| parse_enabled(&s))
        .unwrap_or(false)
}

/// Persist the flag.
pub fn set_enabled(enabled: bool) {
    let _ = std::fs::write(flag_path(), if enabled { "1" } else { "0" });
}

/// Run one scoreboard scan: capture the Dota window, confirm the GSI own hero, and
/// return the payload `{source, observations}` ready to emit to the WebView — or None
/// when the flag is off, Dota/own data is missing, or the own-row oracle fails (in which
/// case nothing is shipped, by design). The observations cover BOTH teams (ally + enemy,
/// each tagged with its `team`), with the player's own row excluded. The WebView stamps
/// `gameTimeStamp` and `type`.
#[cfg(windows)]
pub fn scan_scoreboard_now(own_hero: &str, own_team: &str) -> Option<serde_json::Value> {
    if !is_enabled() {
        return None;
    }
    let hwnd = hwnd::find_dota_hwnd()?;
    // WGC runs a blocking message loop → capture on a dedicated thread.
    let rgba = std::thread::spawn(move || capture::capture_window_rgba(hwnd))
        .join()
        .ok()?
        .ok()?;
    let frame = image::DynamicImage::ImageRgba8(rgba);
    let obs = pipeline::scan_scoreboard(&frame, own_hero, own_team, color_refs::load())?;
    if obs.is_empty() {
        return None;
    }
    Some(serde_json::json!({ "source": "scoreboard", "observations": obs }))
}

/// One top-bar composition scan. `cached` is the caller-held anchor geometry (steady state
/// = cheap verify). Returns the payload plus the refreshed geometry to cache.
#[cfg(windows)]
pub fn scan_topbar_now(own_hero: &str, own_team: &str, cached: Option<&locate::Located>)
    -> Option<(serde_json::Value, locate::Located)> {
    if !is_enabled() { return None; }
    let hwnd = hwnd::find_dota_hwnd()?;
    // WGC runs a blocking message loop → capture on a dedicated thread.
    let rgba = std::thread::spawn(move || capture::capture_window_rgba(hwnd))
        .join()
        .ok()?
        .ok()?;
    let frame = image::DynamicImage::ImageRgba8(rgba);
    let (obs, loc) = topbar::scan_topbar(&frame, own_hero, own_team, topbar_refs::load(), cached)?;
    if obs.is_empty() { return None; }
    Some((serde_json::json!({ "source": "topbar", "observations": obs }), loc))
}

/// Top-bar pixel rect (x, y, w, h) inside a captured frame — both teams' hero
/// portraits. Fractions measured on a 1918x1078 reference, scale-invariant.
fn topbar_rect(cap_w: u32, cap_h: u32) -> (u32, u32, u32, u32) {
    const FX: f64 = 0.240;
    const FY: f64 = 0.000;
    const FW: f64 = 0.520;
    const FH: f64 = 0.075;
    let w = cap_w as f64;
    let h = cap_h as f64;
    (
        (FX * w) as u32,
        (FY * h) as u32,
        (FW * w) as u32,
        (FH * h) as u32,
    )
}

/// Downscale so the long side is <= `max` px (never upscale).
#[cfg(windows)]
fn downscale_max_side(img: image::RgbaImage, max: u32) -> image::RgbaImage {
    let (w, h) = (img.width(), img.height());
    let side = w.max(h);
    if side <= max {
        return img;
    }
    let s = max as f32 / side as f32;
    image::imageops::resize(
        &img,
        (w as f32 * s) as u32,
        (h as f32 * s) as u32,
        image::imageops::FilterType::Triangle,
    )
}

/// Capture the Dota window once, crop the top bar (both teams' hero portraits), and
/// return it as a base64 JPEG for the server's roster-recognition VLM. On-demand
/// one-shot — not part of the CV capture loop.
#[cfg(windows)]
pub fn snapshot_topbar() -> Result<String, String> {
    use base64::Engine as _;

    if !is_enabled() {
        return Err("vision disabled".into());
    }
    let hwnd = hwnd::find_dota_hwnd().ok_or("game window not found")?;
    // WGC runs a blocking message loop → capture on a dedicated thread.
    let img = std::thread::spawn(move || capture::capture_window_rgba(hwnd))
        .join()
        .map_err(|_| "capture thread panicked".to_string())??;
    let (cap_w, cap_h) = (img.width(), img.height());
    let (x, y, w, h) = topbar_rect(cap_w, cap_h);
    let sub = image::imageops::crop_imm(&img, x, y, w, h).to_image();
    let sub = downscale_max_side(sub, 768);
    let mut buf = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(sub)
        .to_rgb8() // JPEG has no alpha
        .write_to(&mut buf, image::ImageFormat::Jpeg)
        .map_err(|e| e.to_string())?;
    Ok(base64::engine::general_purpose::STANDARD.encode(buf.into_inner()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_enabled_only_for_one() {
        assert!(parse_enabled("1"));
        assert!(parse_enabled(" 1\r\n"));
        assert!(!parse_enabled("0"));
        assert!(!parse_enabled(""));
        assert!(!parse_enabled("true"));
    }

    #[test]
    fn topbar_rect_is_within_frame_and_non_degenerate() {
        let r = topbar_rect(1918, 1078);
        assert!(r.0 + r.2 <= 1918, "x+w within frame: {r:?}");
        assert!(r.1 + r.3 <= 1078, "y+h within frame: {r:?}");
        assert!(r.2 > 100, "w not degenerate: {r:?}");
        assert!(r.3 > 40, "h not degenerate: {r:?}");
    }
}

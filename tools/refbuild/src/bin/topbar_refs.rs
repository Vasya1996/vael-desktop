//! CALIBRATE + bake the top-bar hero color-reference library from the fresh VPK
//! top-bar art dump (128x72 PNGs, base + `_altN` arcana variants).
//!
//! Cell anatomy (pixel-scanned on real 1920x1080 frames, scale 1.0, cell 60x40):
//! rows 0-3 are the player-color strip (bright saturated uniform band), rows 4-5 a
//! dark shadow seam under it, rows 6-39 the hero art. As cell-height fractions:
//! strip+seam = 0.15, art band = 0.85 -> 60x34 (aspect 1.76 ~ the art's 1.78). The
//! query is therefore the ART BAND ONLY; calibration grid-searches which vertical
//! window of the 128x72 source art that band corresponds to.
//!
//! Variants: `_alt[N]` (arcanas — e.g. Juggernaut's Bladeform Legacy replaces his
//! top-bar portrait entirely), `_persona<N>` (personas) and `_carnival` (event art)
//! are kept as EXTRA templates of the same hero: the variant
//! suffix is stripped, so the library carries duplicate keys. Ranking collapses
//! variants to the best score per hero and the confidence margin is INTER-HERO
//! (best hero1 - best hero2), never variant-vs-variant of one hero.
//!
//! usage:
//!   topbar_refs calibrate <topbar_art_dir> <keys_table.txt> <frames_dir> <anchor.bin>
//!   topbar_refs emit      <topbar_art_dir> <keys_table.txt> <y0_frac> <h_frac> <out.bin>
//!   topbar_refs report-pair <topbar_art_dir> <keys_table.txt> <frames_dir> <anchor.bin> <y0_frac> <h_frac>
//!
//! Blob format written by `emit` (little-endian, same as `heroes_color_refs.bin`,
//! one entry per VARIANT under its base hero key — duplicate keys are expected):
//!   magic "VAELCREF", version u8, w u16, h u16, count u16,
//!   then count x { key_len u8, key bytes (utf8), w*h*3 RGB bytes }.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use image::imageops::{self, FilterType};
use image::{DynamicImage, GenericImageView, RgbImage};

/// Canonical top-bar match size, chosen to match the art band's aspect (60x34 -> 1.76).
const TB_W: u32 = 48;
const TB_H: u32 = 27;

/// Top of the hero-art band inside a portrait cell, as a fraction of cell height:
/// player-color strip (0.10) + dark shadow seam (0.05), measured by pixel scan.
const CELL_ART_Y0_FRAC: f64 = 0.15;

/// Dead-hero detection: the HUD desaturates a dead hero's portrait to grayscale, so
/// its art band carries no color for the color matcher — such cells are UNREADABLE
/// and excluded from calibration (they'd score near-chance against any color ref).
/// Mean per-pixel saturation (max-min channel) measured across all 120 calibration
/// cells is cleanly bimodal: dead <= 19.2, alive >= 25.4; 22.0 splits the gap.
const MIN_LIVE_SAT: f32 = 22.0;

/// Mean per-pixel saturation (max channel - min channel) of a query art band.
fn art_band_saturation(q: &RgbImage) -> f32 {
    let n = (q.width() * q.height()) as f32;
    q.pixels()
        .map(|p| {
            let mx = p.0.iter().max().copied().unwrap() as f32;
            let mn = p.0.iter().min().copied().unwrap() as f32;
            mx - mn
        })
        .sum::<f32>()
        / n
}

// ---------------------------------------------------------------------------------
// Copied from `refbuild/src/main.rs`: pin the canonical hero keys from the bundled
// `key,base64` table (drops persona/summon/coach textures with invalid keys).
// ---------------------------------------------------------------------------------
fn parse_keys(table: &str) -> Vec<String> {
    let mut v = Vec::new();
    for line in table.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, _)) = line.split_once(',') {
            let k = k.trim();
            if k.starts_with("npc_dota_hero_") {
                v.push(k.to_string());
            }
        }
    }
    v
}

/// Strip a trailing art-variant suffix, if any. The VPK ships alternate top-bar
/// portraits under the base hero name plus a suffix: `_alt<N>` / bare `_alt` (arcana
/// art), `_persona<N>` (persona art), `_carnival` (event art). All collapse to the
/// base hero key so they bake as extra variants of the same hero.
fn strip_variant_suffix(stem: &str) -> &str {
    if let Some(base) = stem.strip_suffix("_carnival") {
        return base;
    }
    if let Some(i) = stem.rfind("_persona") {
        if !stem[i + 8..].is_empty() && stem[i + 8..].chars().all(|c| c.is_ascii_digit()) {
            return &stem[..i];
        }
    }
    if let Some(i) = stem.rfind("_alt") {
        if stem[i + 4..].chars().all(|c| c.is_ascii_digit()) {
            return &stem[..i];
        }
    }
    stem
}

/// Load all valid art variants from the VPK dump dir: `npc_dota_hero_<hero>_png.png`
/// plus its `_alt[N]`/`_persona<N>`/`_carnival` variants, keyed by the BASE hero key
/// (variant suffix stripped). Files whose base key is not in the canonical table
/// (creeps, summons, coach) are dropped.
fn load_variants(dir: &Path, keys: &[String]) -> Vec<(String, RgbImage)> {
    let keyset: HashSet<&str> = keys.iter().map(|k| k.as_str()).collect();
    let mut out: Vec<(String, RgbImage)> = Vec::new();
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read_dir {:?}: {e}", dir))
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .collect();
    files.sort();
    for p in files {
        let name = match p.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        let stem = match name.strip_suffix("_png.png") {
            Some(s) => s,
            None => continue,
        };
        // Collapse a trailing variant suffix (`_alt[N]`, `_persona<N>`, `_carnival`).
        let base = strip_variant_suffix(stem);
        if !keyset.contains(base) {
            continue;
        }
        match image::open(&p) {
            Ok(img) => out.push((base.to_string(), img.to_rgb8())),
            Err(e) => eprintln!("skip {:?}: {e}", p),
        }
    }
    out
}

// ---------------------------------------------------------------------------------
// Copied from `ncc.rs`: unmasked color ZNCC + rank. `best_hero` replaces `ncc::best`
// because the library carries multiple variants per hero (see module doc).
// ---------------------------------------------------------------------------------
fn color_ncc(query: &RgbImage, tmpl: &RgbImage) -> f32 {
    let n = (query.width() * query.height()) as usize;
    if n == 0 || query.dimensions() != tmpl.dimensions() {
        return -1.0;
    }
    let mut total = 0f32;
    for c in 0..3 {
        let q: Vec<f32> = query.pixels().map(|p| p.0[c] as f32).collect();
        let t: Vec<f32> = tmpl.pixels().map(|p| p.0[c] as f32).collect();
        let mq = q.iter().sum::<f32>() / n as f32;
        let mt = t.iter().sum::<f32>() / n as f32;
        let (mut num, mut dq, mut dt) = (0f32, 0f32, 0f32);
        for i in 0..n {
            let a = q[i] - mq;
            let b = t[i] - mt;
            num += a * b;
            dq += a * a;
            dt += b * b;
        }
        let denom = (dq * dt).sqrt();
        if denom > 0.0 {
            total += num / denom;
        }
    }
    total / 3.0
}

fn rank(query: &RgbImage, refs: &[(String, RgbImage)]) -> Vec<(String, f32)> {
    let mut v: Vec<(String, f32)> = refs.iter().map(|(k, t)| (k.clone(), color_ncc(query, t))).collect();
    v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    v
}

/// (best_key, top1 score, INTER-HERO top1-top2 margin) from a descending ranking that
/// may contain several variants of the same hero: variants collapse to the hero's best
/// score, and the margin is taken to the best score of a DIFFERENT hero.
fn best_hero(ranked: &[(String, f32)]) -> (String, f32, f32) {
    let (k0, s0) = ranked.first().cloned().unwrap_or_default();
    let s1 = ranked.iter().find(|(k, _)| *k != k0).map(|(_, s)| *s).unwrap_or(0.0);
    (k0, s0, s0 - s1)
}

// ---------------------------------------------------------------------------------
// Copied from `locate.rs`: anchor parsing + the gated multi-scale locator.
// ---------------------------------------------------------------------------------
const SEARCH_Y_FRAC: f64 = 0.015;
const SEARCH_X0_FRAC: f64 = 0.30;
const SEARCH_X1_FRAC: f64 = 0.70;

const SCALE_MIN: f64 = 0.5;
const SCALE_MAX: f64 = 1.6;
const SCALE_STEP_COARSE: f64 = 0.1;
const SCALE_STEP_FINE: f64 = 0.02;
const STRIDE_COARSE: u32 = 4;
const STRIDE_FINE: u32 = 1;
const SUBSAMPLE_COARSE: u32 = 2;
const SUBSAMPLE_FINE: u32 = 1;

const SCORE_MIN: f32 = 0.55;
const MARGIN_MIN: f32 = 0.12;

struct Anchor {
    w: u32,
    h: u32,
    tmpl: RgbImage,
    mask: Vec<bool>,
}

struct Located {
    x: u32,
    y: u32,
    scale: f64,
    score: f32,
    margin: f32,
}

struct ScaleTmpl {
    scale: f64,
    w: u32,
    h: u32,
    color: RgbImage,
    mask: Vec<bool>,
}

#[derive(Clone)]
struct Candidate {
    x: u32,
    y: u32,
    scale: f64,
    score: f32,
}

fn parse_anchor(data: &[u8]) -> Option<Anchor> {
    if data.len() < 13 || &data[0..8] != b"VAELTANC" {
        return None;
    }
    let w = u16::from_le_bytes([data[9], data[10]]) as u32;
    let h = u16::from_le_bytes([data[11], data[12]]) as u32;
    let tmpl_px = (w as usize).checked_mul(h as usize)?.checked_mul(3)?;
    let mask_px = (w as usize).checked_mul(h as usize)?;
    let tmpl_end = 13usize.checked_add(tmpl_px)?;
    let mask_end = tmpl_end.checked_add(mask_px)?;
    if data.len() < mask_end {
        return None;
    }
    let tmpl = RgbImage::from_raw(w, h, data[13..tmpl_end].to_vec())?;
    let mask: Vec<bool> = data[tmpl_end..mask_end].iter().map(|&b| b != 0).collect();
    Some(Anchor { w, h, tmpl, mask })
}

fn resize_mask(mask: &[bool], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<bool> {
    let mut out = vec![false; (dw * dh) as usize];
    for y in 0..dh {
        for x in 0..dw {
            let sx = (x as u64 * sw as u64 / dw as u64).min(sw as u64 - 1) as u32;
            let sy = (y as u64 * sh as u64 / dh as u64).min(sh as u64 - 1) as u32;
            out[(y * dw + x) as usize] = mask[(sy * sw + sx) as usize];
        }
    }
    out
}

fn masked_color_ncc(q: &RgbImage, t: &RgbImage, mask: &[bool], step: u32) -> f32 {
    let (w, h) = q.dimensions();
    if q.dimensions() != t.dimensions() {
        return -1.0;
    }
    let mut total = 0f32;
    let mut used = 0;
    for c in 0..3 {
        let mut qs = Vec::new();
        let mut ts = Vec::new();
        let mut y = 0;
        while y < h {
            let mut x = 0;
            while x < w {
                if mask[(y * w + x) as usize] {
                    qs.push(q.get_pixel(x, y).0[c] as f32);
                    ts.push(t.get_pixel(x, y).0[c] as f32);
                }
                x += step;
            }
            y += step;
        }
        let n = qs.len() as f32;
        if n < 8.0 {
            continue;
        }
        let mq = qs.iter().sum::<f32>() / n;
        let mt = ts.iter().sum::<f32>() / n;
        let (mut num, mut dq, mut dt) = (0f32, 0f32, 0f32);
        for i in 0..qs.len() {
            let a = qs[i] - mq;
            let b = ts[i] - mt;
            num += a * b;
            dq += a * a;
            dt += b * b;
        }
        let denom = (dq * dt).sqrt();
        if denom > 0.0 {
            total += num / denom;
            used += 1;
        }
    }
    if used == 0 {
        -1.0
    } else {
        total / used as f32
    }
}

fn build_scale_tmpl(a: &Anchor, scale: f64) -> Option<ScaleTmpl> {
    let w = ((a.w as f64) * scale).round().max(8.0) as u32;
    let h = ((a.h as f64) * scale).round().max(4.0) as u32;
    let dyn_img = DynamicImage::ImageRgb8(a.tmpl.clone());
    let color = dyn_img.resize_exact(w, h, FilterType::Lanczos3).to_rgb8();
    let mask = resize_mask(&a.mask, a.w, a.h, w, h);
    Some(ScaleTmpl { scale, w, h, color, mask })
}

fn eval_window(frame: &DynamicImage, x: u32, y: u32, t: &ScaleTmpl, step: u32) -> f32 {
    let window = frame.crop_imm(x, y, t.w, t.h).to_rgb8();
    masked_color_ncc(&window, &t.color, &t.mask, step)
}

fn search_coarse(frame: &DynamicImage, a: &Anchor) -> Vec<Candidate> {
    let (fw, fh) = frame.dimensions();
    let sx0 = (fw as f64 * SEARCH_X0_FRAC) as u32;
    let sx1 = (fw as f64 * SEARCH_X1_FRAC) as u32;
    let sy0 = 0u32;
    let y_top_max = (fh as f64 * SEARCH_Y_FRAC) as u32;

    let mut out = Vec::new();
    let mut s = SCALE_MIN;
    while s <= SCALE_MAX + 1e-9 {
        if let Some(t) = build_scale_tmpl(a, s) {
            if t.w < sx1.saturating_sub(sx0) && t.h < fh {
                let mut y = sy0;
                while y <= y_top_max && y + t.h <= fh {
                    let mut x = sx0;
                    while x + t.w <= sx1 {
                        let score = eval_window(frame, x, y, &t, SUBSAMPLE_COARSE);
                        out.push(Candidate { x, y, scale: t.scale, score });
                        x += STRIDE_COARSE;
                    }
                    y += STRIDE_COARSE;
                }
            }
        }
        s += SCALE_STEP_COARSE;
    }
    out
}

fn top1_margin(cands: &[Candidate], a: &Anchor) -> (Candidate, f32) {
    let mut v: Vec<&Candidate> = cands.iter().collect();
    v.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    let top = v[0].clone();
    let nms_w = a.w as f64 * top.scale * 0.6;
    let nms_h = a.h as f64 * top.scale * 0.6;
    let mut margin = top.score - (-1.0);
    for c in &v[1..] {
        let dx = (c.x as f64 - top.x as f64).abs();
        let dy = (c.y as f64 - top.y as f64).abs();
        if dx > nms_w || dy > nms_h || (c.scale - top.scale).abs() > 0.15 {
            margin = top.score - c.score;
            break;
        }
    }
    (top, margin)
}

fn refine(frame: &DynamicImage, a: &Anchor, coarse: &Candidate) -> Candidate {
    let (fw, fh) = frame.dimensions();
    let mut best = coarse.clone();
    let mut s = (coarse.scale - SCALE_STEP_COARSE).max(SCALE_MIN);
    let s_max = (coarse.scale + SCALE_STEP_COARSE).min(SCALE_MAX);
    while s <= s_max + 1e-9 {
        if let Some(t) = build_scale_tmpl(a, s) {
            let x0 = coarse.x.saturating_sub(STRIDE_COARSE);
            let x1 = (coarse.x + STRIDE_COARSE).min(fw.saturating_sub(t.w));
            let y0 = coarse.y.saturating_sub(STRIDE_COARSE);
            let y1 = (coarse.y + STRIDE_COARSE).min(fh.saturating_sub(t.h));
            let mut y = y0;
            while y <= y1 {
                let mut x = x0;
                while x <= x1 {
                    let score = eval_window(frame, x, y, &t, SUBSAMPLE_FINE);
                    if score > best.score {
                        best = Candidate { x, y, scale: s, score };
                    }
                    x += STRIDE_FINE;
                }
                y += STRIDE_FINE;
            }
        }
        s += SCALE_STEP_FINE;
    }
    best
}

fn locate(frame: &DynamicImage, a: &Anchor) -> Option<Located> {
    let coarse = search_coarse(frame, a);
    if coarse.is_empty() {
        return None;
    }
    let (top, margin) = top1_margin(&coarse, a);
    let fine = refine(frame, a, &top);
    if fine.score < SCORE_MIN || margin < MARGIN_MIN {
        return None;
    }
    Some(Located { x: fine.x, y: fine.y, scale: fine.scale, score: fine.score, margin })
}

// ---------------------------------------------------------------------------------
// Portrait geometry constants, from `topbar_locate.rs` (measured on v1_t480.png,
// 1920x1080; anchor at x=856, y=0, w=209, h=40).
// ---------------------------------------------------------------------------------
const ANCHOR_X_REF: f64 = 856.0;
const ANCHOR_W_REF: f64 = 209.0;
const ANCHOR_H_REF: f64 = 40.0;
const PANEL_CX_REF: f64 = ANCHOR_X_REF + ANCHOR_W_REF / 2.0; // 960.5
const PANEL_CY_REF: f64 = ANCHOR_H_REF / 2.0; // 20.0

const ALLY_X0_REF: f64 = 545.0;
const ALLY_PITCH_REF: f64 = 311.0 / 5.0; // 62.2
const ENEMY_X0_REF: f64 = 1066.0;
const ENEMY_PITCH_REF: f64 = 310.0 / 5.0; // 62.0
const PORTRAIT_W_REF: f64 = 60.0;
const PORTRAIT_H_REF: f64 = 40.0;
const PORTRAIT_Y_REF: f64 = 0.0;

#[derive(Clone, Copy)]
struct Rect {
    x: i64,
    y: i64,
    w: i64,
    h: i64,
}

/// Portrait geometry from the located panel (cx, cy = panel center in frame px; scale
/// = ratio vs. the ANCHOR_W/H reference). No further search — pure proportion math.
fn portrait_rects(cx: f64, cy: f64, scale: f64) -> [Rect; 10] {
    let mut out = [Rect { x: 0, y: 0, w: 0, h: 0 }; 10];
    for i in 0..5 {
        let ref_left = ALLY_X0_REF + i as f64 * ALLY_PITCH_REF;
        let dx = ref_left - PANEL_CX_REF;
        let dy = PORTRAIT_Y_REF - PANEL_CY_REF;
        out[i] = Rect {
            x: (cx + dx * scale).round() as i64,
            y: (cy + dy * scale).round() as i64,
            w: (PORTRAIT_W_REF * scale).round() as i64,
            h: (PORTRAIT_H_REF * scale).round() as i64,
        };
    }
    for i in 0..5 {
        let ref_left = ENEMY_X0_REF + i as f64 * ENEMY_PITCH_REF;
        let dx = ref_left - PANEL_CX_REF;
        let dy = PORTRAIT_Y_REF - PANEL_CY_REF;
        out[5 + i] = Rect {
            x: (cx + dx * scale).round() as i64,
            y: (cy + dy * scale).round() as i64,
            w: (PORTRAIT_W_REF * scale).round() as i64,
            h: (PORTRAIT_H_REF * scale).round() as i64,
        };
    }
    out
}

// ---------------------------------------------------------------------------------
// Candidate builder: a VERTICAL window of the 128x72 source art at full width —
// y0_frac = window top, h_frac = window height, both as fractions of the source
// height (clamped to stay inside) — then resize_exact to TB_W x TB_H, the same
// Lanczos3 path the query art band goes through.
// ---------------------------------------------------------------------------------
fn build_candidate(src: &RgbImage, y0_frac: f64, h_frac: f64) -> RgbImage {
    let (w, h) = src.dimensions();
    let ch = (((h as f64) * h_frac).round().max(1.0) as u32).min(h);
    let y0 = ((((h as f64) * y0_frac).round()).max(0.0) as u32).min(h - ch);
    let cropped = imageops::crop_imm(src, 0, y0, w, ch).to_image();
    DynamicImage::ImageRgb8(cropped).resize_exact(TB_W, TB_H, FilterType::Lanczos3).to_rgb8()
}

fn list_tb_frames(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read_dir {:?}: {e}", dir))
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|e| e.eq_ignore_ascii_case("png")).unwrap_or(false))
        .filter(|p| {
            p.file_stem()
                .map(|s| s.to_string_lossy().starts_with("tb_"))
                .unwrap_or(false)
        })
        .collect();
    v.sort();
    v
}

struct FrameData {
    stem: String,
    match_id: String,
    queries: Vec<RgbImage>, // 10 portrait art-band crops, already TB_W x TB_H
    live: Vec<bool>,        // false = dead-hero (grayscale) cell, excluded from reads
}

/// Locate the anchor on every tb_* frame and extract the 10 art-band queries.
fn load_frames(frames_dir: &Path, anchor: &Anchor) -> Vec<FrameData> {
    let frame_paths = list_tb_frames(frames_dir);
    eprintln!("calibration frames (tb_*.png): {}", frame_paths.len());
    assert_eq!(frame_paths.len(), 12, "expected 12 tb_*.png frames, got {}", frame_paths.len());

    let mut frames_data = Vec::new();
    for p in &frame_paths {
        let stem = p.file_stem().unwrap().to_string_lossy().to_string();
        let match_id = stem.splitn(3, '_').take(2).collect::<Vec<_>>().join("_"); // "tb_1"
        let img = image::open(p).unwrap_or_else(|e| panic!("open {:?}: {e}", p));
        let located = locate(&img, anchor).unwrap_or_else(|| panic!("locate() failed to find the anchor on {:?}", p));
        let cx = located.x as f64 + (anchor.w as f64 * located.scale) / 2.0;
        let cy = located.y as f64 + (anchor.h as f64 * located.scale) / 2.0;
        eprintln!(
            "{:<20} loc=({:.1},{:.1}) scale={:.3} score={:.3} margin={:.3}",
            stem, cx, cy, located.scale, located.score, located.margin
        );

        let (fw, fh) = img.dimensions();
        let mut queries = Vec::with_capacity(10);
        for r in portrait_rects(cx, cy, located.scale) {
            let x0 = (r.x.max(0) as u32).min(fw.saturating_sub(1));
            let y0 = (r.y.max(0) as u32).min(fh.saturating_sub(1));
            let w = (r.w.max(1) as u32).min(fw - x0);
            let h = (r.h.max(1) as u32).min(fh - y0);
            // Query = the ART BAND only: skip the player-color strip + shadow seam.
            let art_y0 = ((h as f64) * CELL_ART_Y0_FRAC).round() as u32;
            let art_h = h - art_y0;
            let crop = img.crop_imm(x0, y0 + art_y0, w, art_h);
            let q = crop.resize_exact(TB_W, TB_H, FilterType::Lanczos3).to_rgb8();
            queries.push(q);
        }
        let live: Vec<bool> = queries.iter().map(|q| art_band_saturation(q) >= MIN_LIVE_SAT).collect();
        for (slot, alive) in live.iter().enumerate() {
            if !alive {
                eprintln!("  {} slot {} is DEAD (grayscale) -> excluded from reads", stem, slot);
            }
        }
        frames_data.push(FrameData { stem, match_id, queries, live });
    }
    frames_data
}

fn load_sources(art_dir: &str, table_path: &str) -> Vec<(String, RgbImage)> {
    let table = std::fs::read_to_string(table_path).unwrap_or_else(|e| panic!("read {table_path}: {e}"));
    let mut keys = parse_keys(&table);
    // dawnbreaker shipped after the bundled phash table was generated and is missing
    // from it; her top-bar art is real, so pin the key explicitly.
    if !keys.iter().any(|k| k == "npc_dota_hero_dawnbreaker") {
        keys.push("npc_dota_hero_dawnbreaker".to_string());
    }
    let variants = load_variants(Path::new(art_dir), &keys);
    let distinct: HashSet<&str> = variants.iter().map(|(k, _)| k.as_str()).collect();
    eprintln!(
        "source art: {} variants over {} distinct heroes (canonical table: {})",
        variants.len(),
        distinct.len(),
        keys.len()
    );
    assert!(distinct.len() >= 120, "expected ~126 distinct heroes, got {}", distinct.len());
    variants
}

fn cmd_calibrate(args: &[String]) {
    if args.len() < 4 {
        eprintln!("usage: calibrate <topbar_art_dir> <keys_table.txt> <frames_dir> <anchor.bin>");
        std::process::exit(2);
    }
    let source_refs = load_sources(&args[0], &args[1]);
    let anchor_blob = std::fs::read(&args[3]).unwrap_or_else(|e| panic!("read {}: {e}", args[3]));
    let anchor = parse_anchor(&anchor_blob).expect("parse anchor.bin");
    let frames_data = load_frames(Path::new(&args[2]), &anchor);

    // 2D grid over the source art's VERTICAL window. If the winner lands on a grid
    // boundary, the grid is extended one step in that direction (once) — a wider
    // search, not a weaker gate.
    let mut y0_fracs: Vec<f64> = vec![0.0, 0.05, 0.10, 0.15, 0.20];
    let mut h_fracs: Vec<f64> = vec![0.75, 0.80, 0.85, 0.90, 0.95];

    struct PairResult {
        y0: f64,
        hf: f64,
        mean_margin: f32,
        consist: usize,
        distinct: usize,
        jugg_ok: usize,
        pass: bool,
    }

    let eval_pair = |y0f: f64, hf: f64| -> PairResult {
        let candidate_lib: Vec<(String, RgbImage)> =
            source_refs.iter().map(|(k, img)| (k.clone(), build_candidate(img, y0f, hf))).collect();

        let mut margins = Vec::new();
        let mut distinct_violations = 0usize;
        let mut jugg_ok = 0usize;
        let mut match_slot_keys: HashMap<(String, usize), Vec<String>> = HashMap::new();

        for fd in &frames_data {
            let mut frame_keys: Vec<String> = Vec::with_capacity(10);
            for (slot, q) in fd.queries.iter().enumerate() {
                if !fd.live[slot] {
                    continue; // dead (grayscale) cell -> no read
                }
                let ranked = rank(q, &candidate_lib);
                let (key, _top1, margin) = best_hero(&ranked);
                margins.push(margin);
                frame_keys.push(key.clone());
                match_slot_keys.entry((fd.match_id.clone(), slot)).or_default().push(key);
            }
            let mut sorted = frame_keys.clone();
            sorted.sort();
            let n = sorted.len();
            sorted.dedup();
            if sorted.len() != n {
                distinct_violations += 1;
            }
            let jugg_count = frame_keys.iter().filter(|k| k.as_str() == "npc_dota_hero_juggernaut").count();
            if jugg_count == 1 {
                jugg_ok += 1;
            }
        }

        let mut consistency_violations = 0usize;
        for keys in match_slot_keys.values() {
            let mut uniq = keys.clone();
            uniq.sort();
            uniq.dedup();
            if uniq.len() > 1 {
                consistency_violations += 1;
            }
        }

        let mean_margin = margins.iter().sum::<f32>() / margins.len() as f32;
        let pass = mean_margin >= 0.15
            && consistency_violations == 0
            && distinct_violations == 0
            && jugg_ok == frames_data.len();
        PairResult {
            y0: y0f,
            hf,
            mean_margin,
            consist: consistency_violations,
            distinct: distinct_violations,
            jugg_ok,
            pass,
        }
    };

    println!(
        "\n{:<9} {:<8} {:>12} {:>14} {:>14} {:>10} {:<8}",
        "y0_frac", "h_frac", "mean_margin", "consist_viol", "distinct_viol", "jugg_ok/12", "verdict"
    );

    let mut results: Vec<PairResult> = Vec::new();
    for &y0f in &y0_fracs {
        for &hf in &h_fracs {
            let r = eval_pair(y0f, hf);
            println!(
                "{:<9.3} {:<8.3} {:>12.4} {:>14} {:>14} {:>10} {:<8}",
                r.y0, r.hf, r.mean_margin, r.consist, r.distinct, r.jugg_ok,
                if r.pass { "PASS" } else { "fail" }
            );
            results.push(r);
        }
    }

    // One-time boundary extension: if the best pair (by mean margin, preferring
    // gate-passers) sits on the grid edge, add one more step past that edge.
    let best_of = |rs: &[PairResult]| -> (f64, f64) {
        let pool: Vec<&PairResult> = if rs.iter().any(|r| r.pass) {
            rs.iter().filter(|r| r.pass).collect()
        } else {
            rs.iter().collect()
        };
        // First-of-equal-maxima (see the winner selection below for why ties are real).
        let b = pool
            .iter()
            .fold(None::<&&PairResult>, |acc, r| match acc {
                Some(b) if b.mean_margin >= r.mean_margin => Some(b),
                _ => Some(r),
            })
            .unwrap();
        (b.y0, b.hf)
    };
    let (by0, bhf) = best_of(&results);
    let mut extra_y0: Vec<f64> = Vec::new();
    let mut extra_hf: Vec<f64> = Vec::new();
    if by0 == *y0_fracs.first().unwrap() && by0 > 0.0 {
        extra_y0.push((by0 - 0.05).max(0.0));
    }
    if by0 == *y0_fracs.last().unwrap() {
        extra_y0.push(by0 + 0.05);
    }
    if bhf == *h_fracs.first().unwrap() {
        extra_hf.push(bhf - 0.05);
    }
    if bhf == *h_fracs.last().unwrap() && bhf < 1.0 {
        extra_hf.push((bhf + 0.05).min(1.0));
    }
    if !extra_y0.is_empty() || !extra_hf.is_empty() {
        println!("\nwinner on grid boundary -> extending grid once: extra y0 {extra_y0:?}, extra h {extra_hf:?}");
        let old_y0 = y0_fracs.clone();
        let old_hf = h_fracs.clone();
        y0_fracs.extend(&extra_y0);
        h_fracs.extend(&extra_hf);
        for &y0f in &y0_fracs {
            for &hf in &h_fracs {
                if old_y0.contains(&y0f) && old_hf.contains(&hf) {
                    continue; // already evaluated
                }
                let r = eval_pair(y0f, hf);
                println!(
                    "{:<9.3} {:<8.3} {:>12.4} {:>14} {:>14} {:>10} {:<8}",
                    r.y0, r.hf, r.mean_margin, r.consist, r.distinct, r.jugg_ok,
                    if r.pass { "PASS" } else { "fail" }
                );
                results.push(r);
            }
        }
    }

    // First-of-equal-maxima wins: y0 clamping makes several grid points collapse to
    // the same effective window, so ties are real — report the canonical (lowest) params.
    let winner = results.iter().filter(|r| r.pass).fold(None::<&PairResult>, |acc, r| match acc {
        Some(b) if b.mean_margin >= r.mean_margin => Some(b),
        _ => Some(r),
    });
    match winner {
        Some(w) => {
            println!("\nWINNER: y0_frac={:.3} h_frac={:.3} mean_margin={:.4}", w.y0, w.hf, w.mean_margin);
        }
        None => {
            println!("\nNO CANDIDATE PASSED THE GATE — do not bake.");
            std::process::exit(1);
        }
    }
}

fn cmd_emit(args: &[String]) {
    if args.len() < 5 {
        eprintln!("usage: emit <topbar_art_dir> <keys_table.txt> <y0_frac> <h_frac> <out.bin>");
        std::process::exit(2);
    }
    let source_refs = load_sources(&args[0], &args[1]);
    let y0_frac: f64 = args[2].parse().expect("y0_frac");
    let h_frac: f64 = args[3].parse().expect("h_frac");

    let entries: Vec<(String, Vec<u8>)> = source_refs
        .iter()
        .map(|(key, img)| (key.clone(), build_candidate(img, y0_frac, h_frac).into_raw()))
        .collect();
    eprintln!("packed entries: {}", entries.len());

    let mut buf: Vec<u8> = Vec::new();
    buf.extend_from_slice(b"VAELCREF");
    buf.push(1u8);
    buf.extend_from_slice(&(TB_W as u16).to_le_bytes());
    buf.extend_from_slice(&(TB_H as u16).to_le_bytes());
    buf.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    for (key, rgb) in &entries {
        let kb = key.as_bytes();
        assert!(kb.len() <= 255, "key too long: {key}");
        assert_eq!(rgb.len(), (TB_W * TB_H * 3) as usize);
        buf.push(kb.len() as u8);
        buf.extend_from_slice(kb);
        buf.extend_from_slice(rgb);
    }

    std::fs::File::create(&args[4])
        .and_then(|mut f| f.write_all(&buf))
        .unwrap_or_else(|e| panic!("write {}: {e}", args[4]));
    eprintln!("wrote {} bytes to {}", buf.len(), &args[4]);
}

/// Print every frame's per-slot best hero + score + inter-hero margin for a given
/// (y0_frac, h_frac) pair — the human-readable detail behind one calibrate table row.
fn cmd_report_pair(args: &[String]) {
    if args.len() < 6 {
        eprintln!("usage: report-pair <topbar_art_dir> <keys_table.txt> <frames_dir> <anchor.bin> <y0_frac> <h_frac>");
        std::process::exit(2);
    }
    let source_refs = load_sources(&args[0], &args[1]);
    let anchor_blob = std::fs::read(&args[3]).unwrap_or_else(|e| panic!("read {}: {e}", args[3]));
    let anchor = parse_anchor(&anchor_blob).expect("parse anchor.bin");
    let frames_data = load_frames(Path::new(&args[2]), &anchor);
    let y0f: f64 = args[4].parse().unwrap();
    let hf: f64 = args[5].parse().unwrap();

    let candidate_lib: Vec<(String, RgbImage)> =
        source_refs.iter().map(|(k, im)| (k.clone(), build_candidate(im, y0f, hf))).collect();

    for fd in &frames_data {
        let mut cells = Vec::new();
        for (slot, q) in fd.queries.iter().enumerate() {
            if !fd.live[slot] {
                cells.push("DEAD".to_string());
                continue;
            }
            let ranked = rank(q, &candidate_lib);
            let (k, s, m) = best_hero(&ranked);
            cells.push(format!("{}({:.2}/{:.2})", k.trim_start_matches("npc_dota_hero_"), s, m));
        }
        println!("{}:\n  ally  {}\n  enemy {}", fd.stem, cells[..5].join(" "), cells[5..].join(" "));
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: topbar_refs calibrate|emit|report-pair ...");
        std::process::exit(2);
    }
    match args[1].as_str() {
        "calibrate" => cmd_calibrate(&args[2..]),
        "emit" => cmd_emit(&args[2..]),
        "report-pair" => cmd_report_pair(&args[2..]),
        other => {
            eprintln!("unknown mode {other:?} (expected calibrate|emit|report-pair)");
            std::process::exit(2);
        }
    }
}

//! Task 6: end-to-end top-bar regression harness (dataset acceptance run).
//!
//! Standalone bin (same convention as `topbar_locate.rs` / `topbar_refs.rs`): copies
//! the anchor locator (`vision/locate.rs`), the runtime portrait geometry
//! (`vision/topbar.rs::portrait_rects`, anchor-top-left based — NOT the panel-center
//! geometry used by the older `topbar_refs.rs` calibration bin), the art-band crop +
//! dead-cell saturation gate (`vision/topbar.rs`), the 48x27 Lanczos3 prep
//! (`vision/topbar_refs.rs::prep_query`), the inter-hero variant-collapse ranking
//! (`vision/topbar_refs.rs::best_hero`) and the classify gate (`vision/recognize.rs`),
//! so recognition here mirrors the runtime pipeline exactly. Consumes the two BAKED
//! assets (`topbar_anchor.bin`, `topbar_color_refs.bin`) directly — no VPK rebuild.
//!
//! usage: topbar_scan <anchor.bin> <topbar_refs.bin> <frames_dir> <out_dir> [extra_dir ...]
//!   frames_dir   the native (as-shot) tb_*.png set — also the "native" resolution set.
//!   out_dir      overlays are written here, one subfolder per resolution/negative set.
//!   extra_dir    additional tb_*.png sets (e.g. rescaled 720p/1440p) to run + compare
//!                for cross-resolution consistency, OR a "negatives" set (any dir whose
//!                basename contains "negativ", case-insensitive) run through the same
//!                pipeline but scored against the negative acceptance rule instead.

use image::imageops::FilterType;
use image::{DynamicImage, GenericImageView, Rgb, RgbImage};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------------
// Anchor locator — copied from `desktop/src-tauri/src/vision/locate.rs`.
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

/// Unconditional locate: always returns the best (fine-refined) candidate + its
/// top1-top2 margin, even if it fails the SCORE_MIN/MARGIN_MIN gate — the harness
/// needs to report the ACTUAL score/margin achieved on a miss, not just "not found".
fn locate_raw(frame: &DynamicImage, a: &Anchor) -> Option<Located> {
    let coarse = search_coarse(frame, a);
    if coarse.is_empty() {
        return None;
    }
    let (top, margin) = top1_margin(&coarse, a);
    let fine = refine(frame, a, &top);
    Some(Located { x: fine.x, y: fine.y, scale: fine.scale, score: fine.score, margin })
}

fn anchor_gate_passed(loc: &Located) -> bool {
    loc.score >= SCORE_MIN && loc.margin >= MARGIN_MIN
}

// ---------------------------------------------------------------------------------
// Portrait geometry — copied from `desktop/src-tauri/src/vision/topbar.rs`
// (anchor-top-left based; this is what the RUNTIME pipeline actually uses).
// ---------------------------------------------------------------------------------
const ALLY_DX: f64 = -311.0;
const ALLY_PITCH: f64 = 62.2;
const ENEMY_DX: f64 = 210.0;
const ENEMY_PITCH: f64 = 62.0;
const CELL_W: f64 = 60.0;
const CELL_H: f64 = 40.0;

/// Skip the top 15% (player-color strip + shadow seam) of a portrait cell — the query
/// is the ART BAND only. Mirrors `topbar.rs::CELL_ART_Y0_FRAC`.
const CELL_ART_Y0_FRAC: f64 = 0.15;

/// Dead-hero (desaturated) cell floor. Mirrors `topbar.rs::MIN_LIVE_SAT`.
const MIN_LIVE_SAT: f32 = 20.5;

/// GSI own-hero oracle threshold used at runtime for VERIFYING a known identity.
/// Per the coordinator's calibration finding, Juggernaut's arcana top-bar art in this
/// dataset needs this looser floor (not `recognize::PEAK_MIN` 0.45) to read as top1.
const OWN_PEAK_MIN: f32 = 0.25;

/// `recognize::classify` gate — `known` requires both.
const PEAK_MIN: f32 = 0.45;
const CLASSIFY_MARGIN_MIN: f32 = 0.15;

#[derive(Clone, Copy)]
struct Rect {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

fn portrait_rects(loc: &Located) -> [Rect; 10] {
    let mut out = [Rect { x: 0, y: 0, w: 0, h: 0 }; 10];
    let w = (CELL_W * loc.scale).round().max(1.0) as u32;
    let h = (CELL_H * loc.scale).round().max(1.0) as u32;
    for i in 0..5u32 {
        let x = loc.x as f64 + (ALLY_DX + i as f64 * ALLY_PITCH) * loc.scale;
        out[i as usize] = Rect { x: x.round().max(0.0) as u32, y: loc.y, w, h };
    }
    for i in 0..5u32 {
        let x = loc.x as f64 + (ENEMY_DX + i as f64 * ENEMY_PITCH) * loc.scale;
        out[5 + i as usize] = Rect { x: x.round().max(0.0) as u32, y: loc.y, w, h };
    }
    out
}

fn art_band_crop(frame: &DynamicImage, cell: Rect) -> Option<DynamicImage> {
    let (fw, fh) = frame.dimensions();
    if cell.x + cell.w > fw || cell.y + cell.h > fh {
        return None;
    }
    let art_y0 = ((cell.h as f64) * CELL_ART_Y0_FRAC).round() as u32;
    let art_h = cell.h.saturating_sub(art_y0);
    if art_h == 0 {
        return None;
    }
    Some(frame.crop_imm(cell.x, cell.y + art_y0, cell.w, art_h))
}

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

fn prep_query(img: &DynamicImage) -> RgbImage {
    img.resize_exact(48, 27, FilterType::Lanczos3).to_rgb8()
}

// ---------------------------------------------------------------------------------
// Color ZNCC ranking — copied from `vision/ncc.rs` (color_ncc) +
// `vision/topbar_refs.rs` (best_hero variant-collapse, inter-hero margin).
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

fn best_hero(ranked: &[(String, f32)]) -> (String, f32, f32) {
    let (k0, s0) = ranked.first().cloned().unwrap_or_default();
    let s1 = ranked.iter().find(|(k, _)| *k != k0).map(|(_, s)| *s).unwrap_or(0.0);
    (k0, s0, s0 - s1)
}

fn classify(top1: f32, margin: f32) -> &'static str {
    if top1 >= PEAK_MIN && margin >= CLASSIFY_MARGIN_MIN {
        "known"
    } else {
        "weak"
    }
}

fn parse_refs_blob(data: &[u8]) -> Vec<(String, RgbImage)> {
    let mut out = Vec::new();
    if data.len() < 15 || &data[0..8] != b"VAELCREF" {
        return out;
    }
    let w = u16::from_le_bytes([data[9], data[10]]) as u32;
    let h = u16::from_le_bytes([data[11], data[12]]) as u32;
    let count = u16::from_le_bytes([data[13], data[14]]) as usize;
    let px = match (w as usize).checked_mul(h as usize).and_then(|n| n.checked_mul(3)) {
        Some(p) if p > 0 => p,
        _ => return out,
    };
    let mut p = 15usize;
    for _ in 0..count {
        if p >= data.len() {
            break;
        }
        let kl = data[p] as usize;
        p += 1;
        if p + kl + px > data.len() {
            break;
        }
        let key = match std::str::from_utf8(&data[p..p + kl]) {
            Ok(s) => s.to_string(),
            Err(_) => break,
        };
        p += kl;
        let raw = data[p..p + px].to_vec();
        p += px;
        if let Some(img) = RgbImage::from_raw(w, h, raw) {
            out.push((key, img));
        }
    }
    out
}

// ---------------------------------------------------------------------------------
// Overlay drawing — copied from `topbar_locate.rs`.
// ---------------------------------------------------------------------------------
fn put(img: &mut RgbImage, x: i64, y: i64, color: Rgb<u8>) {
    if x >= 0 && y >= 0 && (x as u32) < img.width() && (y as u32) < img.height() {
        img.put_pixel(x as u32, y as u32, color);
    }
}

fn draw_rect(img: &mut RgbImage, x: i64, y: i64, w: i64, h: i64, color: Rgb<u8>, thickness: i64) {
    for t in 0..thickness {
        for xx in x..x + w {
            put(img, xx, y + t, color);
            put(img, xx, y + h - 1 - t, color);
        }
        for yy in y..y + h {
            put(img, x + t, yy, color);
            put(img, x + w - 1 - t, yy, color);
        }
    }
}

// ---------------------------------------------------------------------------------
// Per-slot + per-frame result bookkeeping.
// ---------------------------------------------------------------------------------
#[derive(Clone)]
struct SlotRead {
    dead: bool, // desaturated / out-of-frame -> no guess
    hero: String,
    top1: f32,
    margin: f32,
    status: &'static str, // "known" | "weak" | "dead"
}

struct FrameResult {
    res_label: String,
    stem: String,
    match_id: String,
    located: bool,
    cx: f64,
    cy: f64,
    scale: f64,
    score: f32,
    margin: f32,
    slots: [SlotRead; 10],
}

fn short(key: &str) -> &str {
    key.trim_start_matches("npc_dota_hero_")
}

fn list_pngs(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read_dir {:?}: {e}", dir))
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|e| e.eq_ignore_ascii_case("png")).unwrap_or(false))
        .collect();
    v.sort();
    v
}

fn list_tb_pngs(dir: &Path) -> Vec<PathBuf> {
    list_pngs(dir)
        .into_iter()
        .filter(|p| {
            p.file_stem().map(|s| s.to_string_lossy().starts_with("tb_")).unwrap_or(false)
        })
        .collect()
}

fn match_id_of(stem: &str) -> String {
    stem.splitn(3, '_').take(2).collect::<Vec<_>>().join("_")
}

/// Process one frame: locate + read all 10 slots + save overlay. Returns the result.
fn process_frame(
    path: &Path,
    res_label: &str,
    anchor: &Anchor,
    refs: &[(String, RgbImage)],
    out_dir: &Path,
) -> FrameResult {
    let img = image::open(path).unwrap_or_else(|e| panic!("open {:?}: {e}", path));
    let stem = path.file_stem().unwrap().to_string_lossy().to_string();
    let match_id = match_id_of(&stem);

    let loc = locate_raw(&img, anchor);
    let mut overlay = img.to_rgb8();

    let (located, cx, cy, scale, score, margin, slots) = match &loc {
        None => {
            eprintln!("  {} [{}]: locate_raw returned no candidates at all", res_label, stem);
            (false, 0.0, 0.0, 0.0, -1.0f32, -1.0f32, std::array::from_fn(|_| SlotRead {
                dead: true,
                hero: String::new(),
                top1: 0.0,
                margin: 0.0,
                status: "dead",
            }))
        }
        Some(l) => {
            let gate_ok = anchor_gate_passed(l);
            let cx = l.x as f64 + (anchor.w as f64 * l.scale) / 2.0;
            let cy = l.y as f64 + (anchor.h as f64 * l.scale) / 2.0;

            let anchor_w = (anchor.w as f64 * l.scale).round() as i64;
            let anchor_h = (anchor.h as f64 * l.scale).round() as i64;
            draw_rect(&mut overlay, l.x as i64, l.y as i64, anchor_w, anchor_h, Rgb([255, 255, 0]), 2);

            let rects = portrait_rects(l);
            let slots: [SlotRead; 10] = std::array::from_fn(|i| {
                let cell = rects[i];
                draw_rect(&mut overlay, cell.x as i64, cell.y as i64, cell.w as i64, cell.h as i64, Rgb([0, 255, 0]), 2);
                match art_band_crop(&img, cell) {
                    None => SlotRead { dead: true, hero: String::new(), top1: 0.0, margin: 0.0, status: "dead" },
                    Some(crop) => {
                        let q = prep_query(&crop);
                        if art_band_saturation(&q) < MIN_LIVE_SAT {
                            SlotRead { dead: true, hero: String::new(), top1: 0.0, margin: 0.0, status: "dead" }
                        } else {
                            let ranked = rank(&q, refs);
                            let (hero, top1, marg) = best_hero(&ranked);
                            let status = classify(top1, marg);
                            SlotRead { dead: false, hero, top1, margin: marg, status }
                        }
                    }
                }
            });
            (gate_ok, cx, cy, l.scale, l.score, l.margin, slots)
        }
    };

    let out_sub = out_dir.join(res_label);
    let _ = std::fs::create_dir_all(&out_sub);
    let out_name = format!("{}_score{:.3}.png", stem, score.max(-1.0));
    if let Err(e) = overlay.save(out_sub.join(&out_name)) {
        eprintln!("save overlay {:?}: {e}", out_sub.join(&out_name));
    }

    FrameResult { res_label: res_label.to_string(), stem, match_id, located, cx, cy, scale, score, margin, slots }
}

fn print_frame(fr: &FrameResult) {
    println!(
        "[{:<8}] {:<22} anchor cx={:>7.1} cy={:>5.1} scale={:.3} score={:>6.3} margin={:>6.3} located={}",
        fr.res_label, fr.stem, fr.cx, fr.cy, fr.scale, fr.score, fr.margin, if fr.located { "YES" } else { "NO " }
    );
    let fmt_slot = |i: usize| -> String {
        let s = &fr.slots[i];
        if s.dead {
            format!("{}:DEAD", i)
        } else {
            format!("{}:{}({:.2}/{:.2}/{})", i, short(&s.hero), s.top1, s.margin, s.status)
        }
    };
    let ally: Vec<String> = (0..5).map(fmt_slot).collect();
    let enemy: Vec<String> = (5..10).map(fmt_slot).collect();
    println!("  ally  {}", ally.join(" "));
    println!("  enemy {}", enemy.join(" "));
}

fn is_negative_dir(dir: &Path) -> bool {
    dir.file_name()
        .map(|n| n.to_string_lossy().to_lowercase().contains("negativ"))
        .unwrap_or(false)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 5 {
        eprintln!("usage: topbar_scan <anchor.bin> <topbar_refs.bin> <frames_dir> <out_dir> [extra_dir ...]");
        std::process::exit(2);
    }
    let anchor_blob = std::fs::read(&args[1]).unwrap_or_else(|e| panic!("read {}: {e}", args[1]));
    let anchor = parse_anchor(&anchor_blob).expect("parse anchor.bin");
    let refs_blob = std::fs::read(&args[2]).unwrap_or_else(|e| panic!("read {}: {e}", args[2]));
    let refs = parse_refs_blob(&refs_blob);
    eprintln!("anchor: {}x{} ; refs: {} entries", anchor.w, anchor.h, refs.len());
    assert!(refs.len() >= 120, "refs blob looks empty/truncated ({} entries)", refs.len());

    let frames_dir = PathBuf::from(&args[3]);
    let out_dir = PathBuf::from(&args[4]);
    std::fs::create_dir_all(&out_dir).expect("mkdir out_dir");

    let mut extra_dirs: Vec<PathBuf> = args[5..].iter().map(PathBuf::from).collect();

    // --- Positive (tb_*) resolution sets: native + any extra dir that isn't "negatives".
    let mut positive_results: Vec<FrameResult> = Vec::new();
    let native_label = frames_dir.file_name().unwrap().to_string_lossy().to_string();
    println!("\n=== resolution set: {} ({:?}) ===", native_label, frames_dir);
    for p in list_tb_pngs(&frames_dir) {
        let fr = process_frame(&p, &native_label, &anchor, &refs, &out_dir);
        print_frame(&fr);
        positive_results.push(fr);
    }

    let mut negative_dirs: Vec<PathBuf> = Vec::new();
    let mut i = 0;
    while i < extra_dirs.len() {
        if is_negative_dir(&extra_dirs[i]) {
            negative_dirs.push(extra_dirs.remove(i));
        } else {
            i += 1;
        }
    }
    for dir in &extra_dirs {
        let label = dir.file_name().unwrap().to_string_lossy().to_string();
        println!("\n=== resolution set: {} ({:?}) ===", label, dir);
        for p in list_tb_pngs(dir) {
            let fr = process_frame(&p, &label, &anchor, &refs, &out_dir);
            print_frame(&fr);
            positive_results.push(fr);
        }
    }

    // --- Acceptance criteria a-d over positive_results ---
    println!("\n=== ACCEPTANCE SUMMARY ===");

    // (a) 36/36 anchor located.
    let total = positive_results.len();
    let located_n = positive_results.iter().filter(|f| f.located).count();
    let misses: Vec<String> = positive_results
        .iter()
        .filter(|f| !f.located)
        .map(|f| format!("{}/{} (score={:.3} margin={:.3})", f.res_label, f.stem, f.score, f.margin))
        .collect();
    println!(
        "(a) anchor located: {}/{} [{}]",
        located_n,
        total,
        if located_n == total && total > 0 { "PASS" } else { "FAIL" }
    );
    for m in &misses {
        println!("    MISS: {}", m);
    }

    // (b) juggernaut top1 exactly one slot per frame, score >= OWN_PEAK_MIN, alive.
    let mut jugg_ok = 0usize;
    let mut jugg_violations: Vec<String> = Vec::new();
    for f in &positive_results {
        let count = f
            .slots
            .iter()
            .filter(|s| !s.dead && s.hero == "npc_dota_hero_juggernaut" && s.top1 >= OWN_PEAK_MIN)
            .count();
        if count == 1 {
            jugg_ok += 1;
        } else {
            jugg_violations.push(format!("{}/{}: juggernaut matched {} slots (expected 1)", f.res_label, f.stem, count));
        }
    }
    println!(
        "(b) juggernaut top1 exactly one slot/frame (score>={:.2}): {}/{} [{}]",
        OWN_PEAK_MIN,
        jugg_ok,
        total,
        if jugg_ok == total && total > 0 { "PASS" } else { "FAIL" }
    );
    for v in &jugg_violations {
        println!("    MISS: {}", v);
    }

    // (c) slot consistency: within each video, every ALIVE slot -> same hero across
    // frames AND across resolutions of the same frame. Dead slots excluded.
    let mut slot_map: HashMap<(String, usize), Vec<(String, String, String)>> = HashMap::new(); // (match_id,slot) -> [(res,stem,hero)]
    for f in &positive_results {
        for (i, s) in f.slots.iter().enumerate() {
            if s.dead {
                continue;
            }
            slot_map
                .entry((f.match_id.clone(), i))
                .or_default()
                .push((f.res_label.clone(), f.stem.clone(), s.hero.clone()));
        }
    }
    let mut consist_violations: Vec<String> = Vec::new();
    let mut consist_dead_reports: Vec<String> = Vec::new();
    for ((mid, slot), entries) in &slot_map {
        let mut heroes: Vec<&str> = entries.iter().map(|(_, _, h)| h.as_str()).collect();
        heroes.sort();
        heroes.dedup();
        if heroes.len() > 1 {
            let detail: Vec<String> = entries.iter().map(|(r, s, h)| format!("{}/{}={}", r, s, short(h))).collect();
            consist_violations.push(format!("{} slot {}: {} distinct heroes -> {}", mid, slot, heroes.len(), detail.join(", ")));
        }
    }
    // report dead-slot coverage per (match, slot) that had zero alive entries at all (excluded from check, informational).
    let all_match_ids: Vec<String> = {
        let mut v: Vec<String> = positive_results.iter().map(|f| f.match_id.clone()).collect();
        v.sort();
        v.dedup();
        v
    };
    for mid in &all_match_ids {
        for slot in 0..10usize {
            if !slot_map.contains_key(&(mid.clone(), slot)) {
                consist_dead_reports.push(format!("{} slot {}: no alive read in any frame/resolution (all dead)", mid, slot));
            }
        }
    }
    println!(
        "(c) slot consistency (video + cross-resolution): {} violations [{}]",
        consist_violations.len(),
        if consist_violations.is_empty() { "PASS" } else { "FAIL" }
    );
    for v in &consist_violations {
        println!("    VIOLATION: {}", v);
    }
    for d in &consist_dead_reports {
        println!("    (dead, excluded) {}", d);
    }

    // (d) all-alive-distinct per frame.
    let mut distinct_ok = 0usize;
    let mut distinct_violations: Vec<String> = Vec::new();
    for f in &positive_results {
        let alive: Vec<&str> = f.slots.iter().filter(|s| !s.dead).map(|s| s.hero.as_str()).collect();
        let mut uniq = alive.clone();
        uniq.sort();
        uniq.dedup();
        if uniq.len() == alive.len() {
            distinct_ok += 1;
        } else {
            distinct_violations.push(format!("{}/{}: {} alive slots, {} distinct heroes", f.res_label, f.stem, alive.len(), uniq.len()));
        }
    }
    println!(
        "(d) all-alive-distinct per frame: {}/{} [{}]",
        distinct_ok,
        total,
        if distinct_ok == total && total > 0 { "PASS" } else { "FAIL" }
    );
    for v in &distinct_violations {
        println!("    VIOLATION: {}", v);
    }

    // --- (e) Negatives ---
    println!("\n=== NEGATIVES ===");
    let mut neg_verdicts: Vec<String> = Vec::new();
    for dir in &negative_dirs {
        let label = dir.file_name().unwrap().to_string_lossy().to_string();
        println!("\n--- negative set: {} ({:?}) ---", label, dir);
        for p in list_pngs(dir) {
            let fr = process_frame(&p, &label, &anchor, &refs, &out_dir);
            print_frame(&fr);
            let known_alive_distinct: usize = {
                let known: Vec<&str> = fr
                    .slots
                    .iter()
                    .filter(|s| !s.dead && s.status == "known")
                    .map(|s| s.hero.as_str())
                    .collect();
                let mut uniq = known.clone();
                uniq.sort();
                uniq.dedup();
                if uniq.len() == known.len() {
                    uniq.len()
                } else {
                    0 // duplicate known heroes -> not a clean 10-distinct board either way
                }
            };
            let verdict = if !fr.located {
                format!("{}/{}: OK — anchor NOT located (score={:.3} margin={:.3}, below gate {:.2}/{:.2})",
                    label, fr.stem, fr.score, fr.margin, SCORE_MIN, MARGIN_MIN)
            } else if known_alive_distinct >= 10 {
                format!("{}/{}: FAIL — anchor located AND read {} known-distinct alive heroes (looks like a real board misread as negative, or a real topbar legitimately visible)",
                    label, fr.stem, known_alive_distinct)
            } else {
                format!("{}/{}: OK — anchor located (score={:.3} margin={:.3}) but read only {} known-distinct alive heroes (not a full confident board)",
                    label, fr.stem, fr.score, fr.margin, known_alive_distinct)
            };
            println!("  VERDICT: {}", verdict);
            neg_verdicts.push(verdict);
        }
    }
    println!("\n=== NEGATIVE VERDICTS (all) ===");
    for v in &neg_verdicts {
        println!("  {}", v);
    }
    let neg_fails = neg_verdicts.iter().filter(|v| v.contains("FAIL")).count();
    println!(
        "(e) negatives: {}/{} OK, {} FAIL [{}]",
        neg_verdicts.len() - neg_fails,
        neg_verdicts.len(),
        neg_fails,
        if neg_fails == 0 { "PASS" } else { "FAIL — see per-negative verdicts above" }
    );
}

//! Prototype: "anchor + geometry" localization of the Dota 2 top HUD bar (5 ally
//! portraits | dark clock panel (score | day-night icon + timer | score) | 5 enemy
//! portraits) at ANY resolution, with no per-resolution hand tuning.
//!
//! Idea: the clock panel is a fixed-shape plaque that never moves relative to the
//! portraits, but its INTERIOR is dynamic (score digits, timer text, day/night icon
//! change every frame). So we template-match only the panel's STATIC pixels (border/
//! background shape, found by pixel-variance across several real frames) with a
//! multi-scale ZNCC search restricted to a small prior region (top-left-ish strip,
//! centered in X). Once the panel is located at (cx, cy, scale), all 10 portrait
//! rects are computed directly from proportions measured once on a reference frame
//! (no further search) — this is the geometry half of "anchor + geometry".
//!
//! Two anchor-matching strategies are implemented per the calibration brief:
//!   (a) masked color ZNCC — ncc.rs-style per-channel zero-mean NCC, restricted to
//!       pixels flagged STATIC by the variance mask (excludes digits/icon/timer).
//!   (b) gradient ZNCC — Sobel-magnitude ZNCC (structural edges), unmasked.
//! Both are run once on the native (as-shot) frames to see which gives the better
//! top1-top2 margin; the winner is then used for every test frame (native + rescaled).
//!
//! usage: topbar_locate <frames_dir> <out_dir> [extra_target_dir ...]
//!   frames_dir      9 native 1920x1080 frames (v1_*.png / v2_*.png / v3_*.png).
//!                   Also doubles as the calibration set for the static-pixel mask.
//!                   Must contain "v1_t480.png" — the reference frame geometry below
//!                   was measured on it.
//!   out_dir         overlay PNGs + per-frame results are written here.
//!   extra_target_dir  additional directories of *.png to run detection on (e.g.
//!                   rescaled 1280x720 / 2560x1440 variants), overlays go to out_dir.

use image::imageops::FilterType;
use image::{DynamicImage, GenericImageView, Rgb, RgbImage};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------------
// Geometry measured directly (pixel-scan, not eyeballed) on v1_t480.png, 1920x1080,
// and cross-checked to land on identical pixels in v2_t420.png / v3_t420.png (the
// HUD position is deterministic — it does not depend on match/gameplay content).
// ---------------------------------------------------------------------------------

/// Clock panel (the anchor): the dark plaque holding score | day-night icon+timer | score.
const ANCHOR_X: u32 = 856;
const ANCHOR_Y: u32 = 0;
const ANCHOR_W: u32 = 209;
const ANCHOR_H: u32 = 40;

const PANEL_CX_REF: f64 = ANCHOR_X as f64 + ANCHOR_W as f64 / 2.0; // 960.5
const PANEL_CY_REF: f64 = ANCHOR_Y as f64 + ANCHOR_H as f64 / 2.0; // 20.0

// Portrait groups: left edge of the group's first cell, and the pitch (cell-to-cell
// spacing) measured across the whole 5-cell group so per-cell rounding cannot drift.
const ALLY_X0_REF: f64 = 545.0;
const ALLY_PITCH_REF: f64 = 311.0 / 5.0; // 62.2
const ENEMY_X0_REF: f64 = 1066.0;
const ENEMY_PITCH_REF: f64 = 310.0 / 5.0; // 62.0
const PORTRAIT_W_REF: f64 = 60.0; // slightly under the pitch so the drawn box doesn't straddle the gap
const PORTRAIT_H_REF: f64 = 40.0;
const PORTRAIT_Y_REF: f64 = 0.0; // portraits are top-aligned with the panel

// ---------------------------------------------------------------------------------
// Search space: the bar is glued to the top edge and centered in X, so the prior is
// tight — central 40% of the frame width. SEARCH_Y_FRAC is slack on the window's TOP
// y-coordinate (not a height band): the panel measured at y=0 on all three real
// matches (native res), so only a couple % of slack is kept for sub-pixel rounding.
// NOTE: an early version used a full top-8%-of-height band and that was wide enough
// to also catch a second, structurally similar dark strip (a per-hero net-worth/item
// row) that appears a further ~35-60px down in some frames — it won several scale/
// position slots and produced 3/27 mislocalizations. Tying the prior to the true
// invariant (glued to y=0) instead of a generous height band removes that confound.
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
const SUBSAMPLE_COARSE: u32 = 2; // evaluate every 2nd pixel row/col during coarse search
const SUBSAMPLE_FINE: u32 = 1;

/// Static-pixel mask threshold: a panel pixel is "static" (used for matching) if its
/// luminance std-dev across the calibration frames is below this (0..255 scale).
const MASK_STD_THRESHOLD: f32 = 10.0;

#[derive(Clone, Copy)]
enum Approach {
    Color,
    Gradient,
}

#[derive(Clone)]
struct Candidate {
    x: u32,
    y: u32,
    scale: f64,
    score: f32,
}

#[derive(Clone, Copy)]
struct Rect {
    x: i64,
    y: i64,
    w: i64,
    h: i64,
}

fn crop_anchor(img: &DynamicImage) -> RgbImage {
    img.crop_imm(ANCHOR_X, ANCHOR_Y, ANCHOR_W, ANCHOR_H).to_rgb8()
}

/// Per-pixel luminance std-dev across the calibration crops -> bool mask (true = static).
fn build_static_mask(samples: &[RgbImage]) -> Vec<bool> {
    let w = ANCHOR_W as usize;
    let h = ANCHOR_H as usize;
    let n = samples.len() as f32;
    let mut mask = vec![true; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut vals = Vec::with_capacity(samples.len());
            for s in samples {
                let p = s.get_pixel(x as u32, y as u32);
                let l = 0.299 * p.0[0] as f32 + 0.587 * p.0[1] as f32 + 0.114 * p.0[2] as f32;
                vals.push(l);
            }
            let mean = vals.iter().sum::<f32>() / n;
            let var = vals.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / n;
            mask[y * w + x] = var.sqrt() < MASK_STD_THRESHOLD;
        }
    }
    mask
}

/// Nearest-neighbor resize of a bool mask (used to follow the template through scales).
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

/// Per-channel zero-mean NCC (ncc.rs style), restricted to mask==true pixels, sampled
/// every `step` pixels for speed. Range ~ -1..1, higher = better.
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

/// 3x3 Sobel gradient magnitude of the grayscale image (border pixels left at 0).
fn sobel_magnitude(img: &RgbImage) -> Vec<f32> {
    let (w, h) = img.dimensions();
    let gray: Vec<f32> = img
        .pixels()
        .map(|p| 0.299 * p.0[0] as f32 + 0.587 * p.0[1] as f32 + 0.114 * p.0[2] as f32)
        .collect();
    let mut mag = vec![0f32; (w * h) as usize];
    if w < 3 || h < 3 {
        return mag;
    }
    let at = |xx: u32, yy: u32| gray[(yy * w + xx) as usize];
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let gx = -at(x - 1, y - 1) - 2.0 * at(x - 1, y) - at(x - 1, y + 1) + at(x + 1, y - 1)
                + 2.0 * at(x + 1, y)
                + at(x + 1, y + 1);
            let gy = -at(x - 1, y - 1) - 2.0 * at(x, y - 1) - at(x + 1, y - 1) + at(x - 1, y + 1)
                + 2.0 * at(x, y + 1)
                + at(x + 1, y + 1);
            mag[(y * w + x) as usize] = (gx * gx + gy * gy).sqrt();
        }
    }
    mag
}

fn ncc_1d(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len() as f32;
    if a.is_empty() || a.len() != b.len() {
        return -1.0;
    }
    let ma = a.iter().sum::<f32>() / n;
    let mb = b.iter().sum::<f32>() / n;
    let (mut num, mut da, mut db) = (0f32, 0f32, 0f32);
    for i in 0..a.len() {
        let x = a[i] - ma;
        let y = b[i] - mb;
        num += x * y;
        da += x * x;
        db += y * y;
    }
    let denom = (da * db).sqrt();
    if denom > 0.0 {
        num / denom
    } else {
        -1.0
    }
}

fn grad_ncc(qmag: &[f32], tmag: &[f32], w: u32, h: u32, step: u32) -> f32 {
    if w < 3 || h < 3 {
        return -1.0;
    }
    let mut a = Vec::new();
    let mut b = Vec::new();
    let mut y = 1;
    while y < h - 1 {
        let mut x = 1;
        while x < w - 1 {
            let idx = (y * w + x) as usize;
            a.push(qmag[idx]);
            b.push(tmag[idx]);
            x += step;
        }
        y += step;
    }
    ncc_1d(&a, &b)
}

/// One scale's precomputed template data, shared by the coarse and fine passes.
struct ScaleTmpl {
    scale: f64,
    w: u32,
    h: u32,
    color: RgbImage,
    mask: Vec<bool>,
    grad: Vec<f32>,
}

fn build_scale_tmpl(tmpl_color: &RgbImage, mask: &[bool], scale: f64) -> Option<ScaleTmpl> {
    let w = ((ANCHOR_W as f64) * scale).round().max(8.0) as u32;
    let h = ((ANCHOR_H as f64) * scale).round().max(4.0) as u32;
    let dyn_img = DynamicImage::ImageRgb8(tmpl_color.clone());
    let color = dyn_img.resize_exact(w, h, FilterType::Lanczos3).to_rgb8();
    let mask_s = resize_mask(mask, ANCHOR_W, ANCHOR_H, w, h);
    let grad = sobel_magnitude(&color);
    Some(ScaleTmpl { scale, w, h, color, mask: mask_s, grad })
}

fn eval_window(frame: &DynamicImage, x: u32, y: u32, t: &ScaleTmpl, approach: Approach, step: u32) -> f32 {
    let window = frame.crop_imm(x, y, t.w, t.h).to_rgb8();
    match approach {
        Approach::Color => masked_color_ncc(&window, &t.color, &t.mask, step),
        Approach::Gradient => {
            let wmag = sobel_magnitude(&window);
            grad_ncc(&wmag, &t.grad, t.w, t.h, step)
        }
    }
}

/// Coarse multi-scale sliding search over the prior region. Returns every evaluated
/// candidate (used afterwards for non-max-suppressed top1/top2 margin).
fn search_coarse(
    frame: &DynamicImage,
    tmpl_color: &RgbImage,
    mask: &[bool],
    approach: Approach,
) -> Vec<Candidate> {
    let (fw, fh) = frame.dimensions();
    let sx0 = (fw as f64 * SEARCH_X0_FRAC) as u32;
    let sx1 = (fw as f64 * SEARCH_X1_FRAC) as u32;
    // y is slack on the window's TOP coordinate (the bar is glued to y=0), not a
    // height band — see the comment on SEARCH_Y_FRAC above.
    let sy0 = 0u32;
    let y_top_max = (fh as f64 * SEARCH_Y_FRAC) as u32;

    let mut out = Vec::new();
    let mut s = SCALE_MIN;
    while s <= SCALE_MAX + 1e-9 {
        if let Some(t) = build_scale_tmpl(tmpl_color, mask, s) {
            if t.w < sx1.saturating_sub(sx0) && t.h < fh {
                let mut y = sy0;
                while y <= y_top_max && y + t.h <= fh {
                    let mut x = sx0;
                    while x + t.w <= sx1 {
                        let score = eval_window(frame, x, y, &t, approach, SUBSAMPLE_COARSE);
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

/// Non-max-suppressed (top1, margin = top1 - top2) from the coarse candidate list.
fn top1_margin(cands: &[Candidate]) -> (Candidate, f32) {
    let mut v: Vec<&Candidate> = cands.iter().collect();
    v.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    let top = v[0].clone();
    let nms_w = ANCHOR_W as f64 * top.scale * 0.6;
    let nms_h = ANCHOR_H as f64 * top.scale * 0.6;
    let mut margin = top.score - (-1.0); // fallback if nothing distinct found
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

/// Fine refine: small scale + position window around the coarse best, step 1px, full
/// (unsubsampled) correlation, for an accurate final (cx, cy, scale).
fn refine(frame: &DynamicImage, tmpl_color: &RgbImage, mask: &[bool], approach: Approach, coarse: &Candidate) -> Candidate {
    let (fw, fh) = frame.dimensions();
    let mut best = coarse.clone();
    let mut s = (coarse.scale - SCALE_STEP_COARSE).max(SCALE_MIN);
    let s_max = (coarse.scale + SCALE_STEP_COARSE).min(SCALE_MAX);
    while s <= s_max + 1e-9 {
        if let Some(t) = build_scale_tmpl(tmpl_color, mask, s) {
            let x0 = coarse.x.saturating_sub(STRIDE_COARSE);
            let x1 = (coarse.x + STRIDE_COARSE).min(fw.saturating_sub(t.w));
            let y0 = coarse.y.saturating_sub(STRIDE_COARSE);
            let y1 = (coarse.y + STRIDE_COARSE).min(fh.saturating_sub(t.h));
            let mut y = y0;
            while y <= y1 {
                let mut x = x0;
                while x <= x1 {
                    let score = eval_window(frame, x, y, &t, approach, SUBSAMPLE_FINE);
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

fn locate(frame: &DynamicImage, tmpl_color: &RgbImage, mask: &[bool], approach: Approach) -> (Candidate, f32) {
    let coarse = search_coarse(frame, tmpl_color, mask, approach);
    let (top, margin) = top1_margin(&coarse);
    let fine = refine(frame, tmpl_color, mask, approach, &top);
    (fine, margin)
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

fn put(img: &mut RgbImage, x: i64, y: i64, color: Rgb<u8>) {
    if x >= 0 && y >= 0 && (x as u32) < img.width() && (y as u32) < img.height() {
        img.put_pixel(x as u32, y as u32, color);
    }
}

fn draw_rect(img: &mut RgbImage, r: Rect, color: Rgb<u8>, thickness: i64) {
    for t in 0..thickness {
        for xx in r.x..r.x + r.w {
            put(img, xx, r.y + t, color);
            put(img, xx, r.y + r.h - 1 - t, color);
        }
        for yy in r.y..r.y + r.h {
            put(img, r.x + t, yy, color);
            put(img, r.x + r.w - 1 - t, yy, color);
        }
    }
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

fn run_one(
    path: &Path,
    tmpl_color: &RgbImage,
    mask: &[bool],
    approach: Approach,
    out_dir: &Path,
) {
    let img = match image::open(path) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("skip {:?}: {e}", path);
            return;
        }
    };
    let (best, margin) = locate(&img, tmpl_color, mask, approach);
    let cx = best.x as f64 + (ANCHOR_W as f64 * best.scale) / 2.0;
    let cy = best.y as f64 + (ANCHOR_H as f64 * best.scale) / 2.0;

    let stem = path.file_stem().unwrap().to_string_lossy();
    println!(
        "{:<24} cx={:>7.1} cy={:>6.1} scale={:.3} score={:.3} margin={:.3}",
        stem, cx, cy, best.scale, best.score, margin
    );

    let mut overlay = img.to_rgb8();
    let anchor_rect = Rect {
        x: best.x as i64,
        y: best.y as i64,
        w: (ANCHOR_W as f64 * best.scale).round() as i64,
        h: (ANCHOR_H as f64 * best.scale).round() as i64,
    };
    draw_rect(&mut overlay, anchor_rect, Rgb([255, 255, 0]), 2);
    for r in portrait_rects(cx, cy, best.scale) {
        draw_rect(&mut overlay, r, Rgb([0, 255, 0]), 2);
    }
    let out_name = format!("{}_score{:.3}.png", stem, best.score);
    let out_path = out_dir.join(out_name);
    if let Err(e) = overlay.save(&out_path) {
        eprintln!("save {:?}: {e}", out_path);
    }
}

fn avg_margin(files: &[PathBuf], tmpl_color: &RgbImage, mask: &[bool], approach: Approach) -> f32 {
    let mut sum = 0f32;
    let mut n = 0f32;
    for f in files {
        if let Ok(img) = image::open(f) {
            let (_, margin) = locate(&img, tmpl_color, mask, approach);
            sum += margin;
            n += 1.0;
        }
    }
    if n == 0.0 {
        0.0
    } else {
        sum / n
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: topbar_locate <frames_dir> <out_dir> [extra_target_dir ...]");
        std::process::exit(2);
    }
    let frames_dir = PathBuf::from(&args[1]);
    let out_dir = PathBuf::from(&args[2]);
    std::fs::create_dir_all(&out_dir).expect("mkdir out_dir");

    let native_files = list_pngs(&frames_dir);
    eprintln!("calibration/native frames: {}", native_files.len());

    let ref_path = frames_dir.join("v1_t480.png");
    let ref_img = image::open(&ref_path).unwrap_or_else(|e| panic!("open ref {:?}: {e}", ref_path));
    let tmpl_color = crop_anchor(&ref_img);

    // Build the static-pixel mask from variance across all native calibration frames.
    let mut anchor_crops = Vec::new();
    for f in &native_files {
        if let Ok(img) = image::open(f) {
            if img.width() == 1920 && img.height() == 1080 {
                anchor_crops.push(crop_anchor(&img));
            }
        }
    }
    let mask = build_static_mask(&anchor_crops);
    let static_count = mask.iter().filter(|&&m| m).count();
    eprintln!(
        "static mask: {}/{} px ({:.0}%) from {} calibration crops",
        static_count,
        mask.len(),
        100.0 * static_count as f32 / mask.len() as f32,
        anchor_crops.len()
    );

    // --- (a) vs (b): compare masked-color ZNCC vs. gradient ZNCC on the native frames.
    let margin_color = avg_margin(&native_files, &tmpl_color, &mask, Approach::Color);
    let empty_mask = vec![true; mask.len()]; // gradient approach is unmasked
    let margin_grad = avg_margin(&native_files, &tmpl_color, &empty_mask, Approach::Gradient);
    eprintln!(
        "anchor strategy comparison (avg top1-top2 margin over {} native frames): masked-color={:.3}  gradient={:.3}",
        native_files.len(),
        margin_color,
        margin_grad
    );
    let (approach, approach_mask, approach_name) = if margin_color >= margin_grad {
        (Approach::Color, &mask, "masked-color ZNCC")
    } else {
        (Approach::Gradient, &empty_mask, "gradient ZNCC")
    };
    eprintln!("=> using {approach_name} for all runs\n");

    println!("file                     cx       cy    scale  score  margin");
    for f in &native_files {
        run_one(f, &tmpl_color, approach_mask, approach, &out_dir);
    }
    for extra in &args[3..] {
        let dir = PathBuf::from(extra);
        for f in list_pngs(&dir) {
            run_one(&f, &tmpl_color, approach_mask, approach, &out_dir);
        }
    }
}

//! Runtime anchor locator for the top-bar clock panel (masked multi-scale ZNCC).
//!
//! Ported from the validated prototype (`desktop/tools/refbuild/src/bin/topbar_locate.rs`,
//! Task: cross-resolution anchor+geometry design doc) — masked-color ZNCC beat gradient
//! ZNCC on the calibration set, so only that path is kept here; the gradient/Sobel code
//! is dead weight at runtime and was dropped.
//!
//! Idea: the clock panel is a fixed-shape plaque that never moves relative to the HUD,
//! but its INTERIOR is dynamic (score digits, timer text, day/night icon change every
//! frame). So we template-match only the panel's STATIC pixels (the baked `topbar_anchor()`
//! mask) with a multi-scale ZNCC search restricted to a small prior region (glued to the
//! top edge, centered in X) — this finds the panel at any resolution/UI scale with no
//! per-resolution hand tuning.
//!
//! `locate` does the full gated sweep (coarse multi-scale search -> top1/top2 margin gate
//! -> fine refine) and is meant for cold-start / re-acquire. `verify` is the cheap per-frame
//! re-check once a `Located` is already trusted: it only re-samples the known scale in a
//! tiny +/-4px window, so it is far cheaper than a full `locate` call.

use std::sync::OnceLock;

use image::imageops::FilterType;
use image::{DynamicImage, GenericImageView, RgbImage};

// ---------------------------------------------------------------------------------
// Search space: the bar is glued to the top edge and centered in X, so the prior is
// tight — central 40% of the frame width. SEARCH_Y_FRAC is slack on the window's TOP
// y-coordinate (not a height band): the panel measured at y=0 on all calibration
// frames (native res), so only a couple % of slack is kept for sub-pixel rounding.
// NOTE: an early prototype version used a full top-8%-of-height band and that was wide
// enough to also catch a second, structurally similar dark strip (a per-hero net-worth/
// item row) that appears a further ~35-60px down in some frames — it won several scale/
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

/// Gate for `locate`: reject a hit unless BOTH the peak score and the top1-top2 margin
/// clear these floors. Prototype's observed floor on real frames was score 0.693 /
/// margin 0.14; these constants sit a bit below that as a safety margin.
pub const SCORE_MIN: f32 = 0.55;
pub const MARGIN_MIN: f32 = 0.12;

const ANCHOR_BLOB: &[u8] = include_bytes!("topbar_anchor.bin");

/// The baked anchor: clock-panel template + static-pixel mask (Task 1 format).
pub struct Anchor {
    pub w: u32,
    pub h: u32,
    pub tmpl: RgbImage,
    pub mask: Vec<bool>,
}

/// A located anchor: top-left of the matched window in frame pixels, the scale it
/// matched at (vs. the baked template's native size), and the match quality.
pub struct Located {
    pub x: u32,
    pub y: u32,
    pub scale: f64,
    pub score: f32,
    pub margin: f32,
}

/// One scale's precomputed template data, shared by the coarse and fine passes.
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

/// Parse the baked anchor blob (Task 1 format, little-endian):
/// magic "VAELTANC" (8 bytes), version u8, w u16, h u16, then w*h*3 RGB template bytes
/// (row-major), then w*h mask bytes (1 = static pixel, 0 = dynamic). Any malformation
/// (bad magic, truncated buffer, overflowing header) yields `None` — a missing anchor
/// means the locator simply never finds anything, which is the safe failure.
pub fn parse_anchor(data: &[u8]) -> Option<Anchor> {
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

/// The bundled top-bar anchor, parsed once.
pub fn topbar_anchor() -> &'static Anchor {
    static ANCHOR: OnceLock<Anchor> = OnceLock::new();
    ANCHOR.get_or_init(|| parse_anchor(ANCHOR_BLOB).expect("baked topbar_anchor.bin is well-formed"))
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

/// Coarse multi-scale sliding search over the prior region. Returns every evaluated
/// candidate (used afterwards for non-max-suppressed top1/top2 margin).
fn search_coarse(frame: &DynamicImage, a: &Anchor) -> Vec<Candidate> {
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

/// Non-max-suppressed (top1, margin = top1 - top2) from the coarse candidate list.
fn top1_margin(cands: &[Candidate], a: &Anchor) -> (Candidate, f32) {
    let mut v: Vec<&Candidate> = cands.iter().collect();
    v.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    let top = v[0].clone();
    let nms_w = a.w as f64 * top.scale * 0.6;
    let nms_h = a.h as f64 * top.scale * 0.6;
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
/// (unsubsampled) correlation, for an accurate final (x, y, scale).
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

/// Full gated sweep: coarse multi-scale search -> top1/top2 margin -> fine refine ->
/// gate. The margin comes from the coarse sweep's non-max-suppression (a local refine
/// window has no competing candidates to suppress, so margin isn't recomputed there),
/// but the score gate is checked against the REFINED score: the coarse scale grid is
/// only 0.1 apart, so a true match that lands between two grid points can coarse-score
/// well under its real quality (e.g. a true scale of 0.667 sits roughly between the
/// 0.6 and 0.7 grid points, both of which underscore it) — refine is exactly what
/// recovers that, so gating before it would reject good matches for a search-grid
/// artifact instead of a real absence of the anchor.
pub fn locate(frame: &DynamicImage, a: &Anchor) -> Option<Located> {
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

/// Cheap per-frame re-check once `prev` is already trusted: re-evaluates only
/// `prev.scale` in a +/-4px window around `prev.x, prev.y` (stride 1, full sampling —
/// no subsampling since the window is tiny). Only `SCORE_MIN` is applied; the margin
/// gate needs a full sweep to mean anything, so `prev.margin` is carried through
/// unchanged. A verify miss just means the caller should fall back to a fresh `locate`.
pub fn verify(frame: &DynamicImage, a: &Anchor, prev: &Located) -> Option<Located> {
    let t = build_scale_tmpl(a, prev.scale)?;
    let (fw, fh) = frame.dimensions();
    let x0 = prev.x.saturating_sub(4);
    let x1 = (prev.x + 4).min(fw.saturating_sub(t.w));
    let y0 = prev.y.saturating_sub(4);
    let y1 = (prev.y + 4).min(fh.saturating_sub(t.h));

    let mut best: Option<Candidate> = None;
    let mut y = y0;
    while y <= y1 {
        let mut x = x0;
        while x <= x1 {
            let score = eval_window(frame, x, y, &t, SUBSAMPLE_FINE);
            if best.as_ref().map_or(true, |b| score > b.score) {
                best = Some(Candidate { x, y, scale: prev.scale, score });
            }
            x += STRIDE_FINE;
        }
        y += STRIDE_FINE;
    }

    let best = best?;
    if best.score < SCORE_MIN {
        return None;
    }
    Some(Located { x: best.x, y: best.y, scale: best.scale, score: best.score, margin: prev.margin })
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage};

    /// Build a synthetic frame with the baked anchor planted at `scale`, glued to y=0
    /// and slightly off-center in x (like the real HUD). Noise background so ZNCC has
    /// variance to reject. Returns the frame and the planted window's x.
    fn synth(fw: u32, fh: u32, scale: f64, a: &Anchor) -> (DynamicImage, u32) {
        let mut frame = RgbImage::from_fn(fw, fh, |x, y| {
            Rgb([(x * 7 % 251) as u8, (y * 13 % 251) as u8, ((x + y) % 251) as u8])
        });
        let (tw, th) = (((a.w as f64) * scale).round() as u32, ((a.h as f64) * scale).round() as u32);
        let t = DynamicImage::ImageRgb8(a.tmpl.clone()).resize_exact(tw, th, FilterType::Lanczos3).to_rgb8();
        let px = fw / 2 - tw / 2 + 3; // slight off-center like the real panel
        image::imageops::overlay(&mut frame, &t, px as i64, 0);
        (DynamicImage::ImageRgb8(frame), px)
    }

    #[test]
    fn baked_anchor_parses() {
        let a = topbar_anchor();
        assert_eq!((a.w, a.h), (209, 40));
        let static_frac = a.mask.iter().filter(|&&m| m).count() as f32 / a.mask.len() as f32;
        assert!(static_frac > 0.5 && static_frac < 0.99, "mask should exclude only the dynamic interior, got {static_frac}");
    }

    #[test]
    fn locates_anchor_planted_in_synthetic_frame_across_scales() {
        let a = topbar_anchor();
        for &(fw, fh, scale) in &[(1920u32, 1080u32, 1.0f64), (1280, 720, 0.667), (2560, 1440, 1.333)] {
            let (frame, px) = synth(fw, fh, scale, a);
            let loc = locate(&frame, a).unwrap_or_else(|| panic!("not found at {fw}x{fh}"));
            assert!((loc.scale - scale).abs() < 0.05, "scale {} vs {scale}", loc.scale);
            assert!((loc.x as i64 - px as i64).abs() <= 3 && loc.y <= 2);
            // verify() must re-confirm the same spot cheaply.
            let (frame2, _) = synth(fw, fh, scale, a);
            let v = verify(&frame2, a, &loc);
            assert!(v.is_some());
        }
    }

    #[test]
    fn rejects_frame_without_anchor() {
        let a = topbar_anchor();
        let frame = RgbImage::from_fn(1920, 1080, |x, y| Rgb([(x * 3 % 256) as u8, (y * 5 % 256) as u8, 128]));
        assert!(locate(&DynamicImage::ImageRgb8(frame), a).is_none());
    }
}

//! Color ZNCC matcher — the validated recognizer (Phase-0: 6/7 on clean scoreboard
//! portraits vs the 126 VPK refs, and never a false `known`). Ported from the
//! `vision-spikes` color-NCC spike.
//!
//! Why this and not pHash: zero-mean makes it brightness-invariant (a bright glowing
//! hero no longer wins everything), and comparing per RGB channel makes it COLOR-aware,
//! which separates the look-alikes grayscale pHash confused (Oracle/Lina, Chaos
//! Knight/Death Prophet). Confidence is gated on the top1−top2 MARGIN, not the peak,
//! so two heroes that both score high but close go to `unconfirmed` (safe) rather than
//! a guessed `known`.

use image::{DynamicImage, RgbImage};

/// Canonical match size. Both refs (baked into the bundle) and live query crops are
/// brought to exactly this size before ZNCC, so they go through identical processing.
/// Aspect ~1.78 mirrors the VPK 128x72 HUD portrait.
pub const W: u32 = 48;
pub const H: u32 = 27;

fn resized_rgb(img: &DynamicImage, w: u32, h: u32) -> RgbImage {
    img.resize_exact(w, h, image::imageops::FilterType::Lanczos3)
        .to_rgb8()
}

/// Bring an on-screen portrait crop to the canonical match size (RGB).
pub fn prep_query(img: &DynamicImage) -> RgbImage {
    resized_rgb(img, W, H)
}

/// Zero-mean normalized cross-correlation per RGB channel (OpenCV TM_CCOEFF_NORMED
/// style), averaged. Zero-mean → brightness-invariant; per-channel → color-aware.
/// Both images must be the same size. Range ~ -1..1, higher = better.
pub fn color_ncc(query: &RgbImage, tmpl: &RgbImage) -> f32 {
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

/// Rank refs by color ZNCC (descending). Returns (key, score) sorted best-first.
pub fn rank(query: &RgbImage, refs: &[(String, RgbImage)]) -> Vec<(String, f32)> {
    let mut v: Vec<(String, f32)> = refs
        .iter()
        .map(|(k, t)| (k.clone(), color_ncc(query, t)))
        .collect();
    v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    v
}

/// (best_key, top1 score, top1−top2 margin) from a descending ranking.
pub fn best(ranked: &[(String, f32)]) -> (String, f32, f32) {
    let (k0, s0) = ranked.first().cloned().unwrap_or_default();
    let s1 = ranked.get(1).map(|(_, s)| *s).unwrap_or(0.0);
    (k0, s0, s0 - s1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage};

    fn solid(w: u32, h: u32, px: [u8; 3]) -> RgbImage {
        RgbImage::from_pixel(w, h, Rgb(px))
    }

    #[test]
    fn identical_color_blocks_correlate_perfectly() {
        // Same image → ~1.0. Every channel must VARY (a constant channel has zero
        // variance and contributes 0 to the average, which is the intended behavior).
        let a = RgbImage::from_fn(W, H, |x, y| Rgb([(x * 5) as u8, (y * 9) as u8, 200 - x as u8]));
        let b = a.clone();
        assert!((color_ncc(&a, &b) - 1.0).abs() < 1e-4);
    }

    #[test]
    fn zero_mean_is_brightness_invariant() {
        // Same pattern, uniformly brighter → zero-mean NCC still ~1.0 (not fooled by brightness).
        let a = RgbImage::from_fn(W, H, |x, _| Rgb([(x * 3) as u8, (x * 2) as u8, x as u8]));
        let b = RgbImage::from_fn(W, H, |x, _| {
            Rgb([(x * 3 + 40).min(255) as u8, (x * 2 + 40).min(255) as u8, (x + 40) as u8])
        });
        assert!(color_ncc(&a, &b) > 0.95);
    }

    #[test]
    fn color_separates_when_palette_differs() {
        // A red template and a green template do not correlate well.
        let red = solid(W, H, [200, 20, 20]);
        let green = solid(W, H, [20, 200, 20]);
        // Solid images have zero variance → denom 0 → channel contributes 0; total 0.
        assert!(color_ncc(&red, &green) <= 0.0);
    }

    #[test]
    fn rank_and_best_pick_the_closest() {
        let q = RgbImage::from_fn(W, H, |x, y| Rgb([(x + y) as u8, x as u8, y as u8]));
        let near = q.clone();
        let far = RgbImage::from_fn(W, H, |x, y| Rgb([y as u8, (x * 2) as u8, (x + 1) as u8]));
        let refs = vec![("far".to_string(), far), ("near".to_string(), near)];
        let ranked = rank(&q, &refs);
        let (k, top1, margin) = best(&ranked);
        assert_eq!(k, "near");
        assert!(top1 > 0.99);
        assert!(margin > 0.0);
    }

    #[test]
    fn mismatched_sizes_score_minus_one() {
        let a = solid(W, H, [1, 2, 3]);
        let b = solid(W + 1, H, [1, 2, 3]);
        assert_eq!(color_ncc(&a, &b), -1.0);
    }
}

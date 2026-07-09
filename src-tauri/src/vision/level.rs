//! Read an enemy's LEVEL from the scoreboard "УР." circle.
//!
//! The level is a 1- or 2-digit number centred in a small circle, wrapped by a gold XP
//! ring. The ring would wreck a naive read, so we:
//!   1. mask to the central digit band (rectangular inset) and threshold the bright digits;
//!   2. split columns into digit runs by ink-HEIGHT — the ring's arcs are thin (a few px
//!      per column) while digit columns are tall, so the ring is rejected by shape;
//!   3. classify each glyph (grayscale zero-mean NCC) against bundled 0-9 prototypes;
//!   4. gate each digit on peak AND margin, and require a plausible 1..=30 result.
//!
//! Safety (the "never a wrong level" rule): the wrong-level guarantee rests on this static
//! per-digit gate — a digit ships only with a clear peak AND a clear margin over the
//! runner-up; anything ambiguous makes the WHOLE read return None (the caller omits the
//! level). The temporal/monotonic guards in the capture loop are defence against transient
//! flicker, NOT against a stable misread, so the gate is set with large measured headroom:
//! true digit reads sat at peak >= 0.847 / margin >= 0.335 on the calibration match, so we
//! gate at 0.75 / 0.20 (no true read lost, ambiguous reads rejected). The fixed-pixel
//! segmentation is calibrated for the ~44x36 level box at 1918x1078; `read_level` refuses a
//! box of any other size (off-calibration resolution → omit, never guess). Prototypes and
//! thresholds were validated offline against a real match across 5:13 → 31:47 (130 reads,
//! 0 wrong; held-out 40 reads, 0 wrong). See desktop/docs and desktop/tools/refbuild.

use std::sync::OnceLock;

use image::RgbaImage;

// Digit band inside the level box and segmentation thresholds (box is ~44x36 px on the
// calibrated 1918x1078 surface). The ring hugs the perimeter; these keep only the centre.
const INSET_L: u32 = 10;
const INSET_R: u32 = 30; // inclusive
const INSET_T: u32 = 6;
const INSET_B: u32 = 29; // inclusive
const INK_T: u8 = 110; // luminance threshold for "digit ink"
const MIN_COL: u32 = 3; // a column needs this many ink px to count (kills thin ring arcs)
const MIN_PEAK: u32 = 7; // a run needs one column at least this tall to be a digit

// Canonical glyph size and per-digit gates (grayscale ZNCC, range ~-1..1). Set with large
// headroom below the measured true-read floor (peak 0.847 / margin 0.335) so an ambiguous
// or biased read is rejected rather than guessed (see module docs).
const GW: u32 = 14;
const GH: u32 = 20;
const DIGIT_PEAK_MIN: f32 = 0.75;
const DIGIT_MARGIN_MIN: f32 = 0.20;

// The fixed-pixel insets/segmentation above are calibrated for the level box at the
// product owner's 1918x1078 HUD (box ~44x36). A capture at another resolution/HUD scale
// yields a different box size, so we only read levels for a box within this size band and
// omit otherwise (never guess on uncalibrated geometry).
const BOX_W_MIN: u32 = 40;
const BOX_W_MAX: u32 = 48;
const BOX_H_MIN: u32 = 32;
const BOX_H_MAX: u32 = 40;

const BLOB: &[u8] = include_bytes!("digits_refs.bin");

/// digit (0-9) -> prototype glyph (GW*GH grayscale values as f32)
fn templates() -> &'static [(u8, Vec<f32>)] {
    static T: OnceLock<Vec<(u8, Vec<f32>)>> = OnceLock::new();
    T.get_or_init(|| parse_blob(BLOB))
}

fn parse_blob(data: &[u8]) -> Vec<(u8, Vec<f32>)> {
    let mut out = Vec::new();
    if data.len() < 15 || &data[0..8] != b"VAELDREF" {
        return out;
    }
    let gw = u16::from_le_bytes([data[9], data[10]]) as usize;
    let gh = u16::from_le_bytes([data[11], data[12]]) as usize;
    let count = u16::from_le_bytes([data[13], data[14]]) as usize;
    let px = match gw.checked_mul(gh) {
        Some(p) => p,
        None => return out,
    };
    let mut p = 15usize;
    for _ in 0..count {
        if p >= data.len() {
            break;
        }
        let d = data[p];
        p += 1;
        if p + px > data.len() {
            break;
        }
        let proto: Vec<f32> = data[p..p + px].iter().map(|&b| b as f32).collect();
        p += px;
        out.push((d, proto));
    }
    out
}

fn lum(p: &image::Rgba<u8>) -> u8 {
    ((p.0[0] as u32 * 77 + p.0[1] as u32 * 150 + p.0[2] as u32 * 29) >> 8) as u8
}

/// Binary ink map (true = digit pixel) over the central band of a level box.
fn ink_map(box_img: &RgbaImage) -> Vec<Vec<bool>> {
    let (w, h) = box_img.dimensions();
    let mut m = vec![vec![false; w as usize]; h as usize];
    if w == 0 || h == 0 {
        return m;
    }
    for y in INSET_T..=INSET_B.min(h - 1) {
        for x in INSET_L..=INSET_R.min(w - 1) {
            if lum(box_img.get_pixel(x, y)) >= INK_T {
                m[y as usize][x as usize] = true;
            }
        }
    }
    m
}

fn col_ink(m: &[Vec<bool>], x: usize) -> u32 {
    m.iter().filter(|row| row[x]).count() as u32
}

/// Column-runs that look like digits (thin ring arcs are dropped by the height gates).
fn digit_runs(m: &[Vec<bool>]) -> Vec<(usize, usize)> {
    let h = m.len();
    let w = if h > 0 { m[0].len() } else { 0 };
    let active = |x: usize| col_ink(m, x) >= MIN_COL;
    let mut runs = Vec::new();
    let mut x = 0;
    while x < w {
        if active(x) {
            let start = x;
            while x < w && active(x) {
                x += 1;
            }
            let end = x - 1;
            let peak = (start..=end).map(|c| col_ink(m, c)).max().unwrap_or(0);
            if peak >= MIN_PEAK {
                runs.push((start, end));
            }
        } else {
            x += 1;
        }
    }
    runs
}

/// Tight-cropped, canonical-size binary glyph for one column-run.
fn glyph_from_run(m: &[Vec<bool>], run: (usize, usize)) -> Option<Vec<f32>> {
    let h = m.len();
    let (x0, x1) = run;
    let (mut miny, mut maxy, mut minx, mut maxx) = (usize::MAX, 0usize, usize::MAX, 0usize);
    for y in 0..h {
        for x in x0..=x1 {
            if m[y][x] {
                miny = miny.min(y);
                maxy = maxy.max(y);
                minx = minx.min(x);
                maxx = maxx.max(x);
            }
        }
    }
    if miny == usize::MAX {
        return None;
    }
    let (sw, sh) = ((maxx - minx + 1) as u32, (maxy - miny + 1) as u32);
    let mut g = image::GrayImage::new(sw, sh);
    for y in miny..=maxy {
        for x in minx..=maxx {
            let v = if m[y][x] { 255u8 } else { 0u8 };
            g.put_pixel(x as u32 - minx as u32, y as u32 - miny as u32, image::Luma([v]));
        }
    }
    let r = image::imageops::resize(&g, GW, GH, image::imageops::FilterType::Triangle);
    Some(r.pixels().map(|p| p.0[0] as f32).collect())
}

fn zncc(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len() as f32;
    if n == 0.0 || a.len() != b.len() {
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
    let d = (da * db).sqrt();
    if d > 0.0 {
        num / d
    } else {
        -1.0
    }
}

/// (digit, top1, margin) of a glyph against the 0-9 prototypes.
fn classify(glyph: &[f32]) -> Option<(u8, f32, f32)> {
    let t = templates();
    if t.is_empty() {
        return None;
    }
    let mut s: Vec<(f32, u8)> = t.iter().map(|(d, proto)| (zncc(glyph, proto), *d)).collect();
    s.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let s1 = s.get(1).map(|x| x.0).unwrap_or(-1.0);
    Some((s[0].1, s[0].0, s[0].0 - s1))
}

/// Read a level box. Returns Some(level) for a confident 1..=30 read, else None (unsure).
pub fn read_level(box_img: &RgbaImage) -> Option<u32> {
    let (w, h) = box_img.dimensions();
    if !(BOX_W_MIN..=BOX_W_MAX).contains(&w) || !(BOX_H_MIN..=BOX_H_MAX).contains(&h) {
        return None; // off-calibration box size → don't trust the fixed-pixel segmentation
    }
    let m = ink_map(box_img);
    let runs = digit_runs(&m);
    if runs.is_empty() || runs.len() > 2 {
        return None;
    }
    let mut level = 0u32;
    for run in runs {
        let g = glyph_from_run(&m, run)?;
        let (d, top1, margin) = classify(&g)?;
        if top1 < DIGIT_PEAK_MIN || margin < DIGIT_MARGIN_MIN {
            return None;
        }
        level = level * 10 + d as u32;
    }
    if (1..=30).contains(&level) {
        Some(level)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_blob_has_all_ten_digits() {
        let t = templates();
        assert_eq!(t.len(), 10, "expected 10 digit prototypes");
        let mut seen = [false; 10];
        for (d, proto) in t {
            assert!(*d < 10);
            seen[*d as usize] = true;
            assert_eq!(proto.len() as u32, GW * GH);
        }
        assert!(seen.iter().all(|&s| s), "digits 0-9 all present");
    }

    /// Paint a digit string into a synthetic level box using the bundled prototypes, then
    /// confirm the reader recovers it. Proves segmentation + classification round-trips and
    /// that the digit-count/plausibility gates behave.
    fn synth_box(level: u32) -> RgbaImage {
        let t = templates();
        let proto = |d: u8| &t.iter().find(|(k, _)| *k == d).unwrap().1;
        let mut img = RgbaImage::from_pixel(44, 36, image::Rgba([12, 14, 19, 255]));
        let chars: Vec<u8> = level.to_string().bytes().map(|b| b - b'0').collect();
        // Paint each glyph at a realistic native width (~6x16) so two digits sit in the
        // digit band with a clear gap (matching the real scoreboard layout x12-28).
        let (dw, dh, oy) = (6u32, 16u32, 10u32);
        let xs: &[u32] = if chars.len() == 1 { &[19] } else { &[12, 21] };
        for (i, c) in chars.iter().enumerate() {
            // proto (GW x GH) -> grayscale -> down to native digit size
            let mut g = image::GrayImage::new(GW, GH);
            let p = proto(*c);
            for gy in 0..GH {
                for gx in 0..GW {
                    g.put_pixel(gx, gy, image::Luma([p[(gy * GW + gx) as usize] as u8]));
                }
            }
            let small = image::imageops::resize(&g, dw, dh, image::imageops::FilterType::Triangle);
            let ox = xs[i];
            for sy in 0..dh {
                for sx in 0..dw {
                    if small.get_pixel(sx, sy).0[0] > 110 {
                        let (px, py) = (ox + sx, oy + sy);
                        if px < 44 && py < 36 {
                            img.put_pixel(px, py, image::Rgba([220, 200, 140, 255]));
                        }
                    }
                }
            }
        }
        img
    }

    #[test]
    fn reads_one_and_two_digit_levels() {
        for lvl in [5u32, 7, 9, 10, 13, 20, 24, 28, 30] {
            assert_eq!(read_level(&synth_box(lvl)), Some(lvl), "level {lvl}");
        }
    }

    #[test]
    fn blank_box_is_unsure() {
        let blank = RgbaImage::from_pixel(44, 36, image::Rgba([12, 14, 19, 255]));
        assert_eq!(read_level(&blank), None);
    }

    #[test]
    fn off_calibration_box_size_is_omitted() {
        // A box that isn't ~44x36 (a different resolution / HUD scale) must never be read,
        // since the fixed-pixel segmentation is calibrated for that size — omit, don't guess.
        let big = RgbaImage::from_pixel(88, 72, image::Rgba([220, 200, 140, 255]));
        assert_eq!(read_level(&big), None);
        let small = RgbaImage::from_pixel(22, 18, image::Rgba([220, 200, 140, 255]));
        assert_eq!(read_level(&small), None);
    }
}

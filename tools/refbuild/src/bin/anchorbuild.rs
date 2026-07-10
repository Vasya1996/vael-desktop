//! Bake the top-bar anchor asset: clock-panel template (from the reference frame)
//! + static-pixel mask (per-pixel luminance variance across all calibration frames,
//! same method the validated topbar_locate prototype used).
//!
//! usage: anchorbuild <frames_dir> <out.bin>
//!   frames_dir  1920x1080 calibration frames; must contain v1_t480.png (reference).
//!
//! Blob format (little-endian):
//!   magic   "VAELTANC" (8 bytes)
//!   version u8  = 1
//!   w       u16
//!   h       u16
//!   template  w*h*3 RGB bytes (row-major)
//!   mask      w*h bytes (1 = static pixel, 0 = dynamic)

use image::{DynamicImage, RgbImage};
use std::path::{Path, PathBuf};

/// Clock panel (the anchor): the dark plaque holding score | day-night icon+timer | score.
/// Measured on v1_t480.png, 1920x1080 — see topbar_locate.rs for the full derivation.
const ANCHOR_X: u32 = 856;
const ANCHOR_Y: u32 = 0;
const ANCHOR_W: u32 = 209;
const ANCHOR_H: u32 = 40;

/// Static-pixel mask threshold: a panel pixel is "static" (used for matching) if its
/// luminance std-dev across the calibration frames is below this (0..255 scale).
const MASK_STD_THRESHOLD: f32 = 10.0;

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

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: anchorbuild <frames_dir> <out.bin>");
        std::process::exit(2);
    }
    let frames_dir = PathBuf::from(&args[1]);

    let ref_path = frames_dir.join("v1_t480.png");
    let ref_img = image::open(&ref_path).unwrap_or_else(|e| panic!("open ref {:?}: {e}", ref_path));
    let tmpl = crop_anchor(&ref_img);

    let mut crops = Vec::new();
    for f in list_pngs(&frames_dir) {
        if let Ok(img) = image::open(&f) {
            if img.width() == 1920 && img.height() == 1080 {
                crops.push(crop_anchor(&img));
            }
        }
    }
    let mask = build_static_mask(&crops);

    let mut buf: Vec<u8> = Vec::new();
    buf.extend_from_slice(b"VAELTANC");
    buf.push(1u8);
    buf.extend_from_slice(&(ANCHOR_W as u16).to_le_bytes());
    buf.extend_from_slice(&(ANCHOR_H as u16).to_le_bytes());
    buf.extend_from_slice(tmpl.as_raw());
    buf.extend(mask.iter().map(|&m| m as u8));
    std::fs::write(&args[2], &buf).expect("write out.bin");
    eprintln!("wrote {} bytes ({}x{}, {} static px)", buf.len(), ANCHOR_W, ANCHOR_H,
        mask.iter().filter(|&&m| m).count());
}

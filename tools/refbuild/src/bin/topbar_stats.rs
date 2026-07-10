//! Data-collection tool for topbar threshold (re)calibration.
//! Sections A-D: blob inventory vs art dir, per-entry saturation, self-match
//! confusion matrix, full-pipeline run over native tb_* frames.
//!
//! usage: topbar_stats <anchor.bin> <topbar_refs.bin> <topbar_art_dir> <heroes_list.txt> <frames_dir>

use image::imageops::FilterType;
use image::{DynamicImage, GenericImageView, RgbImage};
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

// ---------------------------------------------------------------------------------
// Copied from vision/locate.rs (via topbar_scan.rs) — anchor locator.
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
    let y_top_max = (fh as f64 * SEARCH_Y_FRAC) as u32;
    let mut out = Vec::new();
    let mut s = SCALE_MIN;
    while s <= SCALE_MAX + 1e-9 {
        if let Some(t) = build_scale_tmpl(a, s) {
            if t.w < sx1.saturating_sub(sx0) && t.h < fh {
                let mut y = 0u32;
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
// Copied from vision/topbar.rs — runtime portrait geometry + sat gate.
// ---------------------------------------------------------------------------------
const ALLY_DX: f64 = -311.0;
const ALLY_PITCH: f64 = 62.2;
const ENEMY_DX: f64 = 210.0;
const ENEMY_PITCH: f64 = 62.0;
const CELL_W: f64 = 60.0;
const CELL_H: f64 = 40.0;
const CELL_ART_Y0_FRAC: f64 = 0.15;
const MIN_LIVE_SAT: f32 = 20.5;
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
// Copied from vision/ncc.rs / topbar_refs.rs — color ZNCC + variant-collapse rank.
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
    let px = (w as usize) * (h as usize) * 3;
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
        let key = std::str::from_utf8(&data[p..p + kl]).unwrap().to_string();
        p += kl;
        let raw = data[p..p + px].to_vec();
        p += px;
        if let Some(img) = RgbImage::from_raw(w, h, raw) {
            out.push((key, img));
        }
    }
    out
}

fn short(key: &str) -> &str {
    key.trim_start_matches("npc_dota_hero_")
}

/// The exact variant-suffix stripping rule from topbar_refs.rs::strip_variant_suffix:
/// `_alt[N]` / `_persona<N>` / `_carnival` all collapse to the base hero key.
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

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 6 {
        eprintln!("usage: topbar_stats <anchor.bin> <topbar_refs.bin> <topbar_art_dir> <heroes_list.txt> <frames_dir>");
        std::process::exit(2);
    }
    let anchor = parse_anchor(&std::fs::read(&args[1]).unwrap()).expect("anchor");
    let refs = parse_refs_blob(&std::fs::read(&args[2]).unwrap());
    let art_dir = PathBuf::from(&args[3]);
    let heroes_list = std::fs::read_to_string(&args[4]).unwrap();
    let frames_dir = PathBuf::from(&args[5]);

    // =========================== A) INVENTORY ===========================
    println!("=== A) BLOB INVENTORY ===");
    println!("blob entries: {}", refs.len());
    let mut per_hero: BTreeMap<&str, usize> = BTreeMap::new();
    for (k, _) in &refs {
        *per_hero.entry(k.as_str()).or_default() += 1;
    }
    println!("distinct heroes in blob: {}", per_hero.len());
    println!("heroes with >1 variant in blob:");
    for (k, n) in &per_hero {
        if *n > 1 {
            println!("  {:<28} {} entries", short(k), n);
        }
    }

    // topbar_art dir: every *_png.png file, classified.
    let blob_keys: HashSet<&str> = per_hero.keys().copied().collect();
    let mut art_files: Vec<String> = std::fs::read_dir(&art_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().to_str().map(String::from))
        .collect();
    art_files.sort();
    let mut hero_art_total = 0usize;
    let mut dropped: Vec<String> = Vec::new();
    for f in &art_files {
        let Some(stem) = f.strip_suffix("_png.png") else {
            if f.starts_with("npc_dota_hero_") {
                dropped.push(format!("{f}  (suffix not _png.png)"));
            }
            continue;
        };
        if !stem.starts_with("npc_dota_hero_") {
            continue; // creeps/summons, intentionally not heroes
        }
        hero_art_total += 1;
        let base = strip_variant_suffix(stem);
        if !blob_keys.contains(base) {
            dropped.push(format!("{f}  (base key '{base}' not in blob)"));
        }
    }
    println!("\ntopbar_art dir: {} files total, {} npc_dota_hero_* *_png.png files", art_files.len(), hero_art_total);
    println!("hero art files NOT represented in the blob ({}):", dropped.len());
    for d in &dropped {
        println!("  {d}");
    }

    // heroes_list.txt cross-check: hero art present in the VPK listing but absent in the art dir.
    let mut vpk_stems: HashSet<String> = HashSet::new();
    for line in heroes_list.lines() {
        if let Some(tok) = line.split_whitespace().next() {
            if let Some(name) = tok.rsplit('/').next() {
                if let Some(stem) = name.strip_suffix("_png.vtex_c") {
                    vpk_stems.insert(stem.to_string());
                }
            }
        }
    }
    let art_stems: HashSet<String> = art_files
        .iter()
        .filter_map(|f| f.strip_suffix("_png.png").map(String::from))
        .collect();
    let mut vpk_missing_in_art: Vec<&String> = vpk_stems
        .iter()
        .filter(|s| s.starts_with("npc_dota_hero_") && !art_stems.contains(*s))
        .collect();
    vpk_missing_in_art.sort();
    println!("\nVPK-listed hero textures MISSING from topbar_art dir ({}):", vpk_missing_in_art.len());
    for s in &vpk_missing_in_art {
        println!("  {s}");
    }

    // =========================== B) SATURATION ===========================
    println!("\n=== B) BLOB ENTRY SATURATION (art_band_saturation on baked 48x27) ===");
    let mut sats: Vec<(usize, f32)> = refs.iter().enumerate().map(|(i, (_, img))| (i, art_band_saturation(img))).collect();
    sats.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
    let below30 = sats.iter().filter(|(_, s)| *s < 30.0).count();
    println!("entries with sat < 30: {below30} (all listed, ascending):");
    // variant index within hero for display
    let mut var_idx: Vec<usize> = vec![0; refs.len()];
    {
        let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
        for (i, (k, _)) in refs.iter().enumerate() {
            let c = seen.entry(k.as_str()).or_default();
            var_idx[i] = *c;
            *c += 1;
        }
    }
    for (i, s) in &sats {
        if *s < 30.0 {
            println!("  sat={:>5.1}  {}#{}", s, short(&refs[*i].0), var_idx[*i]);
        }
    }
    println!("min sat = {:.1} ({}), max sat = {:.1}", sats[0].1, short(&refs[sats[0].0].0), sats.last().unwrap().1);

    // =========================== C) SELF-MATCH / CONFUSION ===========================
    println!("\n=== C) SELF-MATCH + CONFUSION (each blob entry as query vs whole blob) ===");
    struct SelfRes {
        idx: usize,
        self_score: f32,
        margin: f32,
        rival: String,
        rival_score: f32,
    }
    let mut results: Vec<SelfRes> = Vec::new();
    for (i, (k, img)) in refs.iter().enumerate() {
        let ranked = rank(img, &refs);
        let (top_key, top_score, _) = best_hero(&ranked);
        // self score = best score among entries with our own key
        let self_score = ranked.iter().find(|(rk, _)| rk == k).map(|(_, s)| *s).unwrap_or(-1.0);
        let (rival, rival_score) = ranked
            .iter()
            .find(|(rk, _)| rk != k)
            .map(|(rk, s)| (rk.clone(), *s))
            .unwrap_or((String::new(), -1.0));
        let margin = self_score - rival_score;
        if top_key != *k {
            println!("  !! entry {} ({}#{}) top1 is NOT itself: top1={} {:.3}", i, short(k), var_idx[i], short(&top_key), top_score);
        }
        results.push(SelfRes { idx: i, self_score, margin, rival, rival_score });
    }
    let low_self: Vec<&SelfRes> = results.iter().filter(|r| r.self_score < 0.9).collect();
    println!("entries with self-score < 0.9: {}", low_self.len());
    for r in &low_self {
        println!("  self={:.3}  {}#{}", r.self_score, short(&refs[r.idx].0), var_idx[r.idx]);
    }
    results.sort_by(|a, b| a.margin.partial_cmp(&b.margin).unwrap());
    println!("20 worst inter-hero margins (query -> closest OTHER hero):");
    println!("{:<32} {:>6} {:>8} {:>8}  {}", "query(entry)", "self", "rival_s", "margin", "rival");
    for r in results.iter().take(20) {
        println!(
            "{:<32} {:>6.3} {:>8.3} {:>8.3}  {}",
            format!("{}#{}", short(&refs[r.idx].0), var_idx[r.idx]),
            r.self_score,
            r.rival_score,
            r.margin,
            short(&r.rival)
        );
    }

    // =========================== D) REAL FRAMES ===========================
    println!("\n=== D) FULL PIPELINE ON NATIVE FRAMES ===");
    let mut frame_paths: Vec<PathBuf> = std::fs::read_dir(&frames_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension().map(|e| e.eq_ignore_ascii_case("png")).unwrap_or(false)
                && p.file_stem().map(|s| s.to_string_lossy().starts_with("tb_")).unwrap_or(false)
        })
        .collect();
    frame_paths.sort();
    println!("frames: {}", frame_paths.len());

    for p in &frame_paths {
        let stem = p.file_stem().unwrap().to_string_lossy().to_string();
        let img = image::open(p).unwrap();
        let Some(loc) = locate(&img, &anchor) else {
            println!("{stem}: ANCHOR FAIL");
            continue;
        };
        println!(
            "{stem}: anchor x={} y={} scale={:.3} score={:.3} margin={:.3}",
            loc.x, loc.y, loc.scale, loc.score, loc.margin
        );
        let rects = portrait_rects(&loc);
        for (i, cell) in rects.iter().enumerate() {
            let team = if i < 5 { "R" } else { "D" };
            match art_band_crop(&img, *cell) {
                None => println!("  slot {i} {team} OOB"),
                Some(crop) => {
                    let q = prep_query(&crop);
                    let sat = art_band_saturation(&q);
                    let ranked = rank(&q, &refs);
                    let (hero, top1, margin) = best_hero(&ranked);
                    // which variant of the winning hero won
                    let win_variant = ranked
                        .iter()
                        .position(|(k, s)| k == &hero && (*s - top1).abs() < 1e-6)
                        .map(|pos| {
                            let (wk, ws) = &ranked[pos];
                            refs.iter()
                                .enumerate()
                                .filter(|(_, (k, _))| k == wk)
                                .find(|(_, (_, t))| (color_ncc(&q, t) - ws).abs() < 1e-6)
                                .map(|(ri, _)| var_idx[ri])
                                .unwrap_or(0)
                        })
                        .unwrap_or(0);
                    let dead = sat < MIN_LIVE_SAT;
                    println!(
                        "  slot {i} {team} sat={:>5.1} {} top1={:<22} score={:.3} margin={:.3} var#{} cls={}",
                        sat,
                        if dead { "DEAD " } else { "alive" },
                        short(&hero),
                        top1,
                        margin,
                        win_variant,
                        if dead { "-" } else { classify(top1, margin) }
                    );
                }
            }
        }
    }
}

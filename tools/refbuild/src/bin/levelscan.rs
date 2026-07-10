//! Offline builder/validator for reading enemy LEVELS from the scoreboard "УР." column.
//!
//! The level lives in a small circle to the right of each row's name (a number, 1..30).
//! We anchor the level box to the hero portrait the validated pipeline already locks, so
//! the level read shares the board's proven alignment.
//!
//! Modes:
//!   dump  <frame> <refs.bin> <own_hero> <own_team> <out.png>
//!         lock the board, crop each row's level box, write a stacked montage (10 rows,
//!         8x) so the box geometry can be eyeballed and tuned.
//!
//! usage: levelscan dump <screenshot.png> <refs.bin> <own_hero_key> <own_team> <out.png>

use image::{DynamicImage, GenericImageView, RgbImage, RgbaImage};

const W: u32 = 48;
const H: u32 = 27;
const PEAK_MIN: f32 = 0.45;
const RELOCK_R: i32 = 6;
const RELOCK_STEP: i32 = 3;

// zones.rs portrait geometry (same constants the app bakes)
const COL_X: f64 = 86.0 / 1918.0;
const CELL_W: f64 = 88.0 / 1918.0;
const CELL_H: f64 = 52.0 / 1078.0;
const RADIANT_Y0: f64 = 95.0 / 1078.0;
const DIRE_Y0: f64 = 479.0 / 1078.0;
const ROW_PITCH: f64 = 70.0 / 1078.0;

// Level box relative to the portrait cell's top-left (in absolute px at 1918x1078).
// Tunable; verified by the dump montage.
const LVL_DX: i32 = 306; // portrait-left -> level-box-left
const LVL_DY: i32 = 8; // portrait-top  -> level-box-top
const LVL_W: u32 = 44;
const LVL_H: u32 = 36;

fn parse_blob(data: &[u8]) -> Vec<(String, RgbImage)> {
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
        if let Some(img) = RgbImage::from_raw(w, h, data[p..p + px].to_vec()) {
            out.push((key, img));
        }
        p += px;
    }
    out
}

fn color_ncc(q: &RgbImage, t: &RgbImage) -> f32 {
    let n = (q.width() * q.height()) as usize;
    if n == 0 || q.dimensions() != t.dimensions() {
        return -1.0;
    }
    let mut total = 0f32;
    for c in 0..3 {
        let qv: Vec<f32> = q.pixels().map(|p| p.0[c] as f32).collect();
        let tv: Vec<f32> = t.pixels().map(|p| p.0[c] as f32).collect();
        let mq = qv.iter().sum::<f32>() / n as f32;
        let mt = tv.iter().sum::<f32>() / n as f32;
        let (mut num, mut dq, mut dt) = (0f32, 0f32, 0f32);
        for i in 0..n {
            let a = qv[i] - mq;
            let b = tv[i] - mt;
            num += a * b;
            dq += a * a;
            dt += b * b;
        }
        let d = (dq * dt).sqrt();
        if d > 0.0 {
            total += num / d;
        }
    }
    total / 3.0
}

fn best(crop: &DynamicImage, refs: &[(String, RgbImage)]) -> (String, f32) {
    let q = crop
        .resize_exact(W, H, image::imageops::FilterType::Lanczos3)
        .to_rgb8();
    let mut s: Vec<(f32, &str)> = refs
        .iter()
        .map(|(k, t)| (color_ncc(&q, t), k.as_str()))
        .collect();
    s.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    (s[0].1.to_string(), s[0].0)
}

/// portrait cells (x,y,w,h) for the 10 rows
fn rects(fw: u32, fh: u32) -> Vec<(i32, i32, u32, u32)> {
    let mut v = Vec::new();
    for row in 0..10 {
        let (y0, idx) = if row < 5 { (RADIANT_Y0, row) } else { (DIRE_Y0, row - 5) };
        let fy = y0 + idx as f64 * ROW_PITCH;
        v.push((
            (COL_X * fw as f64).round() as i32,
            (fy * fh as f64).round() as i32,
            (CELL_W * fw as f64).round() as u32,
            (CELL_H * fh as f64).round() as u32,
        ));
    }
    v
}

/// search a window for the best hero match, returning (hero, top1, dx, dy)
fn search(
    frame: &DynamicImage,
    cell: (i32, i32, u32, u32),
    cx: i32,
    cy: i32,
    radius: i32,
    refs: &[(String, RgbImage)],
) -> (String, f32, i32, i32) {
    let (fw, fh) = frame.dimensions();
    let mut b = (String::new(), -2f32, cx, cy);
    let mut dy = cy - radius;
    while dy <= cy + radius {
        let mut dx = cx - radius;
        while dx <= cx + radius {
            let x = cell.0 + dx;
            let y = cell.1 + dy;
            if x >= 0 && y >= 0 && (x as u32 + cell.2) <= fw && (y as u32 + cell.3) <= fh {
                let (h, t1) = best(&frame.crop_imm(x as u32, y as u32, cell.2, cell.3), refs);
                if t1 > b.1 {
                    b = (h, t1, dx, dy);
                }
            }
            dx += RELOCK_STEP;
        }
        dy += RELOCK_STEP;
    }
    b
}

/// own-row oracle: returns the global lock offset (dx,dy), or None to discard.
fn lock_board(
    frame: &DynamicImage,
    r: &[(i32, i32, u32, u32)],
    own_rows: &[usize],
    own_hero: &str,
    refs: &[(String, RgbImage)],
) -> Option<(i32, i32)> {
    for &row in own_rows {
        let (h, t1, dx, dy) = search(frame, r[row], 0, 0, RELOCK_R, refs);
        if h == own_hero && t1 >= PEAK_MIN {
            return Some((dx, dy));
        }
    }
    None
}

/// the level box for a row, given the global lock offset
fn level_box(cell: (i32, i32, u32, u32), lock: (i32, i32)) -> (i32, i32, u32, u32) {
    (cell.0 + LVL_DX + lock.0, cell.1 + LVL_DY + lock.1, LVL_W, LVL_H)
}

fn dump(frame: &DynamicImage, refs: &[(String, RgbImage)], own_hero: &str, radiant: bool, out: &str) {
    let (fw, fh) = frame.dimensions();
    let r = rects(fw, fh);
    let own: Vec<usize> = if radiant { (0..5).collect() } else { (5..10).collect() };
    let lock = match lock_board(frame, &r, &own, own_hero, refs) {
        Some(l) => {
            println!("board locked at {:?}", l);
            l
        }
        None => {
            println!("own hero not found -> cannot dump");
            return;
        }
    };
    let scale = 5u32;
    let pad = 4u32;
    let montage_w = LVL_W * scale;
    let montage_h = (LVL_H * scale + pad) * 10;
    let mut canvas = RgbaImage::from_pixel(montage_w, montage_h, image::Rgba([30, 30, 36, 255]));
    for row in 0..10 {
        let (bx, by, bw, bh) = level_box(r[row], lock);
        if bx < 0 || by < 0 || (bx as u32 + bw) > fw || (by as u32 + bh) > fh {
            continue;
        }
        let cell = frame.crop_imm(bx as u32, by as u32, bw, bh).to_rgba8();
        let scaled = image::imageops::resize(&cell, bw * scale, bh * scale, image::imageops::FilterType::Nearest);
        let oy = (LVL_H * scale + pad) * row as u32;
        for (dx, dy, p) in scaled.enumerate_pixels() {
            if dx < montage_w && oy + dy < montage_h {
                canvas.put_pixel(dx, oy + dy, *p);
            }
        }
    }
    canvas.save(out).expect("save montage");
    println!("saved montage {out} ({montage_w}x{montage_h}) rows top->bottom = scoreboard rows 0..9");
}

// ---- digit segmentation ----
// The level digits sit in the centre of the circle; a gold XP ring hugs the perimeter.
// A circular mask drops the ring, then a column projection splits 1 or 2 digits.
// Digits sit in a central band; the gold XP ring hugs the perimeter. The ring's arcs are
// THIN (1-3 ink px per column) while digit columns are TALL (>=~8 px). So we mask to the
// digit band, then segment by column ink-HEIGHT: a column is active only if it has enough
// ink, and a run is a digit only if some column is tall. This drops ring arcs by shape.
const INSET_L: u32 = 10;
const INSET_R: u32 = 30; // inclusive
const INSET_T: u32 = 6;
const INSET_B: u32 = 29; // inclusive
const INK_T: u8 = 110; // luminance threshold for "digit ink"
const MIN_COL: u32 = 3; // a column needs this many ink px to be "active" (kills thin arcs)
const MIN_PEAK: u32 = 7; // a run needs one column at least this tall to be a digit

fn lum(p: &image::Rgba<u8>) -> u8 {
    ((p.0[0] as u32 * 77 + p.0[1] as u32 * 150 + p.0[2] as u32 * 29) >> 8) as u8
}

/// Binary ink map (true = digit pixel) of a level box, after rectangular masking.
fn ink_map(box_img: &RgbaImage) -> Vec<Vec<bool>> {
    let (w, h) = box_img.dimensions();
    let mut m = vec![vec![false; w as usize]; h as usize];
    for y in INSET_T..=INSET_B.min(h.saturating_sub(1)) {
        for x in INSET_L..=INSET_R.min(w.saturating_sub(1)) {
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

/// Split the ink map into digit column-runs (x0..x1 inclusive) using column ink-height.
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

/// Grid montage: cols = frames (arg order), rows = scoreboard rows 0..9.
fn grid(refs: &[(String, RgbImage)], own_hero: &str, radiant: bool, out: &str, frames: &[String]) {
    let scale = 3u32;
    let pad = 3u32;
    let cw = LVL_W * scale + pad;
    let ch = LVL_H * scale + pad;
    let mut canvas = RgbaImage::from_pixel(cw * frames.len() as u32, ch * 10, image::Rgba([30, 30, 36, 255]));
    for (col, fpath) in frames.iter().enumerate() {
        let frame = match image::open(fpath) {
            Ok(f) => f,
            Err(e) => {
                println!("col {col}: open failed {e}");
                continue;
            }
        };
        let (fw, fh) = frame.dimensions();
        let r = rects(fw, fh);
        let own: Vec<usize> = if radiant { (0..5).collect() } else { (5..10).collect() };
        let lock = match lock_board(&frame, &r, &own, own_hero, refs) {
            Some(l) => l,
            None => {
                println!("col {col} ({fpath}): own hero not found -> skipped");
                continue;
            }
        };
        for row in 0..10 {
            let (bx, by, bw, bh) = level_box(r[row], lock);
            if bx < 0 || by < 0 || (bx as u32 + bw) > fw || (by as u32 + bh) > fh {
                continue;
            }
            let cell = frame.crop_imm(bx as u32, by as u32, bw, bh).to_rgba8();
            let scaled = image::imageops::resize(&cell, bw * scale, bh * scale, image::imageops::FilterType::Nearest);
            let ox = cw * col as u32;
            let oy = ch * row as u32;
            for (dx, dy, p) in scaled.enumerate_pixels() {
                let (px, py) = (ox + dx, oy + dy);
                if px < canvas.width() && py < canvas.height() {
                    canvas.put_pixel(px, py, *p);
                }
            }
        }
        println!("col {col}: {fpath} locked {:?}", lock);
    }
    canvas.save(out).expect("save grid");
    println!("saved grid {out} ({}x{}) cols=frames rows=scoreboard 0..9", canvas.width(), canvas.height());
}

/// Visualize segmentation: per row, [original | ink map] with run count printed.
fn seg(frame: &DynamicImage, refs: &[(String, RgbImage)], own_hero: &str, radiant: bool, out: &str) {
    let (fw, fh) = frame.dimensions();
    let r = rects(fw, fh);
    let own: Vec<usize> = if radiant { (0..5).collect() } else { (5..10).collect() };
    let lock = match lock_board(frame, &r, &own, own_hero, refs) {
        Some(l) => l,
        None => {
            println!("own hero not found");
            return;
        }
    };
    let scale = 5u32;
    let pad = 4u32;
    let panel_w = LVL_W * scale;
    let row_h = LVL_H * scale + pad;
    let mut canvas = RgbaImage::from_pixel(panel_w * 2 + pad, row_h * 10, image::Rgba([20, 20, 24, 255]));
    for row in 0..10 {
        let (bx, by, bw, bh) = level_box(r[row], lock);
        if bx < 0 || by < 0 || (bx as u32 + bw) > fw || (by as u32 + bh) > fh {
            continue;
        }
        let cell = frame.crop_imm(bx as u32, by as u32, bw, bh).to_rgba8();
        let m = ink_map(&cell);
        let runs = digit_runs(&m);
        // left: original
        let orig = image::imageops::resize(&cell, bw * scale, bh * scale, image::imageops::FilterType::Nearest);
        let oy = row_h * row as u32;
        for (dx, dy, p) in orig.enumerate_pixels() {
            canvas.put_pixel(dx, oy + dy, *p);
        }
        // right: ink map with run separators (red)
        let ox = panel_w + pad;
        for y in 0..bh {
            for x in 0..bw {
                let on = m[y as usize][x as usize];
                let col = if on { image::Rgba([240, 230, 180, 255]) } else { image::Rgba([30, 30, 36, 255]) };
                for sy in 0..scale {
                    for sx in 0..scale {
                        canvas.put_pixel(ox + x * scale + sx, oy + y * scale + sy, col);
                    }
                }
            }
        }
        for (rs, re) in &runs {
            for y in 0..bh * scale {
                canvas.put_pixel(ox + (*rs as u32) * scale, oy + y, image::Rgba([220, 60, 60, 255]));
                canvas.put_pixel(ox + (*re as u32 + 1) * scale - 1, oy + y, image::Rgba([60, 120, 220, 255]));
            }
        }
        println!("row {row}: {} runs at {:?}", runs.len(), runs);
    }
    canvas.save(out).expect("save seg");
    println!("saved seg {out} ({}x{})", canvas.width(), canvas.height());
}

// ---- digit glyphs & classifier ----
const GW: u32 = 14;
const GH: u32 = 20;
const DIGIT_PEAK_MIN: f32 = 0.75;
const DIGIT_MARGIN_MIN: f32 = 0.20;

/// Tight-cropped, canonical-size binary glyph for one column-run (values 0/255 as f32).
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
            g.put_pixel(x as u32 - minx as u32, y as u32 - miny as u32, image::Luma([if m[y][x] { 255 } else { 0 }]));
        }
    }
    let r = image::imageops::resize(&g, GW, GH, image::imageops::FilterType::Triangle);
    Some(r.pixels().map(|p| p.0[0] as f32).collect())
}

fn zncc(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len() as f32;
    if n == 0.0 {
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

/// (digit, top1, margin) for a glyph against the 0-9 prototypes.
fn classify(glyph: &[f32], templates: &[(u8, Vec<f32>)]) -> (u8, f32, f32) {
    let mut s: Vec<(f32, u8)> = templates.iter().map(|(d, t)| (zncc(glyph, t), *d)).collect();
    s.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let s1 = s.get(1).map(|x| x.0).unwrap_or(-1.0);
    (s[0].1, s[0].0, s[0].0 - s1)
}

/// Per-digit (digit, peak, margin) for a box, ignoring gates — for threshold measurement.
fn read_level_debug(box_img: &RgbaImage, templates: &[(u8, Vec<f32>)]) -> Vec<(u8, f32, f32)> {
    let m = ink_map(box_img);
    let runs = digit_runs(&m);
    let mut out = Vec::new();
    for run in runs {
        if let Some(g) = glyph_from_run(&m, run) {
            out.push(classify(&g, templates));
        }
    }
    out
}

/// Read a level box -> Some((level, min_digit_confidence)) or None when unsure.
fn read_level(box_img: &RgbaImage, templates: &[(u8, Vec<f32>)]) -> Option<(u32, f32)> {
    let m = ink_map(box_img);
    let runs = digit_runs(&m);
    if runs.is_empty() || runs.len() > 2 {
        return None;
    }
    let mut level = 0u32;
    let mut minconf = 1.0f32;
    for run in runs {
        let g = glyph_from_run(&m, run)?;
        let (d, top1, margin) = classify(&g, templates);
        if top1 < DIGIT_PEAK_MIN || margin < DIGIT_MARGIN_MIN {
            return None;
        }
        level = level * 10 + d as u32;
        minconf = minconf.min(top1);
    }
    if !(1..=30).contains(&level) {
        return None;
    }
    Some((level, minconf))
}

// Ground truth for the 9 calibration frames (rows 0..9 x cols 0..8, time order).
const TRUTH: [[u32; 9]; 10] = [
    [8, 11, 13, 19, 20, 21, 25, 27, 29],
    [6, 9, 12, 18, 19, 24, 27, 27, 28],
    [7, 11, 14, 20, 21, 26, 29, 29, 29],
    [5, 8, 12, 19, 20, 23, 25, 26, 30],
    [6, 7, 16, 20, 21, 24, 28, 30, 30],
    [6, 10, 14, 19, 20, 25, 28, 30, 30],
    [6, 10, 16, 20, 20, 24, 25, 27, 30],
    [10, 12, 17, 22, 23, 25, 25, 26, 28],
    [5, 9, 15, 23, 23, 25, 28, 29, 30],
    [6, 11, 15, 19, 20, 24, 26, 26, 30],
];

/// extract the 10 level boxes of one frame (None per row if off-frame / not locked)
fn frame_boxes(frame: &DynamicImage, refs: &[(String, RgbImage)], own_hero: &str, radiant: bool) -> Option<Vec<Option<RgbaImage>>> {
    let (fw, fh) = frame.dimensions();
    let r = rects(fw, fh);
    let own: Vec<usize> = if radiant { (0..5).collect() } else { (5..10).collect() };
    let lock = lock_board(frame, &r, &own, own_hero, refs)?;
    let mut out = Vec::with_capacity(10);
    for row in 0..10 {
        let (bx, by, bw, bh) = level_box(r[row], lock);
        if bx < 0 || by < 0 || (bx as u32 + bw) > fw || (by as u32 + bh) > fh {
            out.push(None);
        } else {
            out.push(Some(frame.crop_imm(bx as u32, by as u32, bw, bh).to_rgba8()));
        }
    }
    Some(out)
}

/// Build digit prototypes from the calibration frames (averaged binary glyphs) and write
/// the bundled blob. Frames must be passed in TRUTH column order (time order).
fn build(refs: &[(String, RgbImage)], own_hero: &str, radiant: bool, out: &str, frames: &[String]) {
    let mut sums: Vec<Vec<f32>> = vec![vec![0.0; (GW * GH) as usize]; 10];
    let mut counts = [0u32; 10];
    let mut used = 0;
    let mut skipped = 0;
    for (col, fpath) in frames.iter().enumerate() {
        let frame = image::open(fpath).expect("open frame");
        let boxes = match frame_boxes(&frame, refs, own_hero, radiant) {
            Some(b) => b,
            None => {
                println!("col {col}: not locked, skipped");
                continue;
            }
        };
        for row in 0..10 {
            let Some(bx) = &boxes[row] else { continue };
            let truth = TRUTH[row][col];
            let chars: Vec<u8> = truth.to_string().bytes().map(|b| b - b'0').collect();
            let m = ink_map(bx);
            let runs = digit_runs(&m);
            if runs.len() != chars.len() {
                skipped += 1;
                println!("  skip col{col} row{row} (truth {truth}): {} runs != {} digits", runs.len(), chars.len());
                continue;
            }
            for (i, run) in runs.iter().enumerate() {
                if let Some(g) = glyph_from_run(&m, *run) {
                    let d = chars[i] as usize;
                    for k in 0..g.len() {
                        sums[d][k] += g[k];
                    }
                    counts[d] += 1;
                    used += 1;
                }
            }
        }
    }
    let mut templates: Vec<(u8, Vec<f32>)> = Vec::new();
    for d in 0..10 {
        if counts[d] == 0 {
            println!("WARNING: digit {d} has no samples!");
            continue;
        }
        let proto: Vec<f32> = sums[d].iter().map(|s| s / counts[d] as f32).collect();
        templates.push((d as u8, proto));
    }
    // write blob: "VAELDREF"(8) ver(1) GW(u16) GH(u16) count(u16) then per: digit(1) + GW*GH bytes
    let mut blob: Vec<u8> = Vec::new();
    blob.extend_from_slice(b"VAELDREF");
    blob.push(1);
    blob.extend_from_slice(&(GW as u16).to_le_bytes());
    blob.extend_from_slice(&(GH as u16).to_le_bytes());
    blob.extend_from_slice(&(templates.len() as u16).to_le_bytes());
    for (d, proto) in &templates {
        blob.push(*d);
        for v in proto {
            blob.push(v.round().clamp(0.0, 255.0) as u8);
        }
    }
    std::fs::write(out, &blob).expect("write blob");
    println!("built {out}: {} digits, {used} glyph samples used, {skipped} cells skipped, counts {:?}", templates.len(), counts);
}

fn load_digits(path: &str) -> Vec<(u8, Vec<f32>)> {
    let data = std::fs::read(path).expect("read digits blob");
    assert!(data.len() >= 15 && &data[0..8] == b"VAELDREF", "bad digits blob");
    let gw = u16::from_le_bytes([data[9], data[10]]) as usize;
    let gh = u16::from_le_bytes([data[11], data[12]]) as usize;
    let count = u16::from_le_bytes([data[13], data[14]]) as usize;
    let px = gw * gh;
    let mut p = 15usize;
    let mut out = Vec::new();
    for _ in 0..count {
        let d = data[p];
        p += 1;
        let proto: Vec<f32> = data[p..p + px].iter().map(|&b| b as f32).collect();
        p += px;
        out.push((d, proto));
    }
    out
}

/// Validate the reader against TRUTH on all 10 rows of every frame. Reports coverage and
/// any MISMATCH (which must be zero for "never wrong").
fn validate(refs: &[(String, RgbImage)], digits: &str, own_hero: &str, radiant: bool, frames: &[String]) {
    let templates = load_digits(digits);
    let (mut correct, mut unsure, mut wrong, mut total) = (0, 0, 0, 0);
    for (col, fpath) in frames.iter().enumerate() {
        let frame = image::open(fpath).expect("open frame");
        let boxes = match frame_boxes(&frame, refs, own_hero, radiant) {
            Some(b) => b,
            None => {
                println!("col {col}: not locked");
                continue;
            }
        };
        for row in 0..10 {
            let Some(bx) = &boxes[row] else { continue };
            total += 1;
            let truth = TRUTH[row][col];
            match read_level(bx, &templates) {
                Some((lvl, conf)) => {
                    if lvl == truth {
                        correct += 1;
                    } else {
                        wrong += 1;
                        println!("  *** WRONG col{col} row{row}: read {lvl} (conf {conf:.3}) truth {truth}");
                    }
                }
                None => {
                    unsure += 1;
                    println!("  unsure col{col} row{row}: truth {truth}");
                }
            }
        }
    }
    // Measure the margin/peak floor of TRUE digit reads, and the best margin/peak any
    // WRONG digit achieves — the safe gate sits between them.
    let (mut true_min_peak, mut true_min_margin) = (f32::MAX, f32::MAX);
    let (mut wrong_max_peak, mut wrong_max_margin) = (f32::MIN, f32::MIN);
    let mut true_margins: Vec<f32> = Vec::new();
    for (col, fpath) in frames.iter().enumerate() {
        let frame = image::open(fpath).expect("open frame");
        let Some(boxes) = frame_boxes(&frame, refs, own_hero, radiant) else { continue };
        for row in 0..10 {
            let Some(bx) = &boxes[row] else { continue };
            let truth = TRUTH[row][col];
            let chars: Vec<u8> = truth.to_string().bytes().map(|b| b - b'0').collect();
            let digs = read_level_debug(bx, &templates);
            if digs.len() != chars.len() {
                continue;
            }
            for (i, (d, peak, margin)) in digs.iter().enumerate() {
                if *d == chars[i] {
                    true_min_peak = true_min_peak.min(*peak);
                    true_min_margin = true_min_margin.min(*margin);
                    true_margins.push(*margin);
                } else {
                    wrong_max_peak = wrong_max_peak.max(*peak);
                    wrong_max_margin = wrong_max_margin.max(*margin);
                }
            }
        }
    }
    true_margins.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p5 = true_margins.get(true_margins.len() / 20).copied().unwrap_or(0.0);
    println!(
        "digit gate: TRUE reads min_peak {true_min_peak:.3} min_margin {true_min_margin:.3} (5th-pct margin {p5:.3}); WRONG digit best_peak {wrong_max_peak:.3} best_margin {wrong_max_margin:.3}"
    );
    println!("--- validate: {total} cells | correct {correct} | unsure {unsure} | WRONG {wrong} ---");
    if total > 0 {
        println!("coverage {:.1}% | accuracy-on-emitted {}",
            100.0 * correct as f32 / total as f32,
            if correct + wrong > 0 { format!("{:.1}%", 100.0 * correct as f32 / (correct + wrong) as f32) } else { "n/a".into() });
    }
}

/// Held-out check: train prototypes on EVEN time-columns, test on ODD (and report).
/// Frames must be all 9 in TRUTH column order.
fn holdout(refs: &[(String, RgbImage)], own_hero: &str, radiant: bool, frames: &[String]) {
    let mut sums: Vec<Vec<f32>> = vec![vec![0.0; (GW * GH) as usize]; 10];
    let mut counts = [0u32; 10];
    let mut boxes_by_col: Vec<Option<Vec<Option<RgbaImage>>>> = Vec::new();
    for fpath in frames {
        let frame = image::open(fpath).expect("open frame");
        boxes_by_col.push(frame_boxes(&frame, refs, own_hero, radiant));
    }
    // train on even cols
    for (col, boxes) in boxes_by_col.iter().enumerate() {
        if col % 2 != 0 {
            continue;
        }
        let Some(boxes) = boxes else { continue };
        for row in 0..10 {
            let Some(bx) = &boxes[row] else { continue };
            let truth = TRUTH[row][col];
            let chars: Vec<u8> = truth.to_string().bytes().map(|b| b - b'0').collect();
            let m = ink_map(bx);
            let runs = digit_runs(&m);
            if runs.len() != chars.len() {
                continue;
            }
            for (i, run) in runs.iter().enumerate() {
                if let Some(g) = glyph_from_run(&m, *run) {
                    let d = chars[i] as usize;
                    for k in 0..g.len() {
                        sums[d][k] += g[k];
                    }
                    counts[d] += 1;
                }
            }
        }
    }
    let templates: Vec<(u8, Vec<f32>)> = (0..10)
        .filter(|&d| counts[d] > 0)
        .map(|d| (d as u8, sums[d].iter().map(|s| s / counts[d] as f32).collect()))
        .collect();
    println!("holdout: trained on EVEN cols, digit counts {:?}", counts);
    // test on odd cols
    let (mut correct, mut unsure, mut wrong, mut total) = (0, 0, 0, 0);
    for (col, boxes) in boxes_by_col.iter().enumerate() {
        if col % 2 == 0 {
            continue;
        }
        let Some(boxes) = boxes else { continue };
        for row in 0..10 {
            let Some(bx) = &boxes[row] else { continue };
            total += 1;
            let truth = TRUTH[row][col];
            match read_level(bx, &templates) {
                Some((lvl, conf)) => {
                    if lvl == truth {
                        correct += 1;
                    } else {
                        wrong += 1;
                        println!("  *** WRONG col{col} row{row}: read {lvl} (conf {conf:.3}) truth {truth}");
                    }
                }
                None => {
                    unsure += 1;
                    println!("  unsure col{col} row{row}: truth {truth}");
                }
            }
        }
    }
    println!("--- holdout test (ODD cols): {total} cells | correct {correct} | unsure {unsure} | WRONG {wrong} ---");
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 2 {
        eprintln!("usage: levelscan dump|grid|seg|build|validate|holdout ...");
        std::process::exit(2);
    }
    match a[1].as_str() {
        "dump" => {
            if a.len() < 7 {
                eprintln!("usage: levelscan dump <frame> <refs.bin> <own_hero> <own_team> <out.png>");
                std::process::exit(2);
            }
            let frame = image::open(&a[2]).expect("open frame");
            let refs = parse_blob(&std::fs::read(&a[3]).expect("read refs"));
            let radiant = a[5].eq_ignore_ascii_case("radiant");
            dump(&frame, &refs, &a[4], radiant, &a[6]);
        }
        "grid" => {
            if a.len() < 7 {
                eprintln!("usage: levelscan grid <refs.bin> <own_hero> <own_team> <out.png> <frame1> [frame2 ...]");
                std::process::exit(2);
            }
            let refs = parse_blob(&std::fs::read(&a[2]).expect("read refs"));
            let radiant = a[4].eq_ignore_ascii_case("radiant");
            grid(&refs, &a[3], radiant, &a[5], &a[6..]);
        }
        "seg" => {
            if a.len() < 7 {
                eprintln!("usage: levelscan seg <frame> <refs.bin> <own_hero> <own_team> <out.png>");
                std::process::exit(2);
            }
            let frame = image::open(&a[2]).expect("open frame");
            let refs = parse_blob(&std::fs::read(&a[3]).expect("read refs"));
            let radiant = a[5].eq_ignore_ascii_case("radiant");
            seg(&frame, &refs, &a[4], radiant, &a[6]);
        }
        "build" => {
            if a.len() < 7 {
                eprintln!("usage: levelscan build <refs.bin> <own_hero> <own_team> <out.bin> <frame0..8 time order>");
                std::process::exit(2);
            }
            let refs = parse_blob(&std::fs::read(&a[2]).expect("read refs"));
            let radiant = a[4].eq_ignore_ascii_case("radiant");
            build(&refs, &a[3], radiant, &a[5], &a[6..]);
        }
        "validate" => {
            if a.len() < 8 {
                eprintln!("usage: levelscan validate <refs.bin> <digits.bin> <own_hero> <own_team> <frame0..8 time order>");
                std::process::exit(2);
            }
            let refs = parse_blob(&std::fs::read(&a[2]).expect("read refs"));
            let radiant = a[5].eq_ignore_ascii_case("radiant");
            validate(&refs, &a[3], &a[4], radiant, &a[6..]);
        }
        "holdout" => {
            if a.len() < 7 {
                eprintln!("usage: levelscan holdout <refs.bin> <own_hero> <own_team> <frame0..8 time order>");
                std::process::exit(2);
            }
            let refs = parse_blob(&std::fs::read(&a[2]).expect("read refs"));
            let radiant = a[4].eq_ignore_ascii_case("radiant");
            holdout(&refs, &a[3], radiant, &a[5..]);
        }
        other => {
            eprintln!("unknown mode: {other}");
            std::process::exit(2);
        }
    }
}

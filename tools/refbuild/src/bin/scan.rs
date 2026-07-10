//! Real-frame end-to-end validation: mirrors the app's vision::pipeline (zones geometry →
//! GSI own-row oracle → reuse the lock offset → margin gate → duplicate guard) against a
//! screenshot, so the FINAL algorithm can be checked on a real scoreboard.
//!
//! usage: scan <screenshot.png> <refs.bin> <own_hero_key> <own_team:radiant|dire>

use image::{DynamicImage, GenericImageView, RgbImage};

const W: u32 = 48;
const H: u32 = 27;
const PEAK_MIN: f32 = 0.45;
const MARGIN_MIN: f32 = 0.15;
const RELOCK_R: i32 = 6;
const RELOCK_STEP: i32 = 3;

// zones.rs geometry
const COL_X: f64 = 86.0 / 1918.0;
const CELL_W: f64 = 88.0 / 1918.0;
const CELL_H: f64 = 52.0 / 1078.0;
const RADIANT_Y0: f64 = 95.0 / 1078.0;
const DIRE_Y0: f64 = 479.0 / 1078.0;
const ROW_PITCH: f64 = 70.0 / 1078.0;

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

fn best(crop: &DynamicImage, refs: &[(String, RgbImage)]) -> (String, f32, f32) {
    let q = crop.resize_exact(W, H, image::imageops::FilterType::Lanczos3).to_rgb8();
    let mut s: Vec<(f32, &str)> = refs.iter().map(|(k, t)| (color_ncc(&q, t), k.as_str())).collect();
    s.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let s1 = s.get(1).map(|x| x.0).unwrap_or(0.0);
    (s[0].1.to_string(), s[0].0, s[0].0 - s1)
}

fn rects(fw: u32, fh: u32) -> Vec<(u32, u32, u32, u32)> {
    let mut v = Vec::new();
    for row in 0..10 {
        let (y0, idx) = if row < 5 { (RADIANT_Y0, row) } else { (DIRE_Y0, row - 5) };
        let fy = y0 + idx as f64 * ROW_PITCH;
        v.push((
            (COL_X * fw as f64).round() as u32,
            (fy * fh as f64).round() as u32,
            (CELL_W * fw as f64).round() as u32,
            (CELL_H * fh as f64).round() as u32,
        ));
    }
    v
}

fn search(frame: &DynamicImage, cell: (u32, u32, u32, u32), cx: i32, cy: i32, radius: i32, refs: &[(String, RgbImage)]) -> (String, f32, f32, i32, i32) {
    let (fw, fh) = frame.dimensions();
    let mut b = (String::new(), -2f32, 0f32, cx, cy);
    let mut dy = cy - radius;
    while dy <= cy + radius {
        let mut dx = cx - radius;
        while dx <= cx + radius {
            let x = cell.0 as i32 + dx;
            let y = cell.1 as i32 + dy;
            if x >= 0 && y >= 0 && (x as u32 + cell.2) <= fw && (y as u32 + cell.3) <= fh {
                let (h, t1, m) = best(&frame.crop_imm(x as u32, y as u32, cell.2, cell.3), refs);
                if t1 > b.1 {
                    b = (h, t1, m, dx, dy);
                }
            }
            dx += RELOCK_STEP;
        }
        dy += RELOCK_STEP;
    }
    b
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 5 {
        eprintln!("usage: scan <screenshot.png> <refs.bin> <own_hero_key> <own_team>");
        std::process::exit(2);
    }
    let frame = image::open(&a[1]).expect("open");
    let refs = parse_blob(&std::fs::read(&a[2]).expect("refs"));
    let own_hero = &a[3];
    let radiant = a[4].eq_ignore_ascii_case("radiant");
    let (fw, fh) = frame.dimensions();
    let r = rects(fw, fh);
    let (own, enemy): (Vec<usize>, Vec<usize>) = if radiant { ((0..5).collect(), (5..10).collect()) } else { ((5..10).collect(), (0..5).collect()) };

    // own-row oracle → lock offset
    let mut lock: Option<(i32, i32)> = None;
    for &row in &own {
        let (h, t1, _m, dx, dy) = search(&frame, r[row], 0, 0, RELOCK_R, &refs);
        println!("own row {row}: {h} peak {t1:.3}");
        if h == *own_hero && t1 >= PEAK_MIN {
            lock = Some((dx, dy));
            println!("  -> OWN HERO LOCKED at offset ({dx},{dy})");
            break;
        }
    }
    let Some((lx, ly)) = lock else {
        println!("RESULT: own hero not found → capture DISCARDED (no enemy facts shipped)");
        return;
    };
    println!("--- enemy reads (lock {lx},{ly}, refined +/-{RELOCK_STEP}) ---");
    for &row in &enemy {
        let (h, t1, m, _, _) = search(&frame, r[row], lx, ly, RELOCK_STEP, &refs);
        let status = if t1 >= PEAK_MIN && m >= MARGIN_MIN { "known" } else { "unconfirmed" };
        println!("  row {row}: {h:<28} peak {t1:.3} margin {m:.3} -> {status}");
    }
}

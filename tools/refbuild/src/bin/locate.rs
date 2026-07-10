//! Offline calibration + validation tool. Slides the color-ZNCC matcher over the
//! left region of a real screenshot to auto-find the scoreboard portrait rows, with no
//! hand-measured pixel coordinates. Prints, for each detected peak, the locked (x,y),
//! the best hero, the top-1 ZNCC score and the top1−top2 margin. Used to (a) validate
//! the matcher on a real frame against known ground truth and (b) derive the HUD zone
//! geometry the app bakes in.
//!
//! usage: locate <screenshot.png> <heroes_color_refs.bin> [x0 y0 x1 y1 cw ch stride]

use image::{DynamicImage, RgbImage};

const W: u32 = 48;
const H: u32 = 27;

fn parse_blob(data: &[u8]) -> Vec<(String, RgbImage)> {
    let mut out = Vec::new();
    if data.len() < 15 || &data[0..8] != b"VAELCREF" {
        return out;
    }
    let w = u16::from_le_bytes([data[9], data[10]]) as u32;
    let h = u16::from_le_bytes([data[11], data[12]]) as u32;
    let count = u16::from_le_bytes([data[13], data[14]]) as usize;
    let px = (w * h * 3) as usize;
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

fn prep(img: &DynamicImage) -> RgbImage {
    img.resize_exact(W, H, image::imageops::FilterType::Lanczos3)
        .to_rgb8()
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
        let denom = (dq * dt).sqrt();
        if denom > 0.0 {
            total += num / denom;
        }
    }
    total / 3.0
}

fn best(q: &RgbImage, refs: &[(String, RgbImage)]) -> (String, f32, f32) {
    let mut s: Vec<(f32, &str)> = refs.iter().map(|(k, t)| (color_ncc(q, t), k.as_str())).collect();
    s.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let (s0, k0) = s[0];
    let s1 = s.get(1).map(|x| x.0).unwrap_or(0.0);
    (k0.to_string(), s0, s0 - s1)
}

fn arg<T: std::str::FromStr>(a: &[String], i: usize, d: T) -> T {
    a.get(i).and_then(|s| s.parse().ok()).unwrap_or(d)
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 3 {
        eprintln!("usage: locate <screenshot.png> <refs.bin> [x0 y0 x1 y1 cw ch stride]");
        std::process::exit(2);
    }
    let shot = image::open(&a[1]).expect("open screenshot");
    let refs = parse_blob(&std::fs::read(&a[2]).expect("read refs"));
    eprintln!("refs: {}  screenshot: {}x{}", refs.len(), shot.width(), shot.height());

    let x0 = arg(&a, 3, 70u32);
    let y0 = arg(&a, 4, 55u32);
    let x1 = arg(&a, 5, 225u32);
    let y1 = arg(&a, 6, 935u32);
    let cw = arg(&a, 7, 88u32);
    let ch = arg(&a, 8, 52u32);
    let stride = arg(&a, 9, 4u32);

    // Heatmap: best top-1 score + hero + margin at every grid position.
    let mut cells: Vec<(u32, u32, f32, f32, String)> = Vec::new();
    let mut y = y0;
    while y + ch <= y1 {
        let mut x = x0;
        while x + cw <= x1 {
            let crop = shot.crop_imm(x, y, cw, ch);
            let q = prep(&crop);
            let (hero, top1, margin) = best(&q, &refs);
            cells.push((x, y, top1, margin, hero));
            x += stride;
        }
        y += stride;
    }
    // Greedy non-max suppression to pull out distinct portrait peaks.
    cells.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
    let mut peaks: Vec<(u32, u32, f32, f32, String)> = Vec::new();
    for c in &cells {
        let near = peaks.iter().any(|p| {
            (p.0 as i64 - c.0 as i64).abs() < (cw as i64 * 7 / 10)
                && (p.1 as i64 - c.1 as i64).abs() < (ch as i64 * 7 / 10)
        });
        if !near {
            peaks.push(c.clone());
        }
        if peaks.len() >= 14 {
            break;
        }
    }
    peaks.sort_by_key(|p| p.1);
    println!("locked peaks (sorted by y):  x,y  top1  margin  hero");
    for (x, y, top1, margin, hero) in &peaks {
        let status = if *top1 >= 0.40 && *margin >= 0.10 { "KNOWN" } else { "unconfirmed" };
        println!("  ({:>4},{:>4})  {:.3}  {:.3}  {:<28} {}", x, y, top1, margin, hero, status);
    }
}

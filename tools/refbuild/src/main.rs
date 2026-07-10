//! Pack the canonical hero portraits (extracted from the Dota VPK) into the
//! bundled color-reference blob `heroes_color_refs.bin` that the app's color-ZNCC
//! matcher loads. The blob stores each portrait already resized to the canonical
//! match size (48x27 RGB) with the SAME `image` Lanczos3 path the runtime uses for
//! query crops, so reference and query go through identical processing.
//!
//! usage: refbuild <heroes_png_dir> <canonical_keys_table.txt> <out.bin>
//!   heroes_png_dir       — folder of `npc_dota_hero_<hero>_png.png` (Source2Viewer dump)
//!   canonical_keys_table — heroes_sb_phash.txt (its `key,...` lines pin the ~126 valid heroes)
//!   out.bin              — desktop/src-tauri/src/vision/heroes_color_refs.bin
//!
//! Blob format (little-endian):
//!   magic   "VAELCREF" (8 bytes)
//!   version u8  = 1
//!   w       u16 = 48
//!   h       u16 = 27
//!   count   u16
//!   entries count × { key_len u8, key bytes (utf8), w*h*3 RGB bytes (row-major) }

use std::io::Write;

const W: u32 = 48;
const H: u32 = 27;

/// Pull the canonical hero keys from the bundled `key,base64` table (skips `#`/blank
/// lines). This pins the set to the ~126 real heroes and drops `_alt`/`_persona`/
/// summon/coach VPK textures that would otherwise produce invalid dotaconstants keys.
fn parse_keys(table: &str) -> Vec<String> {
    let mut v = Vec::new();
    for line in table.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, _)) = line.split_once(',') {
            let k = k.trim();
            if k.starts_with("npc_dota_hero_") {
                v.push(k.to_string());
            }
        }
    }
    v
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("usage: refbuild <heroes_png_dir> <canonical_keys_table.txt> <out.bin>");
        std::process::exit(2);
    }
    let dir = &args[1];
    let table = std::fs::read_to_string(&args[2]).expect("read canonical keys table");
    let keys = parse_keys(&table);
    eprintln!("canonical keys: {}", keys.len());

    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    for key in &keys {
        let p = format!("{dir}/{key}_png.png");
        match image::open(&p) {
            Ok(img) => {
                let rgb = img
                    .resize_exact(W, H, image::imageops::FilterType::Lanczos3)
                    .to_rgb8();
                entries.push((key.clone(), rgb.into_raw()));
            }
            Err(_) => missing.push(key.clone()),
        }
    }
    if !missing.is_empty() {
        eprintln!(
            "WARNING: {} portraits missing (those heroes stay unrecognized/unconfirmed): {:?}",
            missing.len(),
            missing
        );
    }
    eprintln!("packed entries: {}", entries.len());

    let mut buf: Vec<u8> = Vec::new();
    buf.extend_from_slice(b"VAELCREF");
    buf.push(1u8);
    buf.extend_from_slice(&(W as u16).to_le_bytes());
    buf.extend_from_slice(&(H as u16).to_le_bytes());
    buf.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    for (key, rgb) in &entries {
        let kb = key.as_bytes();
        assert!(kb.len() <= 255, "key too long: {key}");
        assert_eq!(rgb.len(), (W * H * 3) as usize);
        buf.push(kb.len() as u8);
        buf.extend_from_slice(kb);
        buf.extend_from_slice(rgb);
    }

    std::fs::File::create(&args[3])
        .and_then(|mut f| f.write_all(&buf))
        .expect("write out.bin");
    eprintln!("wrote {} bytes to {}", buf.len(), &args[3]);
}

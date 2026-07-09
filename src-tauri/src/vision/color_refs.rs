//! Bundled color reference library for the ZNCC matcher.
//!
//! `heroes_color_refs.bin` is generated offline by `desktop/tools/refbuild` from the
//! real in-game HUD portraits extracted from the Dota VPK (the validated reference
//! surface — web/CDN art does NOT match what the HUD renders). Each entry is the
//! portrait already resized to the canonical match size (48x27 RGB), so the end user
//! needs neither Dota nor the extractor — the refs ship inside the app. Regenerate on
//! a Dota patch: re-extract the VPK portraits, then re-run `refbuild`.
//!
//! Blob format (little-endian): magic "VAELCREF", version u8, w u16, h u16, count u16,
//! then count × { key_len u8, key bytes (utf8), w*h*3 RGB bytes }.

use std::sync::OnceLock;

use image::RgbImage;

const BLOB: &[u8] = include_bytes!("heroes_color_refs.bin");

/// Parse the packed blob into (hero_key, 48x27 RGB) templates. Any malformation
/// yields whatever parsed cleanly before it (empty on a bad header) — a missing ref
/// just never matches → that hero stays `unconfirmed`, which is the safe failure.
pub fn parse_blob(data: &[u8]) -> Vec<(String, RgbImage)> {
    let mut out = Vec::new();
    if data.len() < 15 || &data[0..8] != b"VAELCREF" {
        return out;
    }
    let w = u16::from_le_bytes([data[9], data[10]]) as u32;
    let h = u16::from_le_bytes([data[11], data[12]]) as u32;
    let count = u16::from_le_bytes([data[13], data[14]]) as usize;
    // Guard against a malformed header overflowing the byte count (would panic in debug).
    let px = match (w as usize).checked_mul(h as usize).and_then(|n| n.checked_mul(3)) {
        Some(p) if p > 0 => p,
        _ => return out,
    };
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
        let key = match std::str::from_utf8(&data[p..p + kl]) {
            Ok(s) => s.to_string(),
            Err(_) => break,
        };
        p += kl;
        let raw = data[p..p + px].to_vec();
        p += px;
        if let Some(img) = RgbImage::from_raw(w, h, raw) {
            out.push((key, img));
        }
    }
    out
}

/// The bundled reference library, parsed once.
pub fn load() -> &'static [(String, RgbImage)] {
    static REFS: OnceLock<Vec<(String, RgbImage)>> = OnceLock::new();
    REFS.get_or_init(|| parse_blob(BLOB))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vision::ncc;

    #[test]
    fn bundled_blob_loads_all_heroes_at_canonical_size() {
        let refs = load();
        assert!(
            refs.len() >= 120,
            "expected ~126 hero color refs, got {}",
            refs.len()
        );
        assert!(refs.iter().all(|(k, _)| k.starts_with("npc_dota_hero_")));
        assert!(refs
            .iter()
            .all(|(_, img)| img.width() == ncc::W && img.height() == ncc::H));
    }

    #[test]
    fn keys_are_unique() {
        let refs = load();
        let mut keys: Vec<&str> = refs.iter().map(|(k, _)| k.as_str()).collect();
        keys.sort_unstable();
        let n = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), n, "duplicate hero keys in the reference blob");
    }

    #[test]
    fn bad_header_yields_empty() {
        assert!(parse_blob(b"not-a-blob").is_empty());
        assert!(parse_blob(&[]).is_empty());
    }
}

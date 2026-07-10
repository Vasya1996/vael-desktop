//! Bundled color reference library for the TOP-BAR hero portraits (the 10 cells
//! flanking the clock panel), matched with the same color-ZNCC machinery as the
//! scoreboard (`ncc::color_ncc` / `ncc::rank`).
//!
//! `topbar_color_refs.bin` is generated offline by `desktop/tools/refbuild` (bin
//! `topbar_refs`) from the VPK top-bar art (128x72 `panorama/images/heroes` PNGs),
//! CALIBRATED against real 1920x1080 frames: the baked window is the vertical band
//! y0_frac=0.05 h_frac=0.95 of the source art at full width, resized to 48x27 — the
//! winning parameters of the calibration gate (mean inter-hero margin 0.294, zero
//! per-match consistency / per-frame distinctness violations across 12 frames).
//! Regenerate on a Dota patch: re-extract the VPK art, re-run calibrate + emit.
//!
//! Unlike the scoreboard blob, keys are NOT unique: `_altN` arcana art (e.g.
//! Juggernaut's Bladeform Legacy, which replaces his top-bar portrait entirely) is
//! packed as extra entries under the SAME base hero key. Consumers must therefore
//! collapse variants when ranking — use `best_hero`, not `ncc::best`, so the
//! confidence margin is between two different HEROES, never two variants of one.
//!
//! Blob format: identical to `heroes_color_refs.bin` (see `color_refs::parse_blob`),
//! just with w=48, h=27 and duplicate keys allowed.

use std::sync::OnceLock;

use image::{DynamicImage, RgbImage};

use crate::vision::color_refs;

const BLOB: &[u8] = include_bytes!("topbar_color_refs.bin");

/// Canonical top-bar match size. Matches the aspect of a portrait cell's hero-art
/// band (the 60x40 cell minus the player-color strip and shadow seam -> 60x34,
/// aspect 1.76): both the baked refs and live query crops are brought to exactly
/// this size before ZNCC.
pub const TB_W: u32 = 48;
pub const TB_H: u32 = 27;

/// The bundled top-bar reference library, parsed once. Entries are (base hero key,
/// 48x27 RGB); several entries may share a key (art variants of the same hero).
pub fn load() -> &'static [(String, RgbImage)] {
    static REFS: OnceLock<Vec<(String, RgbImage)>> = OnceLock::new();
    REFS.get_or_init(|| color_refs::parse_blob(BLOB))
}

/// Bring an on-screen portrait art-band crop to the canonical match size (RGB).
pub fn prep_query(img: &DynamicImage) -> RgbImage {
    img.resize_exact(TB_W, TB_H, image::imageops::FilterType::Lanczos3)
        .to_rgb8()
}

/// (best_key, top1 score, INTER-HERO top1-top2 margin) from a descending `ncc::rank`
/// result over this library. Variants of one hero collapse to the hero's best score,
/// and the margin is taken to the best score of a DIFFERENT hero — two variants of
/// the same hero ranking close together must not read as low confidence.
pub fn best_hero(ranked: &[(String, f32)]) -> (String, f32, f32) {
    let (k0, s0) = ranked.first().cloned().unwrap_or_default();
    let s1 = ranked.iter().find(|(k, _)| *k != k0).map(|(_, s)| *s).unwrap_or(0.0);
    (k0, s0, s0 - s1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn bundled_topbar_blob_loads_all_heroes_at_tb_size() {
        let refs = load();
        let distinct: HashSet<&str> = refs.iter().map(|(k, _)| k.as_str()).collect();
        assert!(
            distinct.len() >= 120,
            "expected ~126 distinct hero keys (variants collapse), got {}",
            distinct.len()
        );
        assert!(refs.len() >= distinct.len(), "variant entries must not shrink the library");
        assert!(refs.iter().all(|(k, _)| k.starts_with("npc_dota_hero_")));
        assert!(refs.iter().all(|(_, img)| img.width() == TB_W && img.height() == TB_H));
    }

    #[test]
    fn variants_share_the_base_key() {
        // Juggernaut ships base + arcana art; all must sit under the plain base key.
        let refs = load();
        let jugg = refs.iter().filter(|(k, _)| k == "npc_dota_hero_juggernaut").count();
        assert!(jugg >= 2, "expected juggernaut base + alt variants, got {jugg}");
        assert!(!refs.iter().any(|(k, _)| k.contains("_alt")));
    }

    #[test]
    fn blob_covers_dawnbreaker_and_folds_persona_carnival_variants() {
        let refs = load();
        // dawnbreaker is missing from the bundled phash table; the baker pins her
        // key explicitly — the blob must carry her art.
        assert!(refs.iter().any(|(k, _)| k == "npc_dota_hero_dawnbreaker"));
        // Persona/carnival art must sit under the base hero key, never its own.
        assert!(!refs.iter().any(|(k, _)| k.contains("_persona") || k.contains("_carnival")));
        // antimage ships base + persona1 art -> at least two entries under one key.
        let am = refs.iter().filter(|(k, _)| k == "npc_dota_hero_antimage").count();
        assert!(am >= 2, "expected antimage base + persona variant, got {am}");
    }

    #[test]
    fn best_hero_collapses_variants_to_an_inter_hero_margin() {
        // Two variants of hero A on top: the margin must be measured to hero B
        // (0.9 - 0.5), not to A's second variant (0.9 - 0.8).
        let ranked = vec![
            ("npc_dota_hero_a".to_string(), 0.9),
            ("npc_dota_hero_a".to_string(), 0.8),
            ("npc_dota_hero_b".to_string(), 0.5),
        ];
        let (k, s, m) = best_hero(&ranked);
        assert_eq!(k, "npc_dota_hero_a");
        assert!((s - 0.9).abs() < 1e-6);
        assert!((m - 0.4).abs() < 1e-6);
    }

    #[test]
    fn best_hero_handles_edge_cases() {
        assert_eq!(best_hero(&[]).0, "");
        // Single hero, no competitor: margin falls back to score - 0.
        let only = vec![("npc_dota_hero_a".to_string(), 0.7)];
        let (_, s, m) = best_hero(&only);
        assert_eq!(s, m);
    }
}

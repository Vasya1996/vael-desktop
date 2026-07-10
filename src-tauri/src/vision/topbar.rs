//! Top-bar composition scan pipeline: one captured frame -> hero+team observations
//! for the 10 top-bar portrait cells flanking the clock panel, or None.
//!
//! Safety model (mirrors `pipeline.rs`'s scoreboard model, adapted to the top-bar's
//! anchor-relative geometry):
//!  1. Anchor lock — `locate::verify`/`locate::locate` must find the clock-panel
//!     anchor; no anchor means no geometry to trust, so the whole frame is discarded.
//!  2. GSI own-hero oracle — the player's OWN hero (from GSI) must be recognized,
//!     ALIVE, on its own side (`own_team` fixes left=Radiant/right=Dire, per the Dota
//!     HUD). "Recognized" here is a looser bar than identifying an unknown hero (see
//!     `OWN_PEAK_MIN`): GSI already told us the hero, so we only need to VERIFY it's
//!     the top1 read. If no own-side slot clears this, the capture is discarded —
//!     same discard-the-whole-frame policy as the scoreboard pipeline.
//!  3. Dead-cell skip — the HUD desaturates a dead hero's top-bar portrait to
//!     near-grayscale, making color-ZNCC meaningless for that slot. Every slot (own
//!     and non-own) is checked for live saturation before ranking; a dead/unverifiable
//!     slot never guesses a hero — it reads `unconfirmed` with an empty hero and 0
//!     confidence, and can never satisfy the own-hero oracle either.
//!  4. Margin gate (`recognize::classify`) — every non-own slot still needs a clear
//!     top1 AND top1-top2 margin (inter-hero, via `topbar_refs::best_hero`) to become
//!     `known`; anything weaker is `unconfirmed` (the server drops those).
//!  5. Duplicate guard — `pipeline::dedupe_known` collapses any two slots that
//!     resolved to the same hero (evidence of a misread) down to at most one `known`.
//!
//! Only composition (hero + team) is read here; level/items are later stages.

use image::{DynamicImage, GenericImageView, RgbImage};

use crate::vision::recognize::{self, VisionObservation};
use crate::vision::{locate, ncc, pipeline, topbar_refs, vlog, zones};

// ---------------------------------------------------------------------------------
// Portrait geometry, relative to the anchor's top-left corner (from the validated
// prototype `desktop/tools/refbuild/src/bin/topbar_refs.rs::portrait_rects`, in turn
// measured on a real 1920x1080 frame; anchor x=856 y=0 w=209 h=40). All distances are
// multiplied by `loc.scale`; the row's y sits at `loc.y` directly — the portrait's
// offset from the panel center and the panel center's offset from the anchor's
// top-left cancel out exactly (both anchor and portraits sit at the same y=0 row).
// ---------------------------------------------------------------------------------
const ALLY_DX: f64 = -311.0;
const ALLY_PITCH: f64 = 62.2;
const ENEMY_DX: f64 = 210.0;
const ENEMY_PITCH: f64 = 62.0;
const CELL_W: f64 = 60.0;
const CELL_H: f64 = 40.0;

/// Top of the hero-art band inside a portrait cell, as a fraction of cell height
/// (player-color strip + dark shadow seam). Mirrors
/// `desktop/tools/refbuild/src/bin/topbar_refs.rs::CELL_ART_Y0_FRAC` exactly — this is
/// how the baked top-bar refs were calibrated (queries = art band only, strip-free).
const CELL_ART_Y0_FRAC: f64 = 0.15;

/// Dead-hero cells desaturate to near-grayscale, making color-ZNCC meaningless. Mean
/// per-pixel saturation (max channel - min channel, 0..255) of the art-band query is
/// cleanly bimodal on real frames (dev bin `topbar_refs.rs` calibration set: dead
/// <= 19.2), but legit LIVE art goes as low as 21.7/21.8 (sven / witch_doctor baked
/// refs), so the floor sits between the two: below it a slot is a dead hero this
/// frame and is emitted as `unconfirmed` with an empty hero rather than guessed.
pub const MIN_LIVE_SAT: f32 = 20.5;

/// GSI own-hero oracle threshold — deliberately looser than `recognize::PEAK_MIN`
/// (0.45). The oracle only VERIFIES an identity GSI already told us (top1 must equal
/// the known own hero), which needs far less evidence than IDENTIFYING an unknown
/// hero from scratch: measured floor for arcana-art own cells is score 0.328 (jugg
/// arcana on real frames), and the anchor's own score+margin gates already prove the
/// geometry is right, so a merely-adequate color match is enough to confirm the
/// already-known identity — 0.25 keeps a real safety gap under that floor.
pub const OWN_PEAK_MIN: f32 = 0.25;

/// 10 portrait rects from a located anchor: [0..5] = left group (Radiant),
/// [5..10] = right (Dire). Pure proportion math off the anchor's top-left corner, no
/// further search. X is clamped to 0 if it would go negative (defensive only — a
/// clamped rect still passes through `scan_topbar`'s in-frame bounds check, which is
/// what actually gates a slot as unconfirmed).
pub fn portrait_rects(loc: &locate::Located) -> [zones::Rect; 10] {
    let mut out = [zones::Rect { x: 0, y: 0, w: 0, h: 0 }; 10];
    let w = (CELL_W * loc.scale).round().max(1.0) as u32;
    let h = (CELL_H * loc.scale).round().max(1.0) as u32;
    for i in 0..5u32 {
        let x = loc.x as f64 + (ALLY_DX + i as f64 * ALLY_PITCH) * loc.scale;
        out[i as usize] = zones::Rect { x: x.round().max(0.0) as u32, y: loc.y, w, h };
    }
    for i in 0..5u32 {
        let x = loc.x as f64 + (ENEMY_DX + i as f64 * ENEMY_PITCH) * loc.scale;
        out[5 + i as usize] = zones::Rect { x: x.round().max(0.0) as u32, y: loc.y, w, h };
    }
    out
}

/// Mean per-pixel saturation (max channel - min channel) of an art-band query.
/// Mirrors `desktop/tools/refbuild/src/bin/topbar_refs.rs::art_band_saturation`
/// exactly (same input: the query already resized to the canonical TB_W x TB_H).
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

/// Crop a cell's hero-art band (skips the top `CELL_ART_Y0_FRAC` player-color strip +
/// seam) — None if the cell falls outside the frame.
fn art_band_crop(frame: &DynamicImage, cell: zones::Rect) -> Option<DynamicImage> {
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

/// One slot's read: best hero guess + inter-hero margin (from `topbar_refs::best_hero`)
/// if the slot is a live, in-frame cell; `alive=false` (empty hero, zero score/margin)
/// if the cell is out of frame OR its art band failed the saturation gate — either way
/// it is unverifiable this frame, and no hero guess is carried.
fn read_slot(frame: &DynamicImage, cell: zones::Rect, refs: &[(String, RgbImage)]) -> (String, f32, f32, bool) {
    let Some(crop) = art_band_crop(frame, cell) else {
        return (String::new(), 0.0, 0.0, false);
    };
    let q = topbar_refs::prep_query(&crop);
    if art_band_saturation(&q) < MIN_LIVE_SAT {
        return (String::new(), 0.0, 0.0, false);
    }
    let (hero, top1, margin) = topbar_refs::best_hero(&ncc::rank(&q, refs));
    (hero, top1, margin, true)
}

/// Diagnostic re-derivation of one slot's saturation + ZNCC read for `vision.log`
/// (see `vlog`). Recomputes `art_band_crop`/`art_band_saturation`/ZNCC independently of
/// `read_slot` so logging never touches the real recognition path — it is only ever
/// called behind `vlog::gate()`, i.e. throttled to a couple of full dumps a second at
/// most. Dead cells still get a ZNCC top1/score/margin here (unlike `read_slot`, which
/// skips ZNCC once the saturation gate fails) purely for diagnosing false "dead" reads
/// (e.g. a legitimately low-saturation live hero art sitting just under `MIN_LIVE_SAT`).
fn log_slot_line(frame: &DynamicImage, cell: zones::Rect, refs: &[(String, RgbImage)], r: usize, team: &str, status: &str) -> String {
    match art_band_crop(frame, cell) {
        Some(crop) => {
            let q = topbar_refs::prep_query(&crop);
            let sat = art_band_saturation(&q);
            let alive = sat >= MIN_LIVE_SAT;
            let (hero, top1, margin) = topbar_refs::best_hero(&ncc::rank(&q, refs));
            format!(
                "slot={r} team={team} sat={sat:.1} {} top1={hero} score={top1:.3} margin={margin:.3} -> {status}",
                if alive { "alive" } else { "dead" }
            )
        }
        None => format!("slot={r} team={team} oob -> {status}"),
    }
}

/// One frame -> composition observations (hero+team only, no level/items), or None
/// (anchor not found / own hero not confirmed alive on its own side -> discard,
/// GSI-oracle style). `cached` lets the caller reuse the previous `Located` via
/// `locate::verify` (cheap steady state); on a miss (or first call) a fresh
/// `locate::locate` re-acquires the anchor.
///
/// Every call also writes (throttled, see `vlog::gate`) a diagnostic dump to
/// `vision.log`: the anchor result, the own-hero oracle result, and one line per slot
/// (saturation, alive/dead, ZNCC top1/score/margin, final status) — so a match where a
/// hero never got recognized can be reconstructed after the fact. Logging never changes
/// the returned observations or gating: it only reads already-computed values (or, for
/// the per-slot ZNCC on dead cells, an independent read-only recompute — see
/// `log_slot_line`).
pub fn scan_topbar(
    frame: &DynamicImage,
    own_hero: &str,
    own_team: &str,
    refs: &[(String, RgbImage)],
    cached: Option<&locate::Located>,
) -> Option<(Vec<VisionObservation>, locate::Located)> {
    if refs.is_empty() || own_hero.is_empty() {
        return None;
    }
    let (fw, fh) = frame.dimensions();
    let should_log = vlog::gate();
    let anchor = locate::topbar_anchor();
    let loc = cached
        .and_then(|c| locate::verify(frame, anchor, c))
        .or_else(|| locate::locate(frame, anchor));
    if should_log {
        vlog::write(&match &loc {
            Some(l) => format!(
                "scan {fw}x{fh} anchor ok x={} y={} scale={:.3} score={:.3} margin={:.3}",
                l.x, l.y, l.scale, l.score, l.margin
            ),
            None => format!("scan {fw}x{fh} anchor fail"),
        });
    }
    let loc = loc?;

    let rects = portrait_rects(&loc);
    let reads: Vec<(String, f32, f32, bool)> = rects.iter().map(|&cell| read_slot(frame, cell, refs)).collect();

    let radiant = own_team.eq_ignore_ascii_case("radiant");
    // Own hero must be alive, top1, and clear OWN_PEAK_MIN in >=1 own-side slot; a
    // dead/unverifiable own-side slot can never satisfy this (reads[r].3 is false).
    let mut own_range = if radiant { 0..5 } else { 5..10 };
    let own_row =
        own_range.find(|&r| reads[r].3 && reads[r].0 == own_hero && reads[r].1 >= OWN_PEAK_MIN);

    if should_log {
        vlog::write(&format!(
            "own_hero={own_hero} own_team={own_team} oracle={} row={}",
            if own_row.is_some() { "ok" } else { "fail" },
            own_row.map(|r| r.to_string()).unwrap_or_else(|| "-".into())
        ));
        for r in 0..10 {
            let team = if r < 5 { "radiant" } else { "dire" };
            let status = if Some(r) == own_row {
                "own".to_string()
            } else {
                let (_, top1, margin, alive) = &reads[r];
                if *alive { recognize::classify(*top1, *margin).0.to_string() } else { "unconfirmed".to_string() }
            };
            vlog::write(&log_slot_line(frame, rects[r], refs, r, team, &status));
        }
    }

    let own_row = own_row?;

    let mut obs = Vec::with_capacity(9);
    for r in 0..10 {
        if r == own_row {
            continue;
        }
        let team = if r < 5 { "radiant" } else { "dire" };
        let (hero, top1, margin, alive) = &reads[r];
        let (status, confidence) = if *alive {
            recognize::classify(*top1, *margin)
        } else {
            ("unconfirmed", 0.0)
        };
        obs.push(VisionObservation {
            hero: if *alive { hero.clone() } else { String::new() },
            team: Some(team.to_string()),
            level: None,
            items: None,
            confidence,
            status: status.to_string(),
        });
    }
    pipeline::dedupe_known(&mut obs);
    Some((obs, loc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::imageops::FilterType;
    use image::Rgb;

    /// Build a synthetic top-bar frame: dark background, the BAKED anchor template
    /// planted at `scale` (glued to y=0, slightly off-center in x, like `locate.rs`'s
    /// own synthetic-frame test), then each hero's BAKED topbar ref painted into its
    /// portrait cell's art band (bottom 85%), with a bright saturated strip painted
    /// into the top 15% so the dead-cell check reads the slot as live.
    ///
    /// Portrait geometry for painting is derived by actually calling `locate::locate`
    /// on the anchor-only frame, NOT from the nominal `scale` directly: `locate`'s
    /// scale grid is quantized (`SCALE_STEP_FINE` = 0.02), so a nominal scale like
    /// 0.667 is recovered as e.g. 0.66. That few-thousandths difference, multiplied
    /// out over the row pitch, shifts far cells by several px — harmless on a real
    /// capture (continuous game-rendered art tolerates it) but punishing on this
    /// fixture's double-resized synthetic art. Painting at the SAME quantized
    /// geometry `scan_topbar` will itself recover (portraits sit outside the anchor's
    /// own template/search area, so painting them doesn't change what a second
    /// `locate` call on the full frame finds) removes that fixture-only mismatch.
    ///
    /// Returns the frame AND the `Located` the paint geometry was derived from, so
    /// tests that post-process cells (e.g. `paint_dead`) reuse the exact same rects
    /// instead of re-deriving the plant position by hand.
    fn synthetic_topbar_frame(
        radiant: &[&str],
        dire: &[&str],
        fw: u32,
        fh: u32,
        scale: f64,
    ) -> (DynamicImage, locate::Located) {
        let a = locate::topbar_anchor();
        let mut frame = RgbImage::from_pixel(fw, fh, Rgb([10, 10, 12]));

        let (tw, th) = (((a.w as f64) * scale).round() as u32, ((a.h as f64) * scale).round() as u32);
        let tmpl = DynamicImage::ImageRgb8(a.tmpl.clone())
            .resize_exact(tw, th, FilterType::Lanczos3)
            .to_rgb8();
        let px = fw / 2 - tw / 2 + 3;
        image::imageops::overlay(&mut frame, &tmpl, px as i64, 0);

        let loc = locate::locate(&DynamicImage::ImageRgb8(frame.clone()), a)
            .unwrap_or_else(|| panic!("locate() must find the freshly planted anchor"));
        let rects = portrait_rects(&loc);
        let refs = topbar_refs::load();

        let paint = |frame: &mut RgbImage, cell: zones::Rect, key: &str| {
            let Some((_, tmpl)) = refs.iter().find(|(k, _)| k == key) else { return };
            let art_y0 = ((cell.h as f64) * CELL_ART_Y0_FRAC).round() as u32;
            for y in 0..art_y0 {
                for x in 0..cell.w {
                    frame.put_pixel(cell.x + x, cell.y + y, Rgb([60, 120, 220]));
                }
            }
            let art_h = cell.h - art_y0;
            // Lanczos3 (not Nearest) to preserve fidelity through the paint/read-back
            // round trip when the cell is smaller than the 48x27 source (e.g. 720p).
            let scaled = image::imageops::resize(tmpl, cell.w, art_h, FilterType::Lanczos3);
            image::imageops::overlay(frame, &scaled, cell.x as i64, (cell.y + art_y0) as i64);
        };
        for (i, k) in radiant.iter().enumerate() {
            paint(&mut frame, rects[i], k);
        }
        for (i, k) in dire.iter().enumerate() {
            paint(&mut frame, rects[5 + i], k);
        }
        (DynamicImage::ImageRgb8(frame), loc)
    }

    /// Paint a grayscale (desaturated) version of a hero's ref into a cell's art band
    /// — simulates a dead hero. The player-color strip is still painted (deadness is
    /// only measured on the art band).
    fn paint_dead(frame: &mut DynamicImage, loc: &locate::Located, slot: usize, key: &str) {
        let rects = portrait_rects(loc);
        let cell = rects[slot];
        let refs = topbar_refs::load();
        let Some((_, tmpl)) = refs.iter().find(|(k, _)| k == key) else { return };
        let mut rgb = frame.to_rgb8();
        let art_y0 = ((cell.h as f64) * CELL_ART_Y0_FRAC).round() as u32;
        for y in 0..art_y0 {
            for x in 0..cell.w {
                rgb.put_pixel(cell.x + x, cell.y + y, Rgb([60, 120, 220]));
            }
        }
        let art_h = cell.h - art_y0;
        let scaled = image::imageops::resize(tmpl, cell.w, art_h, FilterType::Lanczos3);
        let gray = image::imageops::grayscale(&scaled);
        let gray_rgb: RgbImage = image::ImageBuffer::from_fn(gray.width(), gray.height(), |x, y| {
            let l = gray.get_pixel(x, y).0[0];
            Rgb([l, l, l])
        });
        image::imageops::overlay(&mut rgb, &gray_rgb, cell.x as i64, (cell.y + art_y0) as i64);
        *frame = DynamicImage::ImageRgb8(rgb);
    }

    // Note: sven's top-bar art measures sat~21.8 (just above MIN_LIVE_SAT=20.5) —
    // real Dota art has legitimately low-saturation portraits close to the dead-cell
    // threshold, so axe (sat~31.5) is used here instead to keep this a clean
    // "definitely alive" fixture.
    const RADIANT: [&str; 5] = [
        "npc_dota_hero_juggernaut",
        "npc_dota_hero_axe",
        "npc_dota_hero_kunkka",
        "npc_dota_hero_lion",
        "npc_dota_hero_necrolyte",
    ];
    const DIRE: [&str; 5] = [
        "npc_dota_hero_drow_ranger",
        "npc_dota_hero_razor",
        "npc_dota_hero_windrunner",
        "npc_dota_hero_warlock",
        "npc_dota_hero_dragon_knight",
    ];

    #[test]
    fn geometry_matches_the_measured_frame() {
        // Absolute pixel regression (zones.rs::geometry_matches_the_measured_frame
        // style): the composition tests paint AND read through the same
        // portrait_rects(), so a systematic corruption of the geometry constants
        // (sign flip, wrong magnitude, ally/enemy swap) would cancel out and still
        // pass them. Pin the rects against the measured 1920x1080 reference frame
        // (anchor at x=856, y=0, scale 1.0 — see the geometry constants' comment).
        let loc = locate::Located { x: 856, y: 0, scale: 1.0, score: 1.0, margin: 1.0 };
        let r = portrait_rects(&loc);
        // Ally (Radiant, left) slot 0 measured at x=545; enemy (Dire, right) slot 0
        // at x=1066 — asymmetric offsets, so an ally/enemy swap cannot pass.
        assert!((r[0].x as i64 - 545).abs() <= 1, "ally slot 0 x {}", r[0].x);
        assert!((r[5].x as i64 - 1066).abs() <= 1, "enemy slot 0 x {}", r[5].x);
        // Neighboring slots within each group are pitched ~62px apart.
        for i in 0..4 {
            let ally_step = r[i + 1].x as i64 - r[i].x as i64;
            assert!((ally_step - 62).abs() <= 1, "ally step {i} = {ally_step}");
            let enemy_step = r[5 + i + 1].x as i64 - r[5 + i].x as i64;
            assert!((enemy_step - 62).abs() <= 1, "enemy step {i} = {enemy_step}");
        }
        // Every cell is 60x40 at scale 1.0 and glued to the anchor's row (y=0).
        for (i, c) in r.iter().enumerate() {
            assert_eq!((c.w, c.h), (60, 40), "cell {i} size");
            assert_eq!(c.y, 0, "cell {i} y");
        }
    }

    #[test]
    fn reads_composition_and_gates_on_own_hero() {
        let refs = topbar_refs::load();
        if refs.len() < 120 {
            return; // refs not bundled in this build
        }
        let (frame, _) = synthetic_topbar_frame(&RADIANT, &DIRE, 1920, 1080, 1.0);
        let (obs, loc) = scan_topbar(&frame, "npc_dota_hero_juggernaut", "radiant", refs, None)
            .expect("own hero confirmed -> Some");
        assert!((loc.scale - 1.0).abs() < 0.05);
        assert_eq!(obs.len(), 9, "9 non-own slots emitted");
        assert!(obs.iter().all(|o| o.hero != "npc_dota_hero_juggernaut"), "own hero excluded");
        for k in &RADIANT[1..] {
            let o = obs.iter().find(|o| &o.hero == k).unwrap_or_else(|| panic!("missing ally {k}"));
            assert_eq!(o.team.as_deref(), Some("radiant"));
            assert_eq!(o.status, "known", "{k} should be known");
        }
        for k in DIRE {
            let o = obs.iter().find(|o| o.hero == k).unwrap_or_else(|| panic!("missing enemy {k}"));
            assert_eq!(o.team.as_deref(), Some("dire"));
            assert_eq!(o.status, "known", "{k} should be known");
        }
    }

    #[test]
    fn works_at_720p_scale() {
        let refs = topbar_refs::load();
        if refs.len() < 120 {
            return;
        }
        let (frame, _) = synthetic_topbar_frame(&RADIANT, &DIRE, 1280, 720, 0.667);
        let (obs, loc) = scan_topbar(&frame, "npc_dota_hero_juggernaut", "radiant", refs, None)
            .expect("own hero confirmed -> Some at 720p");
        assert!((loc.scale - 0.667).abs() < 0.05);
        assert_eq!(obs.len(), 9);
        for k in &RADIANT[1..] {
            let o = obs.iter().find(|o| &o.hero == k).unwrap();
            assert_eq!(o.status, "known");
        }
        for k in DIRE {
            let o = obs.iter().find(|o| o.hero == k).unwrap();
            assert_eq!(o.status, "known");
        }
    }

    #[test]
    fn discards_when_own_hero_not_on_own_side() {
        let refs = topbar_refs::load();
        if refs.len() < 120 {
            return;
        }
        // Juggernaut painted only on the Dire side; own_team says radiant.
        let radiant = ["npc_dota_hero_sven", "npc_dota_hero_kunkka", "npc_dota_hero_lion", "npc_dota_hero_necrolyte", "npc_dota_hero_axe"];
        let dire = [
            "npc_dota_hero_juggernaut",
            "npc_dota_hero_drow_ranger",
            "npc_dota_hero_razor",
            "npc_dota_hero_windrunner",
            "npc_dota_hero_warlock",
        ];
        let (frame, _) = synthetic_topbar_frame(&radiant, &dire, 1920, 1080, 1.0);
        assert!(scan_topbar(&frame, "npc_dota_hero_juggernaut", "radiant", refs, None).is_none());
    }

    #[test]
    fn no_anchor_means_none() {
        let refs = topbar_refs::load();
        if refs.len() < 120 {
            return;
        }
        let frame = RgbImage::from_fn(1920, 1080, |x, y| Rgb([(x * 3 % 256) as u8, (y * 5 % 256) as u8, 128]));
        assert!(scan_topbar(&DynamicImage::ImageRgb8(frame), "npc_dota_hero_juggernaut", "radiant", refs, None).is_none());
    }

    #[test]
    fn dead_cell_reads_unconfirmed_with_empty_hero() {
        let refs = topbar_refs::load();
        if refs.len() < 120 {
            return;
        }
        let (mut frame, loc) = synthetic_topbar_frame(&RADIANT, &DIRE, 1920, 1080, 1.0);
        // Overwrite Dire slot 1 (razor, DIRE[1]) with a grayscale (dead) render, at
        // the exact same geometry the fixture painted it live.
        paint_dead(&mut frame, &loc, 5 + 1, "npc_dota_hero_razor");

        let (obs, _) = scan_topbar(&frame, "npc_dota_hero_juggernaut", "radiant", refs, None)
            .expect("own hero still confirmed -> Some");
        assert_eq!(obs.len(), 9);
        let dead: Vec<_> = obs.iter().filter(|o| o.hero.is_empty()).collect();
        assert_eq!(dead.len(), 1, "exactly one dead slot read as empty-hero");
        assert_eq!(dead[0].status, "unconfirmed");
        assert_eq!(dead[0].confidence, 0.0);
        assert_eq!(dead[0].team.as_deref(), Some("dire"));
        // Razor must not appear elsewhere as a known read either.
        assert!(!obs.iter().any(|o| o.hero == "npc_dota_hero_razor"));
    }

    #[test]
    fn discards_when_own_side_all_dead() {
        let refs = topbar_refs::load();
        if refs.len() < 120 {
            return;
        }
        let (mut frame, loc) = synthetic_topbar_frame(&RADIANT, &DIRE, 1920, 1080, 1.0);
        // Own hero (juggernaut, radiant slot 0) rendered dead -> unverifiable.
        paint_dead(&mut frame, &loc, 0, "npc_dota_hero_juggernaut");

        assert!(scan_topbar(&frame, "npc_dota_hero_juggernaut", "radiant", refs, None).is_none());
    }
}

//! Scoreboard pipeline: one captured frame → validated ENEMY observations.
//!
//! Safety model (why a wrong enemy hero is never shipped):
//!  1. GSI own-row oracle — the player's OWN hero (from GSI) must be recognized in its
//!     own team's group. The offset that locks it is reused for every other row, so the
//!     whole board shares one proven alignment. If the own hero isn't found, the
//!     scoreboard isn't open or the geometry is off → the WHOLE capture is discarded.
//!  2. Margin gate (`recognize::classify`) — only a clear top1 AND top1−top2 margin
//!     becomes `known`; everything else is `unconfirmed` (which the server drops).
//!  3. Duplicate guard — a hero is unique per match, so if two enemy rows resolve to the
//!     same hero (evidence of a misread) only the best keeps `known`; the rest are
//!     downgraded to `unconfirmed`.
//!  4. Both teams' rows are emitted (each tagged with its fixed group's `team`), with the
//!     player's OWN row excluded (it's the GSI anchor — the server already knows it). The
//!     same conservative gate applies to ally and enemy rows alike, so the zero-false
//!     guarantee holds for every emitted fact regardless of side.

use image::{DynamicImage, GenericImageView, RgbImage};

use crate::vision::recognize::{self, VisionObservation};
use crate::vision::{ncc, zones};

// Re-lock search window (px) around each predicted cell — absorbs minor geometry /
// HUD-scale error. Small by design: the baked geometry is measured per resolution and
// the own-row lock is reused across the board, so we only correct a few px.
const RELOCK_R: i32 = 6;
const RELOCK_STEP: i32 = 3;

/// Crop a cell shifted by (dx, dy); None if it would fall outside the frame.
fn crop_at(frame: &DynamicImage, cell: zones::Rect, dx: i32, dy: i32) -> Option<DynamicImage> {
    let (fw, fh) = frame.dimensions();
    let x = cell.x as i32 + dx;
    let y = cell.y as i32 + dy;
    if x < 0 || y < 0 || (x as u32 + cell.w) > fw || (y as u32 + cell.h) > fh {
        return None;
    }
    Some(frame.crop_imm(x as u32, y as u32, cell.w, cell.h))
}

/// Search a window of `radius` px (in `RELOCK_STEP` increments) centered on the (cx, cy)
/// offset and return the offset that maximizes the top-1 score, with that read's
/// (hero, top1, margin, dx, dy).
fn search(
    frame: &DynamicImage,
    cell: zones::Rect,
    cx: i32,
    cy: i32,
    radius: i32,
    refs: &[(String, RgbImage)],
) -> (String, f32, f32, i32, i32) {
    let mut best = (String::new(), -2f32, 0f32, cx, cy);
    let mut dy = cy - radius;
    while dy <= cy + radius {
        let mut dx = cx - radius;
        while dx <= cx + radius {
            if let Some(crop) = crop_at(frame, cell, dx, dy) {
                let (hero, top1, margin) = ncc::best(&ncc::rank(&ncc::prep_query(&crop), refs));
                if top1 > best.1 {
                    best = (hero, top1, margin, dx, dy);
                }
            }
            dx += RELOCK_STEP;
        }
        dy += RELOCK_STEP;
    }
    best
}

/// Own-row oracle: search the own group for the GSI own hero. On success return the
/// row it locked AND the offset that locked it (the board's HUD alignment); None means
/// "not found → discard". The row is the player's own scoreboard slot, excluded from the
/// emitted ally facts (it's the anchor; GSI already knows the own hero).
fn own_lock_offset(
    frame: &DynamicImage,
    rects: &[zones::Rect],
    rows: std::ops::Range<usize>,
    own_hero: &str,
    refs: &[(String, RgbImage)],
) -> Option<(usize, i32, i32)> {
    for r in rows {
        let (hero, top1, _m, dx, dy) = search(frame, rects[r], 0, 0, RELOCK_R, refs);
        if hero == own_hero && top1 >= recognize::PEAK_MIN {
            return Some((r, dx, dy));
        }
    }
    None
}

/// A hero is unique per match: if two enemy rows resolve to the SAME hero, at least one
/// is a misread, so keep `known` only on the highest-confidence row and downgrade the
/// rest to `unconfirmed`. Never let a duplicate become a fact.
fn dedupe_known(obs: &mut [VisionObservation]) {
    use std::collections::HashMap;
    let mut best: HashMap<String, usize> = HashMap::new();
    for i in 0..obs.len() {
        if obs[i].status != "known" {
            continue;
        }
        let keep = match best.get(&obs[i].hero) {
            Some(&bi) => obs[i].confidence > obs[bi].confidence,
            None => true,
        };
        if keep {
            best.insert(obs[i].hero.clone(), i);
        }
    }
    for i in 0..obs.len() {
        if obs[i].status == "known" && best.get(&obs[i].hero) != Some(&i) {
            obs[i].status = "unconfirmed".to_string();
            // The hero on a downgraded row is unproven, so anything read at that row's
            // geometry (its level) is unproven too — drop it so a foreign-row level can
            // never ride a deduped hero.
            obs[i].level = None;
        }
    }
}

/// Scan the scoreboard for both teams' heroes. Returns observations for all 9 non-own
/// rows — enemy AND ally, each tagged with its fixed group's `team` and gated to
/// `known`/`unconfirmed` — or None when the own-row oracle fails (discard the capture).
/// The player's own row is excluded (it's the GSI anchor). `own_hero` is the GSI hero key
/// (e.g. "npc_dota_hero_pudge"); `own_team` is "radiant" or "dire" from GSI
/// `player.team_name`.
pub fn scan_scoreboard(
    frame: &DynamicImage,
    own_hero: &str,
    own_team: &str,
    refs: &[(String, RgbImage)],
) -> Option<Vec<VisionObservation>> {
    if refs.is_empty() || own_hero.is_empty() {
        return None;
    }
    let (fw, fh) = frame.dimensions();
    let rects = zones::portrait_rects(fw, fh);
    let radiant = own_team.eq_ignore_ascii_case("radiant");
    let own_range = if radiant { 0..5 } else { 5..10 };

    // Lock the board on the GSI own hero; every other row searches a tiny window around
    // that proven alignment (cheap, and recovers each row's own sub-pixel margin).
    let (own_row, lx, ly) = own_lock_offset(frame, &rects, own_range, own_hero, refs)?;

    // Emit all 9 non-own rows (both teams). The own row is skipped: it's the anchor and
    // GSI already knows the own hero, so it never becomes a screen-read fact.
    let mut obs = Vec::with_capacity(zones::TOTAL_ROWS - 1);
    for r in 0..zones::TOTAL_ROWS {
        if r == own_row {
            continue;
        }
        let (hero, top1, margin, _, _) = search(frame, rects[r], lx, ly, RELOCK_STEP, refs);
        let (status, confidence) = recognize::classify(top1, margin);
        // Read this row's level from the "УР." box, anchored to the locked portrait. The
        // reader returns None unless it is confident (the temporal/monotonic guards in the
        // capture loop add the final safety layer). Only carry a level for a `known` hero.
        let level = if status == "known" {
            read_level_for_row(frame, rects[r], lx, ly, fw, fh)
        } else {
            None
        };
        obs.push(VisionObservation {
            hero,
            // Team comes from the row's fixed group (Radiant top / Dire bottom), so ally
            // and enemy rows are tagged with their correct side.
            team: Some(zones::team_of_row(r).to_string()),
            level,
            items: None,
            confidence,
            status: status.to_string(),
        });
    }
    dedupe_known(&mut obs);
    Some(obs)
}

/// Crop the level box for a row (portrait shifted by the board lock) and read its digits.
fn read_level_for_row(
    frame: &DynamicImage,
    portrait: zones::Rect,
    lx: i32,
    ly: i32,
    fw: u32,
    fh: u32,
) -> Option<u32> {
    let shifted = zones::Rect {
        x: (portrait.x as i32 + lx).max(0) as u32,
        y: (portrait.y as i32 + ly).max(0) as u32,
        w: portrait.w,
        h: portrait.h,
    };
    let lvl = zones::level_rect(shifted, fw, fh);
    if lvl.x + lvl.w > fw || lvl.y + lvl.h > fh {
        return None;
    }
    let crop = frame.crop_imm(lvl.x, lvl.y, lvl.w, lvl.h).to_rgba8();
    crate::vision::level::read_level(&crop)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vision::color_refs;
    use image::RgbaImage;

    // Build a synthetic 1918x1078 "scoreboard" frame by painting each hero ref into its
    // predicted portrait cell.
    fn synthetic_frame(radiant: &[&str], dire: &[&str]) -> DynamicImage {
        let refs = color_refs::load();
        let get = |key: &str| refs.iter().find(|(k, _)| k == key).map(|(_, i)| i.clone());
        let (fw, fh) = (1918u32, 1078u32);
        let mut frame = RgbaImage::from_pixel(fw, fh, image::Rgba([12, 14, 19, 255]));
        let rects = zones::portrait_rects(fw, fh);
        let mut put = |row: usize, key: &str| {
            if let Some(tmpl) = get(key) {
                let cell = rects[row];
                let scaled = image::imageops::resize(
                    &tmpl,
                    cell.w,
                    cell.h,
                    image::imageops::FilterType::Nearest,
                );
                for (dx, dy, p) in scaled.enumerate_pixels() {
                    frame.put_pixel(cell.x + dx, cell.y + dy, image::Rgba([p.0[0], p.0[1], p.0[2], 255]));
                }
            }
        };
        for (i, k) in radiant.iter().enumerate() {
            put(i, k);
        }
        for (i, k) in dire.iter().enumerate() {
            put(i + 5, k);
        }
        DynamicImage::ImageRgba8(frame)
    }

    #[test]
    fn reads_both_teams_excludes_own_and_gates_on_own_hero() {
        let refs = color_refs::load();
        if refs.len() < 120 {
            return; // refs not bundled in this build
        }
        let radiant = ["npc_dota_hero_pudge", "npc_dota_hero_sven", "npc_dota_hero_kunkka", "npc_dota_hero_lion", "npc_dota_hero_necrolyte"];
        let dire = ["npc_dota_hero_drow_ranger", "npc_dota_hero_razor", "npc_dota_hero_windrunner", "npc_dota_hero_warlock", "npc_dota_hero_dragon_knight"];
        let frame = synthetic_frame(&radiant, &dire);

        // Own = Pudge on the Radiant side. Allies are the other 4 Radiant rows; enemies are
        // the 5 Dire rows. The own hero (Pudge) must NOT appear in the output.
        let obs = scan_scoreboard(&frame, "npc_dota_hero_pudge", "radiant", refs)
            .expect("own hero confirmed → Some");

        // All 9 non-own rows are emitted (4 allies + 5 enemies).
        assert_eq!(obs.len(), 9, "both teams emitted, own row excluded");
        // Own hero is excluded entirely.
        assert!(obs.iter().all(|o| o.hero != "npc_dota_hero_pudge"), "own hero must not be emitted");

        // Allies: the 4 non-own Radiant heroes, each tagged radiant and read `known`.
        for k in &radiant[1..] {
            let o = obs.iter().find(|o| &o.hero == k).unwrap_or_else(|| panic!("missing ally {k}"));
            assert_eq!(o.team.as_deref(), Some("radiant"), "{k} should be tagged radiant (ally)");
            assert_eq!(o.status, "known", "ally {k} should be known");
        }
        // Enemies: the 5 Dire heroes, each tagged dire and read `known`.
        for k in dire {
            let o = obs.iter().find(|o| o.hero == k).unwrap_or_else(|| panic!("missing enemy {k}"));
            assert_eq!(o.team.as_deref(), Some("dire"), "{k} should be tagged dire (enemy)");
            assert_eq!(o.status, "known", "enemy {k} should be known");
        }

        // Every emitted row carries a team, and no row leaks to the wrong side.
        let allies: std::collections::HashSet<&str> = radiant[1..].iter().copied().collect();
        let enemies: std::collections::HashSet<&str> = dire.iter().copied().collect();
        for o in &obs {
            match o.team.as_deref() {
                Some("radiant") => assert!(allies.contains(o.hero.as_str()), "radiant row {} is not an ally", o.hero),
                Some("dire") => assert!(enemies.contains(o.hero.as_str()), "dire row {} is not an enemy", o.hero),
                other => panic!("row {} has unexpected team {other:?}", o.hero),
            }
        }
    }

    #[test]
    fn reads_both_teams_when_own_is_dire() {
        // Mirror case: own hero on the Dire side. Allies are the 4 non-own Dire rows;
        // enemies are the 5 Radiant rows. Confirms the own/ally/enemy split is not
        // hard-wired to Radiant.
        let refs = color_refs::load();
        if refs.len() < 120 {
            return;
        }
        let radiant = ["npc_dota_hero_drow_ranger", "npc_dota_hero_razor", "npc_dota_hero_windrunner", "npc_dota_hero_warlock", "npc_dota_hero_dragon_knight"];
        let dire = ["npc_dota_hero_pudge", "npc_dota_hero_sven", "npc_dota_hero_kunkka", "npc_dota_hero_lion", "npc_dota_hero_necrolyte"];
        let frame = synthetic_frame(&radiant, &dire);

        let obs = scan_scoreboard(&frame, "npc_dota_hero_pudge", "dire", refs)
            .expect("own hero confirmed → Some");
        assert_eq!(obs.len(), 9);
        assert!(obs.iter().all(|o| o.hero != "npc_dota_hero_pudge"));
        // Dire allies (non-own).
        for k in &dire[1..] {
            let o = obs.iter().find(|o| &o.hero == k).unwrap_or_else(|| panic!("missing ally {k}"));
            assert_eq!(o.team.as_deref(), Some("dire"), "{k} should be tagged dire (ally)");
        }
        // Radiant enemies.
        for k in radiant {
            let o = obs.iter().find(|o| o.hero == k).unwrap_or_else(|| panic!("missing enemy {k}"));
            assert_eq!(o.team.as_deref(), Some("radiant"), "{k} should be tagged radiant (enemy)");
        }
    }

    #[test]
    fn discards_when_own_hero_absent() {
        let refs = color_refs::load();
        if refs.len() < 120 {
            return;
        }
        let radiant = ["npc_dota_hero_sven", "npc_dota_hero_kunkka", "npc_dota_hero_lion", "npc_dota_hero_necrolyte", "npc_dota_hero_razor"];
        let dire = ["npc_dota_hero_drow_ranger", "npc_dota_hero_windrunner", "npc_dota_hero_warlock", "npc_dota_hero_dragon_knight", "npc_dota_hero_axe"];
        let frame = synthetic_frame(&radiant, &dire);
        assert!(scan_scoreboard(&frame, "npc_dota_hero_pudge", "radiant", refs).is_none());
    }

    #[test]
    fn duplicate_enemy_hero_keeps_at_most_one_known() {
        // If two enemy rows resolve to the same hero, dedupe_known must downgrade all but
        // one to unconfirmed — never two `known` of the same hero — AND drop the loser's
        // level (an unproven row must not contribute a level read at its geometry).
        let mut obs = vec![
            VisionObservation { hero: "npc_dota_hero_lina".into(), team: Some("dire".into()), level: Some(7), items: None, confidence: 0.4, status: "known".into() },
            VisionObservation { hero: "npc_dota_hero_lina".into(), team: Some("dire".into()), level: Some(22), items: None, confidence: 0.9, status: "known".into() },
            VisionObservation { hero: "npc_dota_hero_axe".into(), team: Some("dire".into()), level: Some(18), items: None, confidence: 0.8, status: "known".into() },
        ];
        dedupe_known(&mut obs);
        let lina_known = obs.iter().filter(|o| o.hero == "npc_dota_hero_lina" && o.status == "known").count();
        assert_eq!(lina_known, 1, "exactly one Lina stays known");
        // The kept one is the higher-confidence row and keeps its level.
        let kept = obs.iter().find(|o| o.hero == "npc_dota_hero_lina" && o.status == "known").unwrap();
        assert!((kept.confidence - 0.9).abs() < 1e-9);
        assert_eq!(kept.level, Some(22));
        // The downgraded duplicate has its level cleared.
        let loser = obs.iter().find(|o| o.hero == "npc_dota_hero_lina" && o.status == "unconfirmed").unwrap();
        assert_eq!(loser.level, None, "downgraded duplicate must not carry a level");
        // A non-duplicated hero is untouched (status and level).
        let axe = obs.iter().find(|o| o.hero == "npc_dota_hero_axe").unwrap();
        assert_eq!(axe.status, "known");
        assert_eq!(axe.level, Some(18));
    }
}

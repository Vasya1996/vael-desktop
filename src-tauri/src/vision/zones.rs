//! HUD zone geometry for the scoreboard surface.
//!
//! The scoreboard (opened on Tab / by clicking the HUD) is the recognition surface:
//! it shows all 10 hero portraits in two fixed groups of five — Radiant on top, Dire
//! below — plus a level column. Coordinates are stored as fractions of the captured
//! client area, measured on a real 1920x1080 capture at the product owner's HUD scale.
//!
//! Geometry alone is never trusted: every capture is cross-checked against GSI via the
//! own-row oracle (see `pipeline`). If the player's OWN hero is not found at its
//! expected group/position, the whole capture is discarded rather than risk shipping a
//! misaligned (wrong) enemy read. A small per-row re-lock search in the pipeline absorbs
//! minor misalignment; this table is the prediction it searches around.

/// A pixel rectangle inside a captured frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

// Portrait cell (fractions of capture width/height). Measured: top-left x=86, w=88,
// h=52 on 1918x1078; Radiant rows start at y=95, Dire at y=479, row pitch ~70.
const COL_X: f64 = 86.0 / 1918.0;
const CELL_W: f64 = 88.0 / 1918.0;
const CELL_H: f64 = 52.0 / 1078.0;
const RADIANT_Y0: f64 = 95.0 / 1078.0;
const DIRE_Y0: f64 = 479.0 / 1078.0;
const ROW_PITCH: f64 = 70.0 / 1078.0;

// Level box (the "УР." circle) relative to the row's portrait top-left, measured on the
// same 1918x1078 surface: left +306, top +8, size 44x36 px. The digits sit centred in
// this box; `vision::level` masks out the gold XP ring and reads them.
const LVL_DX: f64 = 306.0 / 1918.0;
const LVL_DY: f64 = 8.0 / 1078.0;
const LVL_W: f64 = 44.0 / 1918.0;
const LVL_H: f64 = 36.0 / 1078.0;

/// Number of players per team and in total.
pub const TEAM_ROWS: usize = 5;
pub const TOTAL_ROWS: usize = 10;

fn frac_rect(cap_w: u32, cap_h: u32, fx: f64, fy: f64, fw: f64, fh: f64) -> Rect {
    let w = cap_w as f64;
    let h = cap_h as f64;
    Rect {
        x: (fx * w).round() as u32,
        y: (fy * h).round() as u32,
        w: (fw * w).round().max(1.0) as u32,
        h: (fh * h).round().max(1.0) as u32,
    }
}

/// Which team a scoreboard row index belongs to (rows 0..4 Radiant, 5..9 Dire).
pub fn team_of_row(row: usize) -> &'static str {
    if row < TEAM_ROWS {
        "radiant"
    } else {
        "dire"
    }
}

/// The level ("УР.") box for a row, given that row's portrait rectangle (already carrying
/// any lock offset). Offsets are scaled from the calibrated 1918x1078 surface.
pub fn level_rect(portrait: Rect, cap_w: u32, cap_h: u32) -> Rect {
    Rect {
        x: portrait.x + (LVL_DX * cap_w as f64).round() as u32,
        y: portrait.y + (LVL_DY * cap_h as f64).round() as u32,
        w: (LVL_W * cap_w as f64).round().max(1.0) as u32,
        h: (LVL_H * cap_h as f64).round().max(1.0) as u32,
    }
}

/// Predicted portrait rectangles for all 10 rows, in capture pixels. Index 0..4 are the
/// Radiant group (top), 5..9 the Dire group (bottom).
pub fn portrait_rects(cap_w: u32, cap_h: u32) -> [Rect; TOTAL_ROWS] {
    let mut out = [Rect { x: 0, y: 0, w: 0, h: 0 }; TOTAL_ROWS];
    for (row, slot) in out.iter_mut().enumerate() {
        let (y0, idx) = if row < TEAM_ROWS {
            (RADIANT_Y0, row)
        } else {
            (DIRE_Y0, row - TEAM_ROWS)
        };
        let fy = y0 + idx as f64 * ROW_PITCH;
        *slot = frac_rect(cap_w, cap_h, COL_X, fy, CELL_W, CELL_H);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ten_rows_split_into_two_teams() {
        let rects = portrait_rects(1918, 1078);
        assert_eq!(rects.len(), 10);
        assert_eq!(team_of_row(0), "radiant");
        assert_eq!(team_of_row(4), "radiant");
        assert_eq!(team_of_row(5), "dire");
        assert_eq!(team_of_row(9), "dire");
    }

    #[test]
    fn geometry_matches_the_measured_frame() {
        let r = portrait_rects(1918, 1078);
        // Row 0 (own Pudge) locked at ~ (86, 95), size ~88x52 in the validation run.
        assert!((r[0].x as i64 - 86).abs() <= 2, "x {}", r[0].x);
        assert!((r[0].y as i64 - 95).abs() <= 2, "y {}", r[0].y);
        assert!((r[0].w as i64 - 88).abs() <= 2, "w {}", r[0].w);
        assert!((r[0].h as i64 - 52).abs() <= 2, "h {}", r[0].h);
        // Dire row 0 starts at ~479.
        assert!((r[5].y as i64 - 479).abs() <= 2, "dire y {}", r[5].y);
        // Rows within a team are spaced by ~70.
        assert!((r[1].y as i64 - r[0].y as i64 - 70).abs() <= 2);
    }
}

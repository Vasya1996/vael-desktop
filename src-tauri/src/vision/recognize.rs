//! Recognition gating: turn a (top1 score, top1−top2 margin) from the color-ZNCC
//! matcher into a `VisionObservation` status.
//!
//! The gate is deliberately conservative because the server's EnemyBoard keeps every
//! `known` fact for the rest of the match (newest read per hero wins, but a wrong hero is
//! never removed) — so a single false `known` is a permanent wrong enemy hero. We would
//! rather emit `unconfirmed` (which the server drops) a hundred times than one wrong
//! `known`. Coverage is recovered cheaply by multi-frame voting at the capture layer.

use serde::Serialize;

/// Mirrors shared/protocol.ts VisionObservation. `team`/`level`/`items` are filled by the
/// pipeline / later stages (and omitted from JSON when None).
#[derive(Serialize, Debug, Clone)]
pub struct VisionObservation {
    pub hero: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<String>>,
    pub confidence: f64,
    pub status: String,
}

// Conservative thresholds (color ZNCC, range ~-1..1), tuned on a real 10-hero scoreboard
// frame. There, every correctly-placed portrait read at peak >= 0.58 and margin >= 0.28,
// while half-aligned crops (a window straddling two rows) topped out at peak 0.51 /
// margin 0.13. Gating at 0.45 / 0.15 leaves comfortable headroom over the true reads yet
// rejects every misaligned/partial read — so the EnemyBoard never gets a wrong hero.
pub const PEAK_MIN: f32 = 0.45;
pub const MARGIN_MIN: f32 = 0.15;
const MARGIN_FULL: f32 = 0.40;

/// Gate a (top1 score, top1−top2 margin) into (status, confidence). `known` requires a
/// clear peak AND a clear margin over the runner-up; otherwise `unconfirmed`.
pub fn classify(top1: f32, margin: f32) -> (&'static str, f64) {
    let known = top1 >= PEAK_MIN && margin >= MARGIN_MIN;
    let confidence = (margin / MARGIN_FULL).clamp(0.0, 1.0) as f64;
    (if known { "known" } else { "unconfirmed" }, confidence)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_requires_peak_and_margin() {
        assert_eq!(classify(0.50, 0.20).0, "known");
        assert_eq!(classify(0.45, 0.15).0, "known"); // exact boundaries
        assert_eq!(classify(0.44, 0.30).0, "unconfirmed"); // peak too low
        assert_eq!(classify(0.80, 0.14).0, "unconfirmed"); // margin too small
        assert_eq!(classify(0.50, 0.40), ("known", 1.0)); // margin saturates confidence
        assert!((classify(0.50, 0.20).1 - 0.5).abs() < 1e-9);
    }

    #[test]
    fn observation_omits_empty_optionals_in_json() {
        let obs = VisionObservation {
            hero: "npc_dota_hero_lina".to_string(),
            team: Some("dire".to_string()),
            level: None,
            items: None,
            confidence: 0.9,
            status: "known".to_string(),
        };
        let json = serde_json::to_string(&obs).unwrap();
        assert!(json.contains("\"hero\""));
        assert!(json.contains("\"team\":\"dire\""));
        assert!(json.contains("\"status\":\"known\""));
        assert!(!json.contains("level"));
        assert!(!json.contains("items"));
    }
}

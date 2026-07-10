//! Minimal file logger for the vision pipeline — diagnostic only (no `log`/`tracing`
//! crate). Writes timestamped lines to `%APPDATA%\vael\logs\vision.log` (a temp dir in
//! test builds, so tests never touch the real config dir), with simple size-based
//! rotation. Every failure is swallowed: a broken log must never break the pipeline.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

// Full-dump throttle: `scan_topbar_now` can be called up to ~4 Hz during an on-demand
// fast-watch window (main.rs's 250 ms cadence), so cap a full scan dump to once every
// couple of seconds. Disabled in test builds (cfg(test) applies crate-wide under
// `cargo test`) so unit tests can assert on every call's output deterministically.
#[cfg(not(test))]
const THROTTLE: Duration = Duration::from_secs(2);
#[cfg(test)]
const THROTTLE: Duration = Duration::from_secs(0);

fn log_dir() -> PathBuf {
    #[cfg(test)]
    {
        std::env::temp_dir().join("vael_vision_test_logs")
    }
    #[cfg(not(test))]
    {
        crate::config_dir().join("logs")
    }
}

fn log_path() -> PathBuf {
    log_dir().join("vision.log")
}

/// Throttle gate for one scan's worth of log lines: true at most once per `THROTTLE`.
/// Call once per scan and reuse the result for every line that scan would write.
pub fn gate() -> bool {
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    let mut last = LAST.lock().unwrap();
    let now = Instant::now();
    let allow = last.map_or(true, |t| now.duration_since(t) >= THROTTLE);
    if allow {
        *last = Some(now);
    }
    allow
}

/// UTC "YYYY-MM-DDTHH:MM:SS" from the current time, no external date crate. Civil
/// calendar algorithm: http://howardhinnant.github.io/date_algorithms.html
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86400);
    let tod = secs.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}",
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    )
}

/// Append one timestamped line, rotating `vision.log` -> `vision.log.1` (overwriting
/// any previous `.1`) once it exceeds 5 MB. Every failure (dir/file/write) is
/// swallowed — a logging problem must never affect the vision pipeline.
pub fn write(line: &str) {
    let dir = log_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = log_path();
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > MAX_LOG_BYTES {
            let _ = std::fs::rename(&path, dir.join("vision.log.1"));
        }
    }
    let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) else {
        return;
    };
    let _ = writeln!(f, "{} {line}", timestamp());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_from_days_matches_a_known_epoch() {
        // 2024-01-01T00:00:00Z is the well-known UNIX timestamp 1704067200.
        assert_eq!(civil_from_days(1704067200 / 86400), (2024, 1, 1));
    }

    #[test]
    fn gate_never_throttles_in_test_builds() {
        // THROTTLE is zero under cfg(test), so back-to-back calls both pass —
        // lets every test assert on its own scan's log lines deterministically.
        assert!(gate());
        assert!(gate());
    }

    #[test]
    fn write_appends_a_timestamped_line_to_the_test_log_dir() {
        let path = log_path();
        let _ = std::fs::remove_file(&path);
        write("hello vlog test");
        let contents = std::fs::read_to_string(&path).expect("log file must exist");
        assert!(contents.contains("hello vlog test"));
        assert!(contents.contains('T'), "line must be timestamp-prefixed: {contents}");
    }
}

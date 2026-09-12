//! Append-only audit log of completed operations — mitos-pkg's equivalent
//! of `apt history` / `dnf history`.
//!
//! Deliberately JSON-Lines on disk (one JSON object per line) rather than
//! one big JSON array: appending a line never requires reading, parsing,
//! and rewriting the whole file the way updating a JSON array would, so
//! the cost of logging one more operation stays constant no matter how
//! long the system has been running — the same reason line-oriented log
//! formats (the systemd journal, most audit logs) exist instead of one
//! ever-growing document.

use crate::error::Result;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, Write};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// RFC 3339 UTC timestamp, e.g. `2026-09-07T12:34:56Z` — hand-
    /// formatted (see `now_rfc3339`) rather than pulling in a datetime
    /// crate, since the only things ever done with it are displaying it
    /// and letting a human sort the file, neither of which needs real
    /// calendar arithmetic.
    pub timestamp: String,
    pub operation: String,
    pub package: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_version: Option<String>,
}

impl HistoryEntry {
    pub fn new(operation: &str, package: &str) -> Self {
        Self {
            timestamp: now_rfc3339(),
            operation: operation.to_string(),
            package: package.to_string(),
            from_version: None,
            to_version: None,
        }
    }

    pub fn with_versions(mut self, from: Option<String>, to: Option<String>) -> Self {
        self.from_version = from;
        self.to_version = to;
        self
    }
}

/// Appends one entry. Best-effort by design, the same posture as
/// `install::rollback`: a history log that can't be written to is a
/// (surfaced) problem, but it must never be the reason an otherwise-
/// successful install/remove/upgrade gets reported as failed — a write
/// failure here is logged to stderr and swallowed, never propagated.
pub fn append(path: &Path, entry: &HistoryEntry) {
    let line = match serde_json::to_string(entry) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("mitos-pkg: warning: failed to serialize history entry: {e}");
            return;
        }
    };
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("mitos-pkg: warning: failed to create history directory: {e}");
            return;
        }
    }
    let result = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut f| writeln!(f, "{line}"));
    if let Err(e) = result {
        eprintln!("mitos-pkg: warning: failed to write history entry: {e}");
    }
}

/// The most recent `limit` entries, oldest first. Reads the whole file —
/// history is plain text and expected to stay small relative to
/// mitos-pkg's other on-disk state; a system doing thousands of installs
/// a day is not this tool's target (see README "Status" if that ever
/// stops being true).
pub fn read_recent(path: &Path, limit: usize) -> Result<Vec<HistoryEntry>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file = std::fs::File::open(path)?;
    let reader = std::io::BufReader::new(file);
    let mut all = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<HistoryEntry>(&line) {
            Ok(entry) => all.push(entry),
            Err(e) => eprintln!("mitos-pkg: warning: skipping unreadable history entry: {e}"),
        }
    }
    let start = all.len().saturating_sub(limit);
    Ok(all.split_off(start))
}

/// Hand-rolled RFC 3339 UTC timestamp (`YYYY-MM-DDTHH:MM:SSZ`) from
/// `SystemTime`, without a datetime crate.
fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let days = secs / 86_400;
    let time_of_day = secs % 86_400;
    let (hour, minute, second) = (
        time_of_day / 3600,
        (time_of_day % 3600) / 60,
        time_of_day % 60,
    );

    let (year, month, day) = civil_from_days(days as i64);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Days-since-epoch -> (year, month, day), using Howard Hinnant's
/// widely-used constant-time civil-from-days algorithm (proleptic
/// Gregorian calendar; correct for the entire range a log timestamp here
/// will ever fall in).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_and_read_round_trips() {
        let path = std::env::temp_dir().join("mitos-pkg-test-history.jsonl");
        let _ = std::fs::remove_file(&path);

        append(
            &path,
            &HistoryEntry::new("install", "mitos-shell")
                .with_versions(None, Some("1.2.0".to_string())),
        );
        append(&path, &HistoryEntry::new("remove", "old-pkg"));

        let entries = read_recent(&path, 10).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].package, "mitos-shell");
        assert_eq!(entries[1].operation, "remove");

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn read_recent_respects_limit() {
        let path = std::env::temp_dir().join("mitos-pkg-test-history-limit.jsonl");
        let _ = std::fs::remove_file(&path);

        for i in 0..5 {
            append(&path, &HistoryEntry::new("install", &format!("pkg{i}")));
        }

        let entries = read_recent(&path, 2).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].package, "pkg3");
        assert_eq!(entries[1].package, "pkg4");

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn civil_from_days_matches_known_reference_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(10957), (2000, 1, 1));
        assert_eq!(civil_from_days(19723), (2024, 1, 1));
    }
}

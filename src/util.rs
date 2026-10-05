//! Small helpers shared by the engine and the front ends.

/// Decimal (SI) byte count in the style of Apple's `ByteCountFormatter`
/// ("8.2 GB", "140 MB"), used for the engine's user-facing sentences so they
/// read the same as the app's own formatting.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["KB", "MB", "GB", "TB", "PB"];
    if bytes < 1000 {
        return format!("{bytes} bytes");
    }
    let mut value = bytes as f64;
    let mut unit = UNITS[0];
    for u in UNITS {
        value /= 1000.0;
        unit = u;
        if value < 1000.0 {
            break;
        }
    }
    if value < 10.0 {
        format!("{value:.1} {unit}")
    } else {
        format!("{value:.0} {unit}")
    }
}

/// A percentage for a sentence: one decimal below 10% ("0.4%", "9.5%"), whole
/// numbers above ("12%"), so small but real losses never read as "0%".
pub fn format_percent(part: u64, whole: u64) -> String {
    if whole == 0 {
        return "0%".to_string();
    }
    let pct = part as f64 / whole as f64 * 100.0;
    if pct < 10.0 {
        format!("{pct:.1}%")
    } else {
        format!("{pct:.0}%")
    }
}

/// Run blocking file work on Tokio's blocking pool and wait for it, so it
/// doesn't hold up the async threads (and with them another job's
/// download). A panic in `work` carries on in the caller, as it would have
/// had `work` run there.
pub(crate) async fn blocking<T, F>(work: F) -> T
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    match tokio::task::spawn_blocking(work).await {
        Ok(value) => value,
        Err(e) => match e.try_into_panic() {
            Ok(panic) => std::panic::resume_unwind(panic),
            Err(e) => panic!("blocking work did not finish: {e}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_decimal_units() {
        assert_eq!(format_bytes(999), "999 bytes");
        assert_eq!(format_bytes(8_200_000_000), "8.2 GB");
        assert_eq!(format_bytes(140_000_000), "140 MB");
        assert_eq!(format_percent(9, 100), "9.0%");
        assert_eq!(format_percent(1, 3), "33%");
    }
}

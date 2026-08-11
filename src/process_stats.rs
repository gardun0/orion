//! Process CPU usage sampling for the footer readout. Linux reads
//! /proc/self/stat; other platforms simply hide the readout for now.

/// Cumulative CPU ticks (utime + stime) from /proc/self/stat.
#[cfg(target_os = "linux")]
pub(crate) fn read_process_ticks() -> Option<u64> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    parse_process_ticks(&stat)
}

/// Not implemented off Linux: the footer shows nothing.
#[cfg(not(target_os = "linux"))]
pub(crate) fn read_process_ticks() -> Option<u64> {
    None
}

/// Parse utime + stime (fields 14-15) from a /proc stat line; the command
/// name in parentheses may contain spaces, so split after the last paren.
#[cfg(target_os = "linux")]
fn parse_process_ticks(stat: &str) -> Option<u64> {
    let after = stat.rsplit_once(')')?.1.trim();
    let mut fields = after.split_whitespace();
    let utime: u64 = fields.nth(11)?.parse().ok()?;
    let stime: u64 = fields.next()?.parse().ok()?;
    Some(utime + stime)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn parses_ticks_past_a_spaced_command_name() {
        // utime/stime are fields 14/15; after the paren, state is index 0 and
        // ten filler fields sit between state and utime.
        let make = |utime: u64, stime: u64| {
            let mut parts = vec!["1234 (my app)".to_string(), "S".to_string()];
            parts.extend(std::iter::repeat_n("0".to_string(), 10));
            parts.push(utime.to_string());
            parts.push(stime.to_string());
            parts.join(" ")
        };
        assert_eq!(parse_process_ticks(&make(42, 17)), Some(59));
        assert_eq!(parse_process_ticks(&make(1000, 250)), Some(1250));
    }

    #[test]
    fn rejects_malformed_lines() {
        assert_eq!(parse_process_ticks("garbage"), None);
        assert_eq!(parse_process_ticks("1 (ok) S 1"), None);
    }
}

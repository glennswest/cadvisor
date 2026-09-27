//! Go's RFC 3339 timestamps, as cadvisor's JSON carries them
//! (`2026-07-16T15:02:14.82626863Z`), in nanoseconds since the epoch.

pub fn nanos(s: &str) -> Option<i128> {
    let b = s.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't' | b' ') || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let num = |a: usize, z: usize| s.get(a..z)?.parse::<i64>().ok();
    let (y, mo, d, h, mi, se) = (num(0, 4)?, num(5, 7)?, num(8, 10)?, num(11, 13)?, num(14, 16)?, num(17, 19)?);
    let mut i = 19;
    let mut frac: i128 = 0;
    if b.get(i) == Some(&b'.') {
        i += 1;
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        let digits = &s[start..i];
        if digits.is_empty() || digits.len() > 9 {
            return None;
        }
        frac = digits.parse::<i128>().ok()? * 10i128.pow(9 - digits.len() as u32);
    }
    let off = match &s[i..] {
        "Z" | "z" => 0,
        tz if tz.len() == 6 && (tz.starts_with('+') || tz.starts_with('-')) && &tz[3..4] == ":" => {
            let m = tz[1..3].parse::<i64>().ok()? * 3600 + tz[4..6].parse::<i64>().ok()? * 60;
            if tz.starts_with('-') { -m } else { m }
        }
        _ => return None,
    };
    let secs = days_from_civil(y, mo, d) * 86400 + h * 3600 + mi * 60 + se - off;
    Some(secs as i128 * 1_000_000_000 + frac)
}

/// Days since 1970-01-01 (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

#[cfg(test)]
mod tests {
    use super::nanos;

    #[test]
    fn go_timestamps() {
        assert_eq!(nanos("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(nanos("1970-01-01T00:00:01.5Z"), Some(1_500_000_000));
        // date -d 2026-07-16T15:02:14Z +%s
        assert_eq!(nanos("2026-07-16T15:02:14.82626863Z"), Some(1_784_214_134_826_268_630));
        assert_eq!(nanos("2026-07-16T17:02:14+02:00"), nanos("2026-07-16T15:02:14Z"));
        assert_eq!(nanos("0001-01-01T00:00:00Z").map(|n| n < 0), Some(true));
        assert_eq!(nanos("yesterday"), None);
        assert_eq!(nanos("2026-07-16T15:02:14"), None);
    }
}

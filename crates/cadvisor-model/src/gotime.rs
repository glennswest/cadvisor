//! [`GoTime`] — a UTC timestamp with Go `time.Time` JSON semantics.
//!
//! Go's `time.Time.MarshalJSON` emits RFC3339 with nanosecond precision where
//! the fractional part has trailing zeros trimmed and is omitted entirely when
//! zero (`time.RFC3339Nano`). The zero value serializes as
//! `"0001-01-01T00:00:00Z"` and, being a struct, is NOT omitted by `omitempty`.

use chrono::{DateTime, NaiveDate, Timelike, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GoTime(pub DateTime<Utc>);

impl GoTime {
    /// Go's zero time: January 1, year 1, 00:00:00 UTC.
    pub fn zero() -> Self {
        GoTime(
            NaiveDate::from_ymd_opt(1, 1, 1)
                .unwrap()
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc(),
        )
    }

    pub fn is_zero(&self) -> bool {
        *self == Self::zero()
    }

    pub fn now() -> Self {
        GoTime(Utc::now())
    }

    /// Formats like Go `time.RFC3339Nano` (always with a `Z` offset — all
    /// GoTime values are UTC).
    pub fn to_rfc3339_nano(&self) -> String {
        let mut s = self.0.format("%Y-%m-%dT%H:%M:%S").to_string();
        let nanos = self.0.nanosecond();
        if nanos > 0 {
            let frac = format!("{nanos:09}");
            s.push('.');
            s.push_str(frac.trim_end_matches('0'));
        }
        s.push('Z');
        s
    }
}

impl Default for GoTime {
    fn default() -> Self {
        Self::zero()
    }
}

impl From<DateTime<Utc>> for GoTime {
    fn from(dt: DateTime<Utc>) -> Self {
        GoTime(dt)
    }
}

impl Serialize for GoTime {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_rfc3339_nano())
    }
}

impl<'de> Deserialize<'de> for GoTime {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = <&str>::deserialize(deserializer)?;
        DateTime::parse_from_rfc3339(s)
            .map(|dt| GoTime(dt.with_timezone(&Utc)))
            .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn json(t: &GoTime) -> String {
        serde_json::to_string(t).unwrap()
    }

    #[test]
    fn zero_value_matches_go() {
        assert_eq!(json(&GoTime::zero()), r#""0001-01-01T00:00:00Z""#);
        assert_eq!(json(&GoTime::default()), r#""0001-01-01T00:00:00Z""#);
    }

    #[test]
    fn fraction_omitted_when_zero() {
        let t = GoTime(Utc.with_ymd_and_hms(2026, 7, 16, 10, 30, 5).unwrap());
        assert_eq!(json(&t), r#""2026-07-16T10:30:05Z""#);
    }

    #[test]
    fn fraction_trailing_zeros_trimmed() {
        let base = Utc.with_ymd_and_hms(2026, 7, 16, 10, 30, 5).unwrap();
        let cases = [
            (500_000_000u32, r#""2026-07-16T10:30:05.5Z""#),
            (123_456_789, r#""2026-07-16T10:30:05.123456789Z""#),
            (123_456_700, r#""2026-07-16T10:30:05.1234567Z""#),
            (1_000_000, r#""2026-07-16T10:30:05.001Z""#),
            (1, r#""2026-07-16T10:30:05.000000001Z""#),
        ];
        for (nanos, want) in cases {
            let t = GoTime(base.with_nanosecond(nanos).unwrap());
            assert_eq!(json(&t), want);
        }
    }

    #[test]
    fn parses_offsets_and_fractions() {
        let t: GoTime = serde_json::from_str(r#""2026-07-16T10:30:05.25+08:00""#).unwrap();
        assert_eq!(json(&t), r#""2026-07-16T02:30:05.25Z""#);
        let z: GoTime = serde_json::from_str(r#""0001-01-01T00:00:00Z""#).unwrap();
        assert!(z.is_zero());
    }
}

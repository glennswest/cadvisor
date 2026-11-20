//! Delimited protobuf exposition (`io.prometheus.client.MetricFamily`), as
//! client_golang serves it when a scraper's `Accept` asks for it first (#40).
//!
//! Hand-written: only the fields cadvisor emits (name, help, type, and per
//! metric its labels, a counter or gauge value and the timestamp), each
//! family prefixed by its length as a varint.

use crate::families::Kind;

/// The `Content-Type` client_golang (expfmt `FmtProtoDelim`) answers with.
pub const CONTENT_TYPE: &str =
    "application/vnd.google.protobuf; proto=io.prometheus.client.MetricFamily; encoding=delimited";

fn varint(buf: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        buf.push((v as u8) | 0x80);
        v >>= 7;
    }
    buf.push(v as u8);
}

fn key(buf: &mut Vec<u8>, field: u32, wire: u8) {
    varint(buf, (u64::from(field) << 3) | u64::from(wire));
}

fn bytes_field(buf: &mut Vec<u8>, field: u32, b: &[u8]) {
    key(buf, field, 2);
    varint(buf, b.len() as u64);
    buf.extend_from_slice(b);
}

/// One `Metric`: labels (field 1), counter (3) or gauge (2) `{value: double}`,
/// and `timestamp_ms` (6) when the sample carries one.
fn metric(out: &mut Vec<u8>, kind: Kind, labels: &[(&str, &str)], value: f64, ts: Option<i64>) {
    let mut pair = Vec::new();
    for (k, v) in labels {
        pair.clear();
        bytes_field(&mut pair, 1, k.as_bytes());
        bytes_field(&mut pair, 2, v.as_bytes());
        bytes_field(out, 1, &pair);
    }
    let mut val = Vec::with_capacity(9);
    key(&mut val, 1, 1);
    val.extend_from_slice(&value.to_le_bytes());
    bytes_field(out, if kind == Kind::Counter { 3 } else { 2 }, &val);
    if let Some(ts) = ts {
        key(out, 6, 0);
        varint(out, ts as u64);
    }
}

/// Appends one length-delimited `MetricFamily`.
pub fn family<'a>(
    buf: &mut Vec<u8>,
    name: &str,
    help: &str,
    kind: Kind,
    series: impl IntoIterator<Item = (Vec<(&'a str, &'a str)>, f64, Option<i64>)>,
) {
    let mut fam = Vec::new();
    bytes_field(&mut fam, 1, name.as_bytes());
    bytes_field(&mut fam, 2, help.as_bytes());
    key(&mut fam, 3, 0);
    varint(&mut fam, if kind == Kind::Counter { 0 } else { 1 });
    let mut m = Vec::new();
    for (labels, value, ts) in series {
        m.clear();
        metric(&mut m, kind, &labels, value, ts);
        bytes_field(&mut fam, 4, &m);
    }
    varint(buf, fam.len() as u64);
    buf.extend_from_slice(&fam);
}

/// Whether `Accept` asks for delimited protobuf ahead of the text format,
/// following expfmt's `Negotiate`: entries by descending `q` (ties keep
/// their order), the first a format it knows wins, text when none does.
/// The protobuf text and compact-text encodings are not served, so they are
/// passed over.
pub fn wants_protobuf(accept: &str) -> bool {
    let mut entries: Vec<(f32, bool, bool)> = Vec::new(); // (q, is_delimited_proto, is_text)
    for part in accept.split(',') {
        let mut fields = part.split(';').map(str::trim);
        let media = fields.next().unwrap_or("").to_ascii_lowercase();
        let (mut q, mut proto, mut encoding, mut version) = (1.0f32, "", "", None);
        for p in fields {
            let Some((k, v)) = p.split_once('=') else { continue };
            let v = v.trim().trim_matches('"');
            match k.trim().to_ascii_lowercase().as_str() {
                "q" => q = v.parse().unwrap_or(0.0),
                "proto" => proto = v,
                "encoding" => encoding = v,
                "version" => version = Some(v),
                _ => {}
            }
        }
        if q <= 0.0 {
            continue;
        }
        let is_proto = media == "application/vnd.google.protobuf"
            && proto == "io.prometheus.client.MetricFamily"
            && encoding == "delimited";
        let is_text = media == "text/plain" && version.is_none_or(|v| v == "0.0.4");
        entries.push((q, is_proto, is_text));
    }
    entries.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    entries.into_iter().find(|e| e.1 || e.2).is_some_and(|e| e.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_gauge_family_byte_for_byte() {
        let mut buf = Vec::new();
        family(&mut buf, "a", "h", Kind::Gauge, [(vec![("k", "v")], 1.0, Some(5))]);
        let mut want = vec![31u8, 0x0a, 1, b'a', 0x12, 1, b'h', 0x18, 1, 0x22, 21];
        want.extend([0x0a, 6, 0x0a, 1, b'k', 0x12, 1, b'v']); // label
        want.extend([0x12, 9, 0x09, 0, 0, 0, 0, 0, 0, 0xf0, 0x3f]); // gauge 1.0
        want.extend([0x30, 5]); // timestamp_ms
        assert_eq!(buf, want);
    }

    #[test]
    fn counter_without_timestamp_and_a_long_varint() {
        let mut buf = Vec::new();
        family(&mut buf, "c", "", Kind::Counter, [(vec![], 0.5, None)]);
        // type COUNTER = 0; value under field 3; no field 6.
        assert_eq!(&buf[..9], &[20, 0x0a, 1, b'c', 0x12, 0, 0x18, 0, 0x22]);
        assert_eq!(&buf[9..12], &[11, 0x1a, 9]);
        assert_eq!(buf.len(), 21);
        let mut v = Vec::new();
        varint(&mut v, 300);
        assert_eq!(v, [0xac, 0x02]);
    }

    #[test]
    fn accept_negotiation() {
        // Prometheus' own scrape header, and ironprom's.
        assert!(wants_protobuf(
            "application/vnd.google.protobuf;proto=io.prometheus.client.MetricFamily;encoding=delimited;q=0.7,text/plain;version=0.0.4;q=0.3,*/*;q=0.2"
        ));
        assert!(wants_protobuf(
            "application/openmetrics-text;version=1.0.0;q=0.9,application/vnd.google.protobuf;proto=io.prometheus.client.MetricFamily;encoding=delimited;q=0.8,text/plain;version=0.0.4;q=0.5"
        ));
        assert!(!wants_protobuf(""));
        assert!(!wants_protobuf("*/*"));
        assert!(!wants_protobuf("text/plain"));
        assert!(!wants_protobuf(
            "text/plain;version=0.0.4;q=0.9,application/vnd.google.protobuf;proto=io.prometheus.client.MetricFamily;encoding=delimited;q=0.5"
        ));
        // Not delimited, or q=0: not protobuf.
        assert!(!wants_protobuf(
            "application/vnd.google.protobuf;proto=io.prometheus.client.MetricFamily;encoding=text"
        ));
        assert!(!wants_protobuf(
            "application/vnd.google.protobuf;proto=io.prometheus.client.MetricFamily;encoding=delimited;q=0"
        ));
    }
}

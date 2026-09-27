//! A strict reader for Prometheus text format 0.0.4, the way cadvisor writes
//! it: `# HELP` and `# TYPE` before a family's samples, one sample per line
//! (`name{k="v",…} value [timestamp_ms]`). Strict on purpose: a scraper that
//! is lenient would hide exactly what the suites check.

use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    pub name: String,
    pub labels: Vec<(String, String)>,
    pub value: f64,
    pub ts: Option<i64>,
}

impl Sample {
    pub fn label(&self, k: &str) -> Option<&str> {
        self.labels.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
    }
}

#[derive(Debug, Default)]
pub struct Expo {
    pub samples: Vec<Sample>,
    /// Family name → TYPE.
    pub types: HashMap<String, String>,
    pub helps: HashSet<String>,
}

impl Expo {
    /// Samples of `name` whose label `k` is `v`.
    pub fn find<'a>(&'a self, name: &'a str, k: &'a str, v: &'a str) -> impl Iterator<Item = &'a Sample> + 'a {
        self.samples.iter().filter(move |s| s.name == name && s.label(k) == Some(v))
    }

    pub fn has_family(&self, name: &str) -> bool {
        self.samples.iter().any(|s| s.name == name)
    }

    pub fn families(&self) -> usize {
        self.types.len()
    }
}

/// The family a sample belongs to: histogram and summary series carry a
/// suffix the TYPE line does not.
fn family<'a>(name: &'a str, types: &HashMap<String, String>) -> &'a str {
    if types.contains_key(name) {
        return name;
    }
    for suf in ["_bucket", "_sum", "_count"] {
        if let Some(f) = name.strip_suffix(suf) {
            if types.contains_key(f) {
                return f;
            }
        }
    }
    name
}

/// Parse and check a whole exposition. The error names the first bad line.
pub fn parse(text: &str) -> Result<Expo, String> {
    let mut e = Expo::default();
    let mut seen: HashSet<String> = HashSet::new();
    for (i, line) in text.lines().enumerate() {
        let n = i + 1;
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("# ") {
            let mut w = rest.splitn(3, ' ');
            match (w.next(), w.next(), w.next()) {
                (Some("HELP"), Some(name), _) => {
                    e.helps.insert(name.to_string());
                }
                (Some("TYPE"), Some(name), Some(t)) => {
                    if !matches!(t, "counter" | "gauge" | "histogram" | "summary" | "untyped") {
                        return Err(format!("line {n}: TYPE {t:?} for {name}"));
                    }
                    if e.types.insert(name.to_string(), t.to_string()).is_some() {
                        return Err(format!("line {n}: a second TYPE for {name}"));
                    }
                }
                _ => {}
            }
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        let s = sample(line).map_err(|m| format!("line {n}: {m}: {line:?}"))?;
        let fam = family(&s.name, &e.types);
        if !e.types.contains_key(fam) {
            return Err(format!("line {n}: {} has no TYPE line before it", s.name));
        }
        let mut key = s.name.clone();
        for (k, v) in &s.labels {
            key.push_str(&format!("\u{0}{k}={v}"));
        }
        if !seen.insert(key) {
            return Err(format!("line {n}: a second sample for the same series: {line:?}"));
        }
        e.samples.push(s);
    }
    Ok(e)
}

fn is_name(s: &str) -> bool {
    let mut c = s.chars();
    matches!(c.next(), Some(ch) if ch.is_ascii_alphabetic() || ch == '_' || ch == ':')
        && c.all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == ':')
}

/// One sample line.
pub fn sample(line: &str) -> Result<Sample, String> {
    let (name, rest) = match line.find(['{', ' ']) {
        Some(i) => line.split_at(i),
        None => return Err("no value".into()),
    };
    if !is_name(name) {
        return Err(format!("bad metric name {name:?}"));
    }
    let mut labels = Vec::new();
    let mut rest = rest;
    if let Some(r) = rest.strip_prefix('{') {
        let b = r.as_bytes();
        let mut i = 0;
        loop {
            if b.get(i) == Some(&b'}') {
                i += 1;
                break;
            }
            let eq = r[i..].find('=').ok_or("a label with no '='")? + i;
            let k = &r[i..eq];
            if !is_name(k) {
                return Err(format!("bad label name {k:?}"));
            }
            if b.get(eq + 1) != Some(&b'"') {
                return Err(format!("label {k} is not quoted"));
            }
            let mut v = String::new();
            let mut j = eq + 2;
            loop {
                match b.get(j) {
                    None => return Err(format!("label {k} is not closed")),
                    Some(b'\\') => {
                        match b.get(j + 1) {
                            Some(b'n') => v.push('\n'),
                            Some(b'\\') => v.push('\\'),
                            Some(b'"') => v.push('"'),
                            other => return Err(format!("label {k}: bad escape {other:?}")),
                        }
                        j += 2;
                    }
                    Some(b'"') => break,
                    Some(_) => {
                        let ch = r[j..].chars().next().unwrap();
                        v.push(ch);
                        j += ch.len_utf8();
                    }
                }
            }
            labels.push((k.to_string(), v));
            i = j + 1;
            match b.get(i) {
                Some(b',') => i += 1,
                Some(b'}') => {}
                other => return Err(format!("after label {k}: {:?}", other.map(|c| *c as char))),
            }
        }
        rest = &r[i..];
    }
    let rest = rest.strip_prefix(' ').ok_or("no space before the value")?;
    let mut w = rest.split(' ');
    let v = w.next().ok_or("no value")?;
    let value = match v {
        "+Inf" => f64::INFINITY,
        "-Inf" => f64::NEG_INFINITY,
        "NaN" => f64::NAN,
        v => v.parse::<f64>().map_err(|_| format!("value {v:?}"))?,
    };
    let ts = match w.next() {
        Some(t) => Some(t.parse::<i64>().map_err(|_| format!("timestamp {t:?}"))?),
        None => None,
    };
    if w.next().is_some() {
        return Err("more than a value and a timestamp".into());
    }
    Ok(Sample { name: name.to_string(), labels, value, ts })
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = "# HELP cadvisor_version_info A metric with a constant '1' value.\n\
# TYPE cadvisor_version_info gauge\n\
cadvisor_version_info{cadvisorRevision=\"\",cadvisorVersion=\"0.1.0\"} 1\n\
# HELP container_cpu_usage_seconds_total Cumulative cpu time consumed in seconds.\n\
# TYPE container_cpu_usage_seconds_total counter\n\
container_cpu_usage_seconds_total{cpu=\"total\",id=\"/\",image=\"\",name=\"\"} 12.5 1726000000000\n\
container_cpu_usage_seconds_total{cpu=\"total\",id=\"/a \\\"b\\\"\",image=\"\",name=\"\"} 1e-3 1726000000000\n";

    #[test]
    fn a_cadvisor_exposition_parses() {
        let e = parse(GOOD).unwrap();
        assert_eq!(e.families(), 2);
        let root = e.find("container_cpu_usage_seconds_total", "id", "/").next().unwrap();
        assert_eq!((root.value, root.ts), (12.5, Some(1726000000000)));
        assert_eq!(e.find("container_cpu_usage_seconds_total", "id", "/a \"b\"").count(), 1);
        assert_eq!(e.samples[0].label("cadvisorVersion"), Some("0.1.0"));
    }

    #[test]
    fn what_a_scraper_would_reject_is_rejected() {
        assert!(parse("x 1\n").unwrap_err().contains("no TYPE"));
        assert!(parse("# TYPE x gauge\nx{a=\"1\"} 1\nx{a=\"1\"} 2\n").unwrap_err().contains("second sample"));
        assert!(parse("# TYPE x gauge\nx{a=1} 1\n").unwrap_err().contains("not quoted"));
        assert!(parse("# TYPE x gauge\nx 1 2 3\n").is_err());
        assert!(parse("# TYPE x gauge\nx one\n").is_err());
        assert!(parse("# TYPE x gauge\n# TYPE x counter\n").is_err());
    }

    #[test]
    fn histogram_series_belong_to_their_family() {
        let t = "# TYPE h histogram\nh_bucket{le=\"+Inf\"} 3\nh_sum 1.5\nh_count 3\n";
        assert_eq!(parse(t).unwrap().samples.len(), 3);
    }
}

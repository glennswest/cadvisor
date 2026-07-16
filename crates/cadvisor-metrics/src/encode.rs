//! Prometheus text-format (0.0.4) encoding with Go-identical float formatting.
//!
//! client_golang formats sample values with `strconv.AppendFloat(_, 'g', -1, 64)`
//! plus fast paths for 0/1/-1: shortest decimal digits, scientific notation
//! when the decimal exponent is < -4 or >= 6, exponent at least two digits.

/// Formats a float exactly like Go's strconv 'g' with precision -1.
pub fn fmt_float(v: f64, out: &mut String) {
    if v == 0.0 {
        out.push('0');
        return;
    }
    if v == 1.0 {
        out.push('1');
        return;
    }
    if v == -1.0 {
        out.push_str("-1");
        return;
    }
    if v.is_nan() {
        out.push_str("NaN");
        return;
    }
    if v.is_infinite() {
        out.push_str(if v > 0.0 { "+Inf" } else { "-Inf" });
        return;
    }

    // ryu gives the shortest representation; reshape it to Go 'g' rules.
    let mut buf = ryu::Buffer::new();
    let s = buf.format(v);
    let (neg, s) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s),
    };
    // Split ryu output (either "d.dddd" or "d.ddddej") into digits + exponent.
    let (mantissa, e_part) = match s.split_once(['e', 'E']) {
        Some((m, e)) => (m, Some(e.parse::<i32>().unwrap_or(0))),
        None => (s, None),
    };
    let (int_part, frac_part) = match mantissa.split_once('.') {
        Some((i, f)) => (i, f.trim_end_matches('0')),
        None => (mantissa, ""),
    };
    // digits = significant digits without leading zeros; exp = position of
    // the decimal point relative to the first significant digit, minus one.
    let mut digits = String::with_capacity(20);
    let mut exp: i32;
    if let Some(e) = e_part {
        // ryu scientific: int_part is a single digit
        digits.push_str(int_part);
        digits.push_str(frac_part);
        exp = e;
    } else if int_part == "0" {
        // 0.000ddd
        let zeros = frac_part.len() - frac_part.trim_start_matches('0').len();
        digits.push_str(frac_part.trim_start_matches('0'));
        exp = -(zeros as i32) - 1;
    } else {
        digits.push_str(int_part);
        digits.push_str(frac_part);
        exp = int_part.len() as i32 - 1;
    }
    let trimmed = digits.trim_end_matches('0');
    let digits = if trimmed.is_empty() { "0" } else { trimmed };
    if digits == "0" {
        exp = 0;
    }

    if neg {
        out.push('-');
    }
    if exp < -4 || exp >= 6 {
        // scientific: d.ddde±XX
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        if exp >= 0 {
            out.push('+');
        } else {
            out.push('-');
        }
        let abs = exp.unsigned_abs();
        if abs < 10 {
            out.push('0');
        }
        let mut itoa_buf = itoa::Buffer::new();
        out.push_str(itoa_buf.format(abs));
    } else if exp >= digits.len() as i32 - 1 {
        // integer with possible trailing zeros
        out.push_str(digits);
        for _ in 0..(exp - (digits.len() as i32 - 1)) {
            out.push('0');
        }
    } else if exp >= 0 {
        out.push_str(&digits[..=exp as usize]);
        out.push('.');
        out.push_str(&digits[exp as usize + 1..]);
    } else {
        out.push_str("0.");
        for _ in 0..(-exp - 1) {
            out.push('0');
        }
        out.push_str(digits);
    }
}

pub fn escape_label_value(v: &str, out: &mut String) {
    for c in v.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
}

/// One rendered sample line: `name{labels} value [timestamp_ms]\n`.
pub fn sample_line(
    out: &mut String,
    name: &str,
    labels: &[(&str, &str)],
    value: f64,
    timestamp_ms: Option<i64>,
) {
    out.push_str(name);
    if !labels.is_empty() {
        out.push('{');
        for (i, (k, v)) in labels.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(k);
            out.push_str("=\"");
            escape_label_value(v, out);
            out.push('"');
        }
        out.push('}');
    }
    out.push(' ');
    fmt_float(value, out);
    if let Some(ts) = timestamp_ms {
        out.push(' ');
        let mut b = itoa::Buffer::new();
        out.push_str(b.format(ts));
    }
    out.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(v: f64) -> String {
        let mut s = String::new();
        fmt_float(v, &mut s);
        s
    }

    #[test]
    fn go_g_formatting() {
        assert_eq!(f(0.0), "0");
        assert_eq!(f(1.0), "1");
        assert_eq!(f(8.0), "8");
        assert_eq!(f(100000.0), "100000");
        assert_eq!(f(15079.320663), "15079.320663");
        assert_eq!(f(16.70385), "16.70385");
        assert_eq!(f(13614579712.0), "1.3614579712e+10");
        assert_eq!(f(41504768.0), "4.1504768e+07");
        assert_eq!(f(700592128.0), "7.00592128e+08");
        assert_eq!(f(1784214137.0), "1.784214137e+09");
        assert_eq!(f(0.25), "0.25");
        assert_eq!(f(0.0001), "0.0001");
        assert_eq!(f(0.00001), "1e-05");
        assert_eq!(f(-2597.0), "-2597");
        assert_eq!(f(1e21), "1e+21");
    }

    /// Every numeric token in the captured v0.49.2 exposition must re-format
    /// byte-identically through fmt_float.
    #[test]
    fn matches_entire_fixture() {
        let fixture = include_str!("../../../conformance/fixtures/metrics.prom");
        let mut checked = 0;
        for line in fixture.lines() {
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            let rest = match line.rfind('}') {
                Some(i) => &line[i + 1..],
                None => &line[line.find(' ').unwrap()..],
            };
            let value_str = rest.split_whitespace().next().unwrap();
            let parsed: f64 = match value_str.parse() {
                Ok(v) => v,
                Err(_) => continue,
            };
            assert_eq!(f(parsed), value_str, "value {value_str} reformats differently");
            checked += 1;
        }
        assert!(checked > 500, "expected many samples, checked {checked}");
    }

    #[test]
    fn line_rendering() {
        let mut out = String::new();
        sample_line(&mut out, "m_total", &[("id", "/a\"b")], 2.0, Some(123));
        assert_eq!(out, "m_total{id=\"/a\\\"b\"} 2 123\n");
        out.clear();
        sample_line(&mut out, "m", &[], 0.0, None);
        assert_eq!(out, "m 0\n");
    }
}

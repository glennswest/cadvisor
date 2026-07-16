//! Derived statistics: instantaneous CPU rate (nanocores/second) from two
//! cumulative samples — upstream `InstCpuStats`.

use cadvisor_model::v1;

/// Returns None when a rate cannot be derived (matches upstream's error
/// cases: time moved backwards, per-cpu count changed, counter decreased).
pub fn inst_cpu(last: &v1::ContainerStats, cur: &v1::ContainerStats) -> Option<v1::CpuInstStats> {
    if cur.timestamp <= last.timestamp {
        return None;
    }
    if last.cpu.usage.per_cpu.len() != cur.cpu.usage.per_cpu.len() {
        return None;
    }
    let delta_ns = (cur.timestamp.0 - last.timestamp.0).num_nanoseconds()? as u64;
    if delta_ns == 0 {
        return None;
    }
    let rate = |last_v: u64, cur_v: u64| -> Option<u64> {
        let delta = cur_v.checked_sub(last_v)?;
        Some((delta as f64 / delta_ns as f64 * 1e9) as u64)
    };
    let mut per_cpu = Vec::with_capacity(cur.cpu.usage.per_cpu.len());
    for (l, c) in last.cpu.usage.per_cpu.iter().zip(&cur.cpu.usage.per_cpu) {
        per_cpu.push(rate(*l, *c)?);
    }
    Some(v1::CpuInstStats {
        usage: v1::CpuInstUsage {
            total: rate(last.cpu.usage.total, cur.cpu.usage.total)?,
            per_cpu,
            user: rate(last.cpu.usage.user, cur.cpu.usage.user)?,
            system: rate(last.cpu.usage.system, cur.cpu.usage.system)?,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cadvisor_model::GoTime;
    use chrono::TimeZone;

    fn sample(sec: u32, total_ns: u64) -> v1::ContainerStats {
        let mut s = v1::ContainerStats {
            timestamp: GoTime(chrono::Utc.with_ymd_and_hms(2026, 7, 16, 12, 0, sec).unwrap()),
            ..Default::default()
        };
        s.cpu.usage.total = total_ns;
        s.cpu.usage.user = total_ns / 2;
        s.cpu.usage.system = total_ns / 2;
        s
    }

    #[test]
    fn one_core_busy() {
        // 1s of cpu over 2s wall = 500M nanocores/s.
        let inst = inst_cpu(&sample(0, 0), &sample(2, 1_000_000_000)).unwrap();
        assert_eq!(inst.usage.total, 500_000_000);
        assert_eq!(inst.usage.user, 250_000_000);
    }

    #[test]
    fn rejects_backwards() {
        assert!(inst_cpu(&sample(2, 0), &sample(1, 10)).is_none());
        assert!(inst_cpu(&sample(0, 100), &sample(1, 50)).is_none(), "counter decreased");
    }
}

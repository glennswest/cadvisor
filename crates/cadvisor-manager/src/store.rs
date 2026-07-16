//! `TimedStore` — the per-container stats ring buffer.
//!
//! Replicates upstream cadvisor's TimedStore semantics: age-based eviction
//! only (default `--storage_duration` 2m), and window selection that trims
//! from the OLD side so the most recent `max_results` samples win, returned
//! oldest→newest.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use cadvisor_model::GoTime;
use cadvisor_model::v1;

#[derive(Debug)]
pub struct TimedStore {
    buf: VecDeque<Arc<v1::ContainerStats>>,
    max_age: Duration,
}

impl TimedStore {
    pub fn new(max_age: Duration) -> Self {
        TimedStore { buf: VecDeque::new(), max_age }
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Adds a sample (assumed newest — housekeeping is the only writer) and
    /// evicts everything older than `newest.timestamp - max_age`.
    pub fn push(&mut self, sample: Arc<v1::ContainerStats>) {
        // Keep timestamp order even if a stale sample slips in.
        match self.buf.back() {
            Some(last) if last.timestamp > sample.timestamp => {
                let pos = self.buf.partition_point(|s| s.timestamp <= sample.timestamp);
                self.buf.insert(pos, sample);
            }
            _ => self.buf.push_back(sample),
        }
        let newest = self.buf.back().unwrap().timestamp.0;
        let cutoff = newest - chrono::Duration::from_std(self.max_age).unwrap_or_default();
        while let Some(front) = self.buf.front() {
            if front.timestamp.0 < cutoff && self.buf.len() > 1 {
                self.buf.pop_front();
            } else {
                break;
            }
        }
    }

    pub fn latest(&self) -> Option<Arc<v1::ContainerStats>> {
        self.buf.back().cloned()
    }

    pub fn two_latest(&self) -> Option<(Arc<v1::ContainerStats>, Arc<v1::ContainerStats>)> {
        let n = self.buf.len();
        if n < 2 {
            return None;
        }
        Some((self.buf[n - 2].clone(), self.buf[n - 1].clone()))
    }

    /// Upstream `InTimeRange` + `getMaxNumSamples` semantics.
    /// `start`/`end` zero-valued means unbounded; `max < 0` means unlimited.
    pub fn in_range(&self, start: GoTime, end: GoTime, max: i64) -> Vec<Arc<v1::ContainerStats>> {
        if self.buf.is_empty() {
            return Vec::new();
        }
        let lo = if start.is_zero() {
            0
        } else {
            self.buf.partition_point(|s| s.timestamp < start)
        };
        let hi = if end.is_zero() {
            self.buf.len()
        } else {
            self.buf.partition_point(|s| s.timestamp <= end)
        };
        if lo >= hi {
            return Vec::new();
        }
        let mut lo = lo;
        let count = (hi - lo) as i64;
        if max >= 0 && count > max {
            // Trim from the old side: keep the most recent `max`.
            lo = hi - max as usize;
        }
        self.buf.range(lo..hi).cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t(sec: u32) -> GoTime {
        GoTime(chrono::Utc.with_ymd_and_hms(2026, 7, 16, 12, 0, sec).unwrap())
    }

    fn sample(sec: u32) -> Arc<v1::ContainerStats> {
        Arc::new(v1::ContainerStats { timestamp: t(sec), ..Default::default() })
    }

    fn store_with(secs: &[u32]) -> TimedStore {
        let mut s = TimedStore::new(Duration::from_secs(120));
        for &sec in secs {
            s.push(sample(sec));
        }
        s
    }

    #[test]
    fn age_eviction() {
        let mut s = TimedStore::new(Duration::from_secs(10));
        for sec in [0, 5, 9, 12, 25] {
            s.push(sample(sec));
        }
        // newest=25, cutoff=15 -> only 25 remains... but 12 < 15 evicted, 25 kept.
        let all = s.in_range(GoTime::zero(), GoTime::zero(), -1);
        let secs: Vec<u32> = all.iter().map(|x| x.timestamp.0.timestamp() as u32 % 100).collect();
        assert_eq!(secs, vec![25]);
    }

    #[test]
    fn most_recent_n_when_trimming() {
        let s = store_with(&[1, 2, 3, 4, 5]);
        let out = s.in_range(GoTime::zero(), GoTime::zero(), 2);
        let secs: Vec<i64> = out.iter().map(|x| x.timestamp.0.timestamp() % 100).collect();
        assert_eq!(secs, vec![4, 5], "keeps most recent, chronological order");
    }

    #[test]
    fn window_selection() {
        let s = store_with(&[1, 2, 3, 4, 5]);
        let out = s.in_range(t(2), t(4), -1);
        let secs: Vec<i64> = out.iter().map(|x| x.timestamp.0.timestamp() % 100).collect();
        assert_eq!(secs, vec![2, 3, 4]);

        assert!(s.in_range(t(50), GoTime::zero(), -1).is_empty(), "start after newest");
        assert!(s.in_range(GoTime::zero(), t(0), -1).is_empty(), "end before oldest");
    }

    #[test]
    fn unlimited_and_zero() {
        let s = store_with(&[1, 2, 3]);
        assert_eq!(s.in_range(GoTime::zero(), GoTime::zero(), -1).len(), 3);
        assert_eq!(s.in_range(GoTime::zero(), GoTime::zero(), 0).len(), 0);
    }

    #[test]
    fn two_latest_order() {
        let s = store_with(&[1, 2, 3]);
        let (prev, last) = s.two_latest().unwrap();
        assert!(prev.timestamp < last.timestamp);
    }
}

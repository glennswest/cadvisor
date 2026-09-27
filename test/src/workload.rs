//! `/test workload <marker> cpu=<millicores> mem=<MiB> secs=<n>
//! [oom_after=<s> limit=<MiB>]`: the load the suites start in pods, from this
//! same image, so they know what cadvisor should report for it.
//!
//! It holds `mem` MiB written (so resident, not just mapped), burns `cpu`
//! millicores in 100 ms duty cycles, and exits 0 after `secs` (0 = never).
//! With `oom_after`, after that many seconds it starts a child (`/test
//! oomchild <cap>`) that allocates in 8 MiB steps until the kernel kills it.
//! The child, not this process, is the one killed, so the container outlives
//! the OOM and cadvisor has time to read the rise in `memory.events`. If the
//! child reaches four times `limit` first, no limit was applied: it exits 3,
//! and so does the workload, so the suite can tell that apart from an OOM kill.
//! The marker is only there to be found in its command line.

use std::hint::black_box;
use std::time::{Duration, Instant};

/// Exit code when the memory limit was never enforced.
pub const NO_LIMIT: i32 = 3;

#[derive(Debug, Clone, PartialEq)]
pub struct Load {
    pub cpu_millis: u32,
    pub mem_mib: u32,
    pub secs: u32,
    pub oom_after: Option<u32>,
    /// The pod's memory limit, MiB (`resources.limits.memory`).
    pub limit_mib: Option<u32>,
}

impl Load {
    pub fn idle(secs: u32) -> Load {
        Load { cpu_millis: 0, mem_mib: 0, secs, oom_after: None, limit_mib: None }
    }

    pub fn args(&self, marker: &str) -> Vec<String> {
        let mut a = vec![
            "workload".to_string(),
            marker.to_string(),
            format!("cpu={}", self.cpu_millis),
            format!("mem={}", self.mem_mib),
            format!("secs={}", self.secs),
        ];
        if let Some(s) = self.oom_after {
            a.push(format!("oom_after={s}"));
        }
        if let Some(l) = self.limit_mib {
            a.push(format!("limit={l}"));
        }
        a
    }

    /// The arguments after `workload <marker>`.
    pub fn parse(args: &[String]) -> Result<Load, String> {
        let mut l = Load::idle(0);
        for a in args {
            let (k, v) = a.split_once('=').ok_or_else(|| format!("{a:?} is not key=value"))?;
            let n: u32 = v.parse().map_err(|_| format!("{k}: {v:?} is not a number"))?;
            match k {
                "cpu" => l.cpu_millis = n,
                "mem" => l.mem_mib = n,
                "secs" => l.secs = n,
                "oom_after" => l.oom_after = Some(n),
                "limit" => l.limit_mib = Some(n),
                _ => return Err(format!("unknown workload argument {k:?}")),
            }
        }
        Ok(l)
    }
}

const MIB: usize = 1 << 20;
const PERIOD: Duration = Duration::from_millis(100);

/// Run the load. Returns the exit code.
pub fn run(load: &Load) -> i32 {
    let start = Instant::now();
    let held = touched(load.mem_mib as usize * MIB);
    if load.cpu_millis > 0 {
        let threads = load.cpu_millis.div_ceil(1000);
        let each = load.cpu_millis / threads;
        for _ in 0..threads {
            std::thread::spawn(move || burn(each));
        }
    }
    let mut oom_done = false;
    loop {
        if load.secs > 0 && start.elapsed() >= Duration::from_secs(load.secs as u64) {
            black_box(&held);
            return 0;
        }
        if let (Some(after), false) = (load.oom_after, oom_done) {
            if start.elapsed() >= Duration::from_secs(after as u64) {
                oom_done = true;
                let cap = load.limit_mib.unwrap_or(64) * 4;
                match oom_child(cap) {
                    Ok(true) => println!("the oom child was killed by SIGKILL"),
                    Ok(false) => {
                        println!("the oom child allocated {cap} MiB and was not killed: no memory limit");
                        return NO_LIMIT;
                    }
                    Err(e) => {
                        println!("the oom child: {e}");
                        return 4;
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Run `/test oomchild <cap>` and wait: `true` when it was SIGKILLed.
fn oom_child(cap_mib: u32) -> Result<bool, String> {
    use std::os::unix::process::ExitStatusExt;
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let st = std::process::Command::new(exe)
        .args(["oomchild", &cap_mib.to_string()])
        .status()
        .map_err(|e| format!("spawn: {e}"))?;
    match (st.signal(), st.code()) {
        (Some(9), _) => Ok(true),
        (_, Some(c)) if c == NO_LIMIT => Ok(false),
        _ => Err(format!("ended {st}")),
    }
}

/// `/test oomchild <cap>`: allocate until killed, or exit 3 at `cap` MiB.
pub fn oom_alloc(cap_mib: u32) -> i32 {
    let mut extra: Vec<Vec<u8>> = Vec::new();
    while extra.len() * 8 < cap_mib as usize {
        extra.push(touched(8 * MIB));
        black_box(&extra);
        std::thread::sleep(Duration::from_millis(20));
    }
    NO_LIMIT
}

/// `n` bytes, every page written.
fn touched(n: usize) -> Vec<u8> {
    let v = vec![1u8; n];
    black_box(v)
}

/// Keep `millis` of a core busy.
fn burn(millis: u32) {
    let busy = PERIOD * millis.min(1000) / 1000;
    let mut x: u64 = 1;
    loop {
        let t = Instant::now();
        while t.elapsed() < busy {
            x = black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407));
        }
        let rest = PERIOD.saturating_sub(t.elapsed());
        if !rest.is_zero() {
            std::thread::sleep(rest);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_round_trip() {
        let l = Load { cpu_millis: 1500, mem_mib: 64, secs: 30, oom_after: Some(10), limit_mib: Some(48) };
        let a = l.args("cadvt-r-a");
        assert_eq!(&a[..2], &["workload".to_string(), "cadvt-r-a".to_string()]);
        assert_eq!(Load::parse(&a[2..]).unwrap(), l);
        assert!(Load::parse(&["cpu".into()]).is_err());
        assert!(Load::parse(&["gpu=1".into()]).is_err());
    }

    #[test]
    fn a_short_workload_ends_on_time() {
        let t = Instant::now();
        assert_eq!(run(&Load { cpu_millis: 100, mem_mib: 1, secs: 1, oom_after: None, limit_mib: None }), 0);
        assert!(t.elapsed() < Duration::from_secs(3));
    }
}

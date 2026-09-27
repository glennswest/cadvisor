//! `/test`: the container's entrypoint.
//!
//! - `/test short|medium|long` (or `STORM_SUITE`): run a suite within
//!   `STORM_TIMEOUT`, one JSON line per test, exit 0, 1 or 2.
//! - `/test workload <marker> key=value…`: the load the suites start in pods.
//! - `/test oomchild <MiB>`: the workload's child that allocates until killed.

use cadvisor_test::env::Env;
use cadvisor_test::report::{Outcome, Report};
use cadvisor_test::workload;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("workload") => std::process::exit(match workload::Load::parse(args.get(2..).unwrap_or(&[])) {
            Ok(l) => workload::run(&l),
            Err(e) => {
                eprintln!("workload: {e}");
                64
            }
        }),
        Some("oomchild") => std::process::exit(workload::oom_alloc(args.get(1).and_then(|a| a.parse().ok()).unwrap_or(256))),
        _ => {}
    }

    let env = Env::read(args.first().cloned());
    let mut r = Report::new();
    let missing = env.missing();
    if !missing.is_empty() {
        r.record("environment", Outcome::Infra(format!("the runner did not set {}", missing.join(", "))), 0, None);
        std::process::exit(r.finish());
    }
    let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            r.record("runtime", Outcome::Infra(format!("tokio runtime: {e}")), 0, None);
            std::process::exit(r.finish());
        }
    };
    let deadline = env.timeout;
    let ran = rt.block_on(async { tokio::time::timeout(deadline, cadvisor_test::run(&env, &mut r)).await });
    if ran.is_err() {
        r.record(
            "timeout",
            Outcome::Fail(format!("the suite did not finish within STORM_TIMEOUT ({} s)", deadline.as_secs())),
            deadline.as_millis(),
            None,
        );
    }
    let code = r.finish();
    rt.shutdown_background();
    std::process::exit(code);
}

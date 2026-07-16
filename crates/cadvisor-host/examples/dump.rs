//! Smoke tool: dump machine info or a cgroup's spec+stats as JSON.
//!
//!   cargo run -p cadvisor-host --example dump machine
//!   cargo run -p cadvisor-host --example dump cgroup /machine.slice/libpod-<id>.scope

#[cfg(target_os = "linux")]
fn main() {
    use cadvisor_host::{cgroup::CgroupReader, fs::FsService, machine};
    use cadvisor_model::GoTime;

    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap_or_else(|| "machine".into());
    match mode.as_str() {
        "machine" => {
            let fs = FsService::new().expect("mountinfo");
            let info = machine::machine_info(&fs, GoTime::now()).expect("machine_info");
            println!("{}", serde_json::to_string_pretty(&info).unwrap());
        }
        "cgroup" => {
            let cg = args.next().expect("usage: dump cgroup <cgroup-path>");
            let reader = CgroupReader::default();
            let spec = reader.read_spec(&cg).expect("spec");
            let stats = reader.read_stats(&cg, GoTime::now()).expect("stats");
            eprintln!("spec: {spec:#?}");
            println!("{}", serde_json::to_string_pretty(&stats).unwrap());
        }
        other => eprintln!("unknown mode {other}"),
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("dump is Linux-only");
}

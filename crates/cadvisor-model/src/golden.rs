//! Golden tests: fixtures captured from real google/cadvisor v0.49.2
//! (Fedora 43, cgroup v2, podman workloads — see fixtures/ directory).
//!
//! Each fixture is deserialized into our typed model and re-serialized; the
//! result must be structurally identical to the original. This catches missing
//! fields, wrong renames, and wrong omitempty behavior in BOTH directions.

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::collections::BTreeMap;

use crate::{v1, v2};

/// Recursively collects paths where `ours` differs from `theirs`.
fn diff(path: &str, theirs: &Value, ours: &Value, out: &mut Vec<String>) {
    match (theirs, ours) {
        (Value::Object(a), Value::Object(b)) => {
            for (k, va) in a {
                match b.get(k) {
                    Some(vb) => diff(&format!("{path}/{k}"), va, vb, out),
                    None => out.push(format!("{path}/{k}: missing in ours (theirs: {va})")),
                }
            }
            for k in b.keys() {
                if !a.contains_key(k) {
                    out.push(format!("{path}/{k}: extra in ours"));
                }
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            if a.len() != b.len() {
                out.push(format!("{path}: array len {} vs {}", a.len(), b.len()));
            }
            for (i, (va, vb)) in a.iter().zip(b.iter()).enumerate() {
                diff(&format!("{path}[{i}]"), va, vb, out);
            }
        }
        (Value::Number(a), Value::Number(b)) if a != b => {
            // Go marshals float32 with shortest-round-trip formatting ("0.1",
            // "0"); serde_json prints f32 through f64
            // ("0.10000000149011612", "0.0"). Same float32 value — compare at
            // f32 precision.
            let same_f32 = match (a.as_f64(), b.as_f64()) {
                (Some(x), Some(y)) => (x as f32) == (y as f32),
                _ => false,
            };
            if !same_f32 {
                out.push(format!("{path}: {a} != {b}"));
            }
        }
        _ => {
            if theirs != ours {
                out.push(format!("{path}: {theirs} != {ours}"));
            }
        }
    }
}

fn round_trip<T: Serialize + DeserializeOwned>(name: &str, text: &str) {
    let theirs: Value = serde_json::from_str(text).expect("fixture must be valid JSON");
    let typed: T = serde_json::from_str(text)
        .unwrap_or_else(|e| panic!("{name}: model failed to deserialize fixture: {e}"));
    let ours = serde_json::to_value(&typed).unwrap();
    let mut problems = Vec::new();
    diff("", &theirs, &ours, &mut problems);
    if !problems.is_empty() {
        let shown = problems.iter().take(25).cloned().collect::<Vec<_>>().join("\n  ");
        panic!(
            "{name}: {} structural differences vs real cadvisor output:\n  {shown}",
            problems.len()
        );
    }
}

macro_rules! golden {
    ($test:ident, $ty:ty, $file:literal) => {
        #[test]
        fn $test() {
            round_trip::<$ty>($file, include_str!(concat!("../fixtures/", $file)));
        }
    };
}

golden!(v1_machine, v1::MachineInfo, "v1_machine.json");
golden!(v1_containers_root, v1::ContainerInfo, "v1_containers_root.json");
golden!(v1_containers_workload, v1::ContainerInfo, "v1_containers_workload.json");
golden!(v1_subcontainers, Vec<v1::ContainerInfo>, "v1_subcontainers.json");
golden!(v1_events, Vec<v1::Event>, "v1_events.json");
golden!(v2_version, String, "v2_version.json");
golden!(v2_attributes, v2::Attributes, "v2_attributes.json");
golden!(
    v2_stats_deprecated,
    BTreeMap<String, Vec<v2::DeprecatedContainerStats>>,
    "v2_stats_deprecated.json"
);
golden!(v21_stats, BTreeMap<String, v2::ContainerInfo>, "v21_stats.json");
golden!(v21_machinestats, Vec<v2::MachineStats>, "v21_machinestats.json");
golden!(v2_summary, BTreeMap<String, v2::DerivedStats>, "v2_summary.json");
golden!(v2_spec, BTreeMap<String, v2::ContainerSpec>, "v2_spec.json");
golden!(v2_ps, Vec<v2::ProcessInfo>, "v2_ps.json");
golden!(v2_storage, Vec<v2::FsInfo>, "v2_storage.json");

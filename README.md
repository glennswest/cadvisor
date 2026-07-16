# cadvisor-rs

An efficient, API-compatible reimplementation of [google/cadvisor](https://github.com/google/cadvisor)
in Rust: Prometheus `/metrics`, REST `/api/v1.0`–`v1.3` and `/api/v2.0`–`v2.1`, with
containerd and CRI-O metadata enrichment. cgroup v2 only. Linux target.

Sibling project of [ironprom](https://github.com/glennswest/ironprom); shares its
workspace conventions and is its primary scrape target for container metrics.

## Performance

Measured side by side against cadvisor v0.49.2 — same Fedora 43 host, 60 busybox
containers (~130 cgroups), default flags, release build (2026-07-16):

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/perf-dark.svg">
  <img alt="Paired bar chart: cadvisor-rs vs cadvisor v0.49.2. Resident memory 36.3 MB vs 62.6 MB (1.7x smaller); /metrics scrape 0.84 ms vs 5.9 ms (7x faster); CPU 1.4% vs 5.5% (3.9x less); binary 6.2 MB vs 46 MB (7.4x smaller). Lower is better." src="docs/perf-light.svg">
</picture>

| | cadvisor v0.49.2 | cadvisor-rs | |
|---|---|---|---|
| Resident memory | 62.6 MB | **36.3 MB** | 1.7× smaller |
| /metrics scrape (avg of 10) | 5.9 ms | **0.84 ms** | 7.0× faster |
| CPU (30s window incl. scrapes) | 5.5 % | **1.4 %** | 3.9× less |
| Binary size | 46 MB | **6.2 MB** | 7.4× smaller |

The mechanisms: no metrics registry or reflection (one pass over the containers
renders straight into a reused buffer), label sets pre-rendered per container,
borrow-based parsing with no maps or regexes on the hot path, and adaptive
1s→60s housekeeping so idle containers cost almost nothing.

## Crates

- `cadvisor` — the server binary; wires the subsystems together and owns the process lifecycle.
- `cadvisor-model` — all wire types (v1, v2, machine, events) with byte-compatible serde.
- `cadvisor-host` — cgroup-v2 / procfs / sysfs data plane (parsers testable off-Linux).
- `cadvisor-runtime` — containerd (gRPC) and CRI-O (unix-socket HTTP) metadata clients.
- `cadvisor-manager` — container registry, in-memory stats ring buffer, housekeeping, discovery.
- `cadvisor-metrics` — hand-rolled Prometheus text-0.0.4 exposition.
- `cadvisor-api` — axum routers for the v1/v2 REST APIs.

## Compatibility

The API contract (JSON field names — including upstream's typos, which are replicated
verbatim — metric names/labels, error bodies, flag semantics) is pinned against
upstream **cadvisor v0.49.2**; `conformance/` runs both side by side on a live host
and diffs the outputs structurally. As of 2026-07-16 the `/metrics` surface and all
v1.3/v2.0/v2.1 endpoints are conformant on plain-cgroup, containerd, and CRI-O hosts
(`deploy/terragrunt/runtime-test/` provisions the containerd+CRI-O test VM).

Known deliberate deviation: the `docker` request types answer from containerd/CRI-O
metadata (upstream returns an empty/erroring result without dockershim).

## Running

Drop-in: flag names, defaults, and Go-style single-dash long flags match upstream.

```sh
cargo build --release
sudo ./target/release/cadvisor -port 8080
curl localhost:8080/metrics
curl localhost:8080/api/v1.3/containers/
```

Needs root (or equivalent) for cgroup/proc access, cgroup v2, and — for
name/image/label enrichment — the containerd (`--containerd`, default
`/run/containerd/containerd.sock`, namespace `--containerd_namespace=k8s.io`)
and/or CRI-O (`--crio`, default `/var/run/crio/crio.sock`) sockets. Missing
sockets degrade gracefully to raw-cgroup monitoring. OOM events come from
`memory.events`; container discovery is inotify-driven with a 1-minute sweep
as safety net (`--global_housekeeping_interval`).

## Packaging

`make package` builds both `.rpm` and `.deb` via nfpm (same convention as
rustkube/fastetcd) into `dist/`: `/usr/bin/cadvisor`, a systemd unit, and an
`EnvironmentFile` at `/etc/cadvisor/cadvisor`. The service is not auto-enabled:

```sh
dnf install cadvisor-rs-<version>.x86_64.rpm   # or: dpkg -i cadvisor-rs_<version>_amd64.deb
systemctl enable --now cadvisor
```

Released artifacts are attached to GitHub releases, so cloud-init/Terragrunt
units can pin an RPM URL.

## Developing

Development happens on macOS; everything Linux runs on a dev host over SSH
(see the `REMOTE` variable in the Makefile).

```sh
make test          # platform-neutral tests (all parsers are &str-in)
make test-linux    # rsync to the dev host, full suite incl. Linux data plane
make build-linux   # release build on the dev host
```

- Wire-format changes: `crates/cadvisor-model/fixtures/` holds captured
  v0.49.2 responses; the golden tests round-trip every fixture and must stay
  byte-faithful.
- Behavior changes: run the conformance harness — start real
  `cadvisor v0.49.2` and cadvisor-rs side by side on the same host, then
  `conformance/diff-metrics.sh` (normalized /metrics diff) and
  `conformance/diff-api.py` (JSON key-path/type diff across all endpoints).
- Runtime integration: `deploy/terragrunt/runtime-test/` provisions a Fedora
  VM with containerd + CRI-O on the g8 Proxmox (allocate the vm_id with
  `deploy/terragrunt/free-vmid.sh` first; `terragrunt destroy` when done).

---
marp: true
theme: default
paginate: true
title: cadvisor-rs — container metrics on every stormcos node
description: Purpose and functionality of cadvisor-rs v0.1.0, from the code
---

<!-- Render: npx @marp-team/marp-cli docs/presentation.md          (HTML)
             npx @marp-team/marp-cli --pdf docs/presentation.md    (PDF)
     Written 2026-09-26 against cadvisor-rs v0.1.0 (main after #7). Every
     claim is checkable in the source or in the files named on each slide;
     README.md has the full reference. -->

# cadvisor-rs

### What every container and VM on a stormcos node is actually using

v0.1.0 · github.com/glennswest/cadvisor

---

## What it is, and the problem it solves

Prometheus dashboards, alerts and tools already speak **cAdvisor**: its
`/metrics` families and its `/api/v1.x` / `/api/v2.x` REST API. stormcos nodes
run each service as one static binary in a golden under `stormd`, and upstream
cAdvisor is a 46 MB Go binary.

**cadvisor-rs is a Rust reimplementation that is wire-compatible with
cadvisor v0.49.2.** It reads cgroup v2, procfs and sysfs. It serves the same
metric names, labels and JSON, and it adds container names, images and labels
from containerd or CRI-O. The release binary is 6.2 MB (glibc build); on
stormcos it ships as a static musl binary in a golden under `stormd`.

cgroup v2 only, Linux only. On any other OS the binary prints a message and
exits 1 (`crates/cadvisor/src/main.rs`).

---

## Where it sits in stormcos

From stormcentral's relationships graph (`stormcentral/config/stormcentral.toml`):

```
stormcos ──depends_on──▶ cadvisor        group "node", role "Node metrics in Rust"
cadvisor ──depends_on──▶ (nothing listed)
```

What it touches at run time (not edges in that graph):

| | How | Source |
|---|---|---|
| **stormd** | PID 1 of its container: starts `cadvisor --port 9096 --listen-ip 0.0.0.0`, probes `/healthz` | `stormcentral/components/stormcos.toml` |
| **stormpump** | writes the cgroups cadvisor reads; `boot.d/40-services` has `start cadvisor` | `stormcos/deploy/build-goldens.sh` |
| **stormlb** | routes `cadvisor.storm1.g8.lo` → `127.0.0.1:9096` | `stormcos/deploy/manifests/85-routes.yaml` |
| containerd / CRI-O | optional metadata over their unix sockets | `crates/cadvisor-runtime` |

It is listed in `stormcos/docs/CLUSTER.md` as `cadvisor` — "container metrics",
on every node.

---

## How it works

```
 /sys/fs/cgroup ──inotify + 1m sweep──▶ ┌─────────────── cadvisor-manager ───────────────┐
 /proc, /sys ─────────────────────────▶ │ registry: one entry per cgroup                 │
   (cadvisor-host readers)              │ per-container housekeeping 1s → backs off 60s  │
                                        │ TimedStore ring buffer, 2m, memory only        │
 containerd.sock (gRPC) ─┐              │ events: create / delete / oom / oomKill        │
 crio.sock (HTTP/1) ─────┴─metadata───▶ │ (cadvisor-runtime enrichment by 64-hex id)     │
                                        └───────────────┬────────────────────────────────┘
                                                        │ snapshots
                        ┌───────────────────────────────┴──────────────────────┐
                        ▼                                                      ▼
            cadvisor-metrics: /metrics                         cadvisor-api: /api/v1.0–v1.3,
            hand-written exposition, one pass                  /api/v2.0–v2.1 (upstream routing
            into a reused buffer                               and error contract)
                        └──────────── axum server in `cadvisor` (one listener) ─┘
```

Seven crates: `cadvisor` (binary), `-model` (wire types), `-host`, `-runtime`,
`-manager`, `-metrics`, `-api`.

---

## What it does today (1/2) — collection

- **Discovery.** Walks `/sys/fs/cgroup`, watches it with inotify, and re-sweeps
  every `--global-housekeeping-interval` (1m). Each cgroup is a container named
  by its path: `/`, `/system.slice/…`, `/machine.slice/…`. Because stormpump's
  cgroups are read directly, VMs and engine domains show up too, not only
  runtime containers.
- **Stats**, with upstream's v2 semantics: `working_set` = `memory.current` −
  `inactive_file`; `cache` = `file`, `rss` = `anon`, `max_usage` = `memory.peak`;
  `cpu.stat` µs → ns; `cpu.weight` → v1 shares; `io.stat`, `pids.*`.
- **Adaptive housekeeping**: 1s per container, backing off to 60s while stats
  do not change. Samples live 2m in memory; nothing is written to disk.
- **Runtime metadata**: containerd `containers.v1` / `tasks.v1` in namespace
  `k8s.io`; CRI-O `GET /info`, `GET /containers/<id>`. A missing socket means
  raw cgroups, not an error. Network stats come from the pod sandbox's
  `/proc/<pid>/net`.

---

## What it does today (2/2) — serving

- **`/metrics`**: Prometheus text 0.0.4, byte-compatible with v0.49.2 at default
  flags — family names, HELP/TYPE, label sets, Go float formatting, per-sample
  timestamps. `--disable-metrics` / `--enable-metrics` select groups `cpu`,
  `cpuLoad`, `memory`, `disk`, `diskIO`, `network`.
- **REST**: `/api/v1.0`–`v1.3` (machine, containers, subcontainers, docker,
  events with the upstream query parameters and `stream=true`) and
  `/api/v2.0`–`v2.1` (version, machine, attributes, stats, spec, summary, ps,
  storage, events, appmetrics, `v2.1/machinestats`).
- **Events**: creation, deletion, and `oom` + `oomKill` when `memory.events`
  `oom_kill` rises.
- **Deliberate deviation**: the `docker` request types are answered from
  containerd/CRI-O metadata instead of failing without dockershim.

Checked: fixtures captured from v0.49.2 round-trip byte for byte
(`crates/cadvisor-model/fixtures/`); `conformance/` diffs `/metrics` and 15 API
endpoints against a real v0.49.2 (conformant on plain, containerd and CRI-O
hosts, 2026-07-16).

---

## Measured against upstream

Same Fedora 43 host, 60 busybox containers (~130 cgroups), default flags,
release build, 2026-07-16:

| | cadvisor v0.49.2 | cadvisor-rs | |
|---|---|---|---|
| Resident memory | 62.6 MB | **36.3 MB** | 1.7× smaller |
| `/metrics` scrape (avg of 10) | 5.9 ms | **0.84 ms** | 7.0× faster |
| CPU (30s incl. scrapes) | 5.5 % | **1.4 %** | 3.9× less |
| Binary | 46 MB | **6.2 MB** | 7.4× smaller |

Glibc build. Not yet re-measured for the musl golden.

---

## Interfaces

**One HTTP listener**, `--listen-ip`:`--port`. Port **9096** on stormcos
(9095 is stormvm), **8080** upstream/RPM default. Plain HTTP, no auth.

| Path | |
|---|---|
| `/healthz`, `/-/healthy`, `/-/ready` | `200 ok` once listening; do not check collectors |
| `/metrics` | Prometheus |
| `/api/…` | REST v1.0–v1.3, v2.0–v2.1; `/api` lists versions (`400`, as upstream) |

**Configuration is flags only** — no config file, no env except `RUST_LOG`
(default `info`, logs to stderr). Kebab-case names, Go-style single dash
accepted: `--housekeeping-interval 1s`, `--max-housekeeping-interval 60s`,
`--allow-dynamic-housekeeping true`, `--global-housekeeping-interval 1m0s`,
`--storage-duration 2m0s`, `--disable-metrics`, `--enable-metrics`,
`--store-container-labels true`, `--whitelisted-container-labels`,
`--containerd /run/containerd/containerd.sock`, `--containerd-namespace k8s.io`,
`--crio /var/run/crio/crio.sock`. Full table: README.

---

## How it ships and is operated

- **stormcos golden, `kind = "service"`.** `stormcos/deploy/build-goldens.sh`
  builds `x86_64-unknown-linux-musl`, puts it at `/usr/sbin/cadvisor` under
  `stormd`. Three goldens: `cadvisor` (32M, system1), `cadvisor-logs`
  (system1), `cadvisor-data` (data1, at `/var/lib/cadvisor`).
- **How it starts**: stormpump `boot.d/40-services` → `start cadvisor`. The
  container uses the `host` profile and shares the pid and uts namespaces, so it
  can read every cgroup and every `/proc/<pid>`.
- **How it is updated**: push → `sc-build` passes → `stormcentral component build
  cadvisor` makes an immutable golden and files the stormcos release request →
  `compose-release.py` composes a release → nodes clone the new golden
  copy-on-write. A commit alone reaches no node. Authority:
  `stormcos/docs/goldens.md`.
- **Elsewhere**: RPM/DEB `cadvisor-rs` via nfpm, systemd unit, runs as root,
  port 8080.

---

## Planned — not in the code yet

- **TLS and bearer-token auth** on `/metrics` and the API (#4).
- **Upstream underscore flag names** (`-listen_ip`, `-housekeeping_interval`)
  — rejected today (#9).
- **`--env-metadata-whitelist`** — parsed, ignored (#10).
- **Test containers** per the stormcos test standard (#12, P1) and QA +
  must-gather in stormcos_qa (#5).
- **Kubelet library seam** — a `cadvisor-kubelet` facade crate and a
  `discovery: bool` manager switch, so rustkube-node can embed node / fs /
  machine stats (plan in `rustkube-node/docs/planning/cadvisor-integration.md`,
  rustkube-node#21). Nothing of it is in this repo yet; today the kubelet serves
  its own `/metrics/cadvisor` from CRI stats.
- Not planned: protobuf responses and upstream's default-off metric groups
  (tcp/udp, sched, hugetlb, perf, resctrl). Their names are accepted and emit
  nothing, as in a default upstream build. PSI stays off, as in v0.49.2.

---

## Status and open issues

- **v0.1.0**, 64 tests passing under `sc-build` (2026-09-26). Golden
  `golden-cadvisor-fff6b7a3761a` built, release request stormcos#110.
- Docs rewritten from the code (#7); golden shipping documented (#6).

| Issue | | |
|---|---|---|
| #12 | test containers (stormcos test standard) | P1, next |
| #4 | TLS / bearer-token auth | not started |
| #9 / #10 | flag-name compatibility, env whitelist | open |
| #11 | Makefile remote targets use `root@dev` | open, do not use them |
| #3 | CRI discovery | mostly done; pod labels come from runtime labels |
| #5 | QA + must-gather in stormcos_qa | not started |

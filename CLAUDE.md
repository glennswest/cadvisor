# CLAUDE.md — cadvisor (cadvisor-rs)

Cross-project rules: `~/src/CLAUDE.md` (build with `sc-build`, never root,
never build on the stormcentral VM). This file is the project context.

## What this is

A Rust reimplementation of google/cadvisor, wire-compatible with **v0.49.2**:
Prometheus `/metrics` and REST `/api/v1.0`–`v1.3`, `/api/v2.0`–`v2.1`, cgroup v2
only, Linux only, with containerd (gRPC) and CRI-O (unix-socket HTTP) metadata
enrichment. Seven crates under `crates/` — see README.md.

## Version

- Current: **0.1.0** (tag `v0.1.0`).
- The version lives in exactly one place: `[workspace.package] version` in the
  root `Cargo.toml` (every crate uses `version.workspace = true`). The binary
  reports it as `cadvisor_version` (`/api/v2.0/version`,
  `cadvisor_version_info`); the Makefile's `package` target reads it from there.

## How it ships

- **stormcos golden (current).** A `service` component in
  `stormcentral/components/stormcos.toml`, built by
  `stormcos/deploy/build-goldens.sh` (`service_golden cadvisor 32M … 9096 …
  /healthz '"--port", "9096", "--listen-ip", "0.0.0.0"'`) as a static
  `x86_64-unknown-linux-musl` binary under `stormd`. Goldens: `cadvisor`
  (system1), `cadvisor-logs` (system1), `cadvisor-data` (data1). Authority for
  the process: `stormcos/docs/goldens.md`. A commit here reaches a node only
  when a golden is built (`stormcentral component build cadvisor`) and a
  release composed.
- **RPM/DEB** via nfpm (`deploy/packaging/`), for non-stormcos hosts.

## Work plan

### Done
- 2026-10-06 — #11 Makefile: the remote `root@dev` targets and the `fixtures`
  stub are gone. `package` builds locally and finds the binary through `cargo
  metadata` (sc-build's target dir is not ./target; that was #21).
  `conformance` runs both diff scripts. Verified: `sc-build 'make package'` on
  4d394f9 → `cadvisor-rs-0.1.0-1.x86_64.rpm` + `cadvisor-rs_0.1.0_amd64.deb`,
  each with /usr/bin/cadvisor, the unit and /etc/cadvisor/cadvisor. `cargo
  build --locked && cargo test --locked` passed on 3e64158 (78 tests; the
  later commit touches only the Makefile). #11 and #21 closed; golden requested.
- 2026-10-06 — #18 machine info refresh: `-update_machine_info_interval` (5m,
  alias kebab) re-reads `MachineInfo`, and the disk map is re-scanned once per
  new unmapped `io.stat` major:minor (`Manager::note_disks`). Verified with
  `sc-build 'cargo build --locked && cargo test --locked'` on 500d809: exit 0,
  78 tests passed. This includes `machine::tests::disk_map_sees_devices_added_later`
  (fixture /sys/block) and `disk_tests::unknown_disks_lists_only_unmapped`.
  Not yet seen on a node with a hot-attached volume. #18 closed; golden
  golden-cadvisor-5d0f953b1af8 (stormcos#331; #9 was golden-cadvisor-fd886e5a8788).
- 2026-10-06 — #15 picked up and checked: VM hypervisor cgroups are
  discovered (raw: CPU, memory, io.stat) but get no labels. The labels are
  blocked on rustkube-node#84 (open, no shape). Disk also needs #18 here.
  Network needs stormvm#16 (SLIRP today: no tap, no netns, nothing to measure).
  Commented; item proposed `--after rustkube-node#84`. No code change.
- 2026-10-06 — #9 upstream flag names: every flag's `long` is upstream's
  (`listen_ip`, …; `containerd-namespace` as upstream), the other spelling an
  `alias` (golden's `--listen-ip` keeps working); bare Go bools mean true.
  Verified with `sc-build 'cargo build --locked && cargo test --locked && cargo
  run -q -p cadvisor -- --help && cargo run -q -p cadvisor -- -listen_ip
  127.0.0.1 -port 0 -housekeeping_interval 10s -store_container_labels
  --version'` on e63f9cd: exit 0 (`--help` lists underscore names; the
  upstream argv parses). #9 closed; golden requested.
- 2026-10-06 — #3 picked up and re-checked: discovery, cgroup v2 stats and
  the containerd/CRI-O labels are done; the only remaining work (identity for
  stormpump pods) is blocked on rustkube-node#84, still open with no shape
  chosen and no commits (checked rustkube-node's last 40 commits). Item
  proposed `--after rustkube-node#84`; no code change, no golden.
- 2026-09-28 — docs refresh since 2026-09-18, third pass: README (machine
  info read once, #18; #12 node-run status; suites plain HTTP), deck issue
  table (#4 done, #18, #19), this file. New issue: #19.
- 2026-09-28 — issue validation pass: #4 closed (strict `sc-build` `--locked` on d5e94bd: 72 tests pass incl. `tls_auth.rs`; enabling it in the golden is stormcos#143). #3, #5, #9, #10, #11 re-checked against the code, still real (commented). #6, #7, #8 were already closed. No golden requested yet for the #4 code (still due: `stormcentral component build cadvisor`).
- 2026-09-27 — docs refresh since 2026-09-18, second pass (after #4):
  README metric groups (`oom_event`), stale comments in `main.rs`,
  `secure.rs`, `cadvisor.example`; presentation/CLAUDE.md issue tables. Every
  docs-vs-code gap found is already an issue (#3, #4, #9, #10, #11); none new.
- 2026-09-26 — #8 presentation: `docs/presentation.md`, 11-slide Marp deck
  (linked from README). Rendered with `marp-cli` under `sc-build` (exit 0);
  64 tests pass. Relationship slide follows `stormcentral/config/stormcentral.toml`
  (only `stormcos → cadvisor`); runtime neighbours (stormd, stormpump, stormlb,
  containerd/CRI-O) shown separately with their sources.
- 2026-09-26 — #7 docs rewritten from the code (README, CLAUDE.md,
  CHANGELOG.md, module comments); also covers #6 (golden shipping documented,
  linking `stormcos/docs/goldens.md` — the `stormpump/docs/goldens.md` path
  named in #6 does not exist). Verified with `sc-build 'cargo build && cargo
  test && cargo run -q -p cadvisor -- --help'` on ccf57ba: exit 0, 64 tests
  passed, 0 failed; `--help` lists the same 15 flags with the same defaults as
  the README table. Gaps filed as #9, #10, #11 (and #4 already open).

### In progress
- 2026-09-27 — #12 test containers per stormcentral `docs/test-standard.md`.
  Layout follows stormlb's `test/`: own cargo workspace `test/`, static musl
  `/test <suite>` in a scratch image (`test/Containerfile`, `test/build.sh`),
  metadata + reference Job in `test/cadvisor-test.yaml`.
  Facts the design rests on (checked 2026-09-27):
  - stormcentral's runner (`src/testruns.rs`) builds its own Job: SA
    `storm-test` with `*` on the run namespace only, no hostNetwork, no
    cluster-scoped read, image not passed in env → suites read their own pod
    (`HOSTNAME`) for the image and size waves from cadvisor's
    `/api/v2.0/machine`, not from `nodes`.
  - cadvisor on a node: `STORM_NODE:9096` (`--listen-ip 0.0.0.0`).
  - stormpump names pod cgroups `/stormpump/w<tag>-<n>` (opaque), so a
    workload pod is found through cadvisor itself: containers created since the
    pod started (`/api/v2.0/spec?recursive=true`) whose `/api/v2.0/ps` shows
    `/test workload <run id> …`.
  - `ps` lists only a cgroup's own `cgroup.procs`, not descendants.
  Suites: short = health, version, machine, /metrics, housekeeping, a workload
  pod appears with its CPU/memory and disappears after delete. medium = every
  endpoint and error contract, events (create/delete, stream), OOM (skip if the
  runtime applies no limit), accuracy vs a known load, many pods, concurrent
  scrapes. long = waves of workload pods sized from the machine, measuring
  discovery latency, scrape time, cadvisor RSS/fds and leftover containers.
  Status (2026-09-27): all of `test/` pushed (55babdc…18234cb). On dev via
  sc-build: 20 unit tests + harness (short and medium against the real
  binary, pod tests skipped) pass; `test/build.sh --locked` → 3.9 MB
  static-pie; rootless podman build of test/Containerfile OK; image runs
  `workload` (0), no env (2), short vs a real cadvisor (0). The harness found
  #14 (duplicate series in /metrics), fixed in f083ba2 (#14 closed).
  Strict sc-build on 991ce54: 87 tests pass. Golden golden-cadvisor-cc73674e1ff2
  built with the #14 fix (release request stormcos#110).
  **Blocked (2026-09-27):** the on-node run. `stormcentral test run cadvisor
  short --tag C2NR0Q2` (run 1739abfdc4) built and pushed the image, then
  errored: stormcentral#56 (`@@RESULT` shell quoting), and the node's
  sbregistry could not seal the golden (401 from stormblock:
  stormblock-registry#40). Next, once both are fixed: run short, then medium,
  fix what they find, close #12.
  Re-checked 2026-09-27 15:08 UTC: stormcentral#56 fixed in stormcentral
  commit 15:07 (issue still open, deploy unconfirmed). Run 43e9193e15 (short,
  0900b1e) got past power-on and apiserver wait, then C2NR0Q2's sbregistry
  refused connections on :5100. Other sessions' runs on C2NR0Q2 are failing
  its /readyz. C2NR0Q2 is the only test machine. Nothing to change in cadvisor;
  re-run once C2NR0Q2 is healthy.

### Known gaps (code does not do what upstream/docs imply) — tracked as issues
- #10 `--env-metadata-whitelist` is parsed and ignored. `collect.rs` would
  render `spec.envs` as `container_env_*`, but nothing fills them.
- TLS / bearer auth (#4, closed) is in the code but off unless flags are set;
  the stormcos golden does not set them yet (stormcos#143: cert, token, https
  liveness; the anonymous route is dropped meanwhile; stormlb#13).
- #3 stormcos pods (stormpump, cgroups `/stormpump/w<tag>-<n>`) get no
  metadata, only `id`. (CRI-O's `ContainerReference.namespace = "crio"` is
  upstream's own value, not a gap.)
- #12 the test suites have not run on a node yet (C2NR0Q2 down; see above).
  Latest (2026-09-28): C2NR0Q2's :5100 answers again; the furthest run
  (stormcentral fe3fc66b32) broke at the image push (stormblock-registry#56,
  fixed in v0.24.1, deploy on C2NR0Q2 unconfirmed).
- #19 the test crate talks plain HTTP without a token (`test/src/cad.rs`);
  it needs https + a token once stormcos#143 turns TLS/auth on in the golden.
- The version is still 0.1.0: the #14 fix is in golden cc73674e1ff2 without a
  release tag.

### Open issues (validated 2026-09-28; priorities set in stormcentral)
- #12 P1 test container: built and verified on dev; node run blocked.
- #3 P1 pod metadata (see above); blocked on rustkube-node#84 (the identity
  source for stormpump pods; open, not started as of 2026-10-06).
- #16 P3 Decide: bare `pod`/`namespace` labels (beyond upstream) — owner's call.
- #15 P2 per-VM stats keyed to the VMI (from stormconsole#14) — blocked on
  rustkube-node#84 (labels), stormvm#16 (network; SLIRP today).
- #19 P2 test suites need https + token (proposed after stormcos#143).
- #10 P3 env whitelist. #5 P3 must-gather collector only
  (#12 covers the tests).

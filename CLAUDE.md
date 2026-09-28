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

- 2026-09-27 — #4 TLS + bearer auth (owner decided #17 = A: TLS with a
  stormcert certificate, bearer tokens checked in cadvisor). Plan: optional
  flags for cert/key and a token file; with them set, serve HTTPS and require
  `Authorization: Bearer` on every path except the health paths; reload
  cert and tokens when the files change; without them, plain HTTP as today
  (upstream default). Then file on stormcos: drop the unauthenticated
  cadvisor route until a golden with #4 ships.
  Done: f3f80e2 (`crates/cadvisor/src/secure.rs`, flags `--tls-cert-file`,
  `--tls-key-file`, `--bearer-token-file`; health paths exempt because
  stormd's probe sends no token), e2e test `crates/cadvisor/tests/tls_auth.rs`.
  sc-build on f3f80e2: 72 tests pass. Filed stormcos#143 (P1) and stormlb#13.
  Follow-up for #12: the test crate talks plain HTTP without a token; it
  needs https + a token once stormcos#143 turns them on.
  **Where it stopped (2026-09-27, session restart):** code, lock (e432bbc)
  and docs (f756b89) pushed. Remaining: strict `sc-build` on f756b89
  (`cargo build --locked && cargo test --locked`, plus a
  `--release --target x86_64-unknown-linux-musl` build and `--help` showing
  the three flags). The first attempt timed out in the slot queue, and a
  re-run was in flight at restart. When it passes: close #4 (evidence:
  72 tests incl. `tls_auth.rs`; enablement is stormcos#143), then request
  the golden once (`stormcentral component build cadvisor`).

### Known gaps (code does not do what upstream/docs imply) — tracked as issues
- #9 Upstream underscore flag names (`-listen_ip`, `-housekeeping_interval`, …)
  are rejected: clap derives kebab-case (`--listen-ip`).
- #10 `--env-metadata-whitelist` is parsed and ignored. `collect.rs` would
  render `spec.envs` as `container_env_*`, but nothing fills them.
- #11 Makefile `sync`/`test-linux`/`build-linux`/`package` rsync to
  `root@dev.g8.lo`. They predate sc-build; do not use them from stormcentral
  sessions.
- #4 TLS / bearer auth is in the code but off unless flags are set; the
  stormcos golden does not set them yet (stormcos#143: cert, token, https
  liveness; the anonymous route is dropped meanwhile; stormlb#13).
- #3 stormcos pods (stormpump, cgroups `/stormpump/w<tag>-<n>`) get no
  metadata, only `id`. CRI-O's namespace is hardcoded `"crio"`.
- #12 the test suites have not run on a node yet (C2NR0Q2 down; see above).
- The version is still 0.1.0: the #14 fix is in golden cc73674e1ff2 without a
  release tag.

### Open issues (validated 2026-09-27; priorities set in stormcentral)
- #12 P1 test container: built and verified on dev; node run blocked.
- #3 P1 pod metadata (see above); the identity source for stormpump pods is
  rustkube-node#84.
- #16 P3 Decide: bare `pod`/`namespace` labels (beyond upstream) — owner's call.
- #15 P2 per-VM stats keyed to the VMI (from stormconsole#14) — not started;
  nothing yet says which cgroup a stormvm hypervisor lands in.
- #4 P2 TLS and bearer-token auth — implemented; closes when verified.
- #9 P2 upstream flag names.
- #10 P3 env whitelist. #11 P3 Makefile. #5 P3 must-gather collector only
  (#12 covers the tests).

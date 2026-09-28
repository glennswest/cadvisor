# Changelog

## [Unreleased]

### 2026-09-27
- **docs:** second refresh from the code since 2026-09-18 (the only code
  change since the first is #4). README: `oom_event` gates
  `container_oom_events_total` and `cpuLoad` includes `container_tasks_state`;
  `percpu`, `app`, `perf_event` and `pressure` emit nothing. `main.rs` and
  `deploy/systemd/cadvisor.example` no longer claim upstream flag names (#9);
  `secure.rs` no longer says the golden turns TLS on (stormcos#143 pending).
  Presentation and `CLAUDE.md`: #3 is P1 (waits on rustkube-node#84), #16 added.
- **feat:** optional TLS and bearer-token auth on the listener (#4; owner
  decision #17). New flags: `--tls-cert-file`/`--tls-key-file` (HTTPS only,
  rustls with ring) and `--bearer-token-file` (one token per line; `401` +
  `WWW-Authenticate: Bearer` on every path but `/healthz`, `/-/healthy`,
  `/-/ready`). Certificate, key and tokens are re-read when replaced, and a
  certificate/key pair that does not match yet is not applied. Off by
  default, as upstream. End-to-end test `crates/cadvisor/tests/tls_auth.rs`.
- **docs:** README "TLS and auth", flag table, ports; presentation interfaces
  and planned slides. Filed stormcos#143 (drop the anonymous cadvisor route;
  what the golden needs) and stormlb#13 (no TLS to backends).
- **docs:** re-checked README, `docs/presentation.md` and `CLAUDE.md` against
  the code (flags, defaults, ports, routes, shipping). They match. Updated the
  #12 on-node status (stormcentral#56 fixed; C2NR0Q2's registry is down) and
  added #15 (per-VM stats) to the open issues.
- **docs:** refreshed from the code since 2026-09-18. README: the #14 rule
  (a series is served once), stormpump pods have only `id` (#3), how to build
  and test `test/` with `sc-build`, the test suites' on-node status, and the
  `cadvisor-test` crate. `docs/presentation.md`: status, planned and issue
  slides. `CLAUDE.md`: known gaps and open issues with priorities.
- **test:** test container per stormcentral's test standard (#12): `test/`
  (own cargo workspace, static musl `/test short|medium|long`, `FROM scratch`),
  `test/build.sh`, `test/Containerfile`, `test/cadvisor-test.yaml` metadata,
  and a hermetic harness that runs short and medium against the real binary.
- **docs:** README "Tests on a node".
- **docs:** `CLAUDE.md` work plan: #12 verified off-node, on-node run blocked
  (stormcentral#56, stormblock-registry#40).
- **fix:** `/metrics` no longer repeats a series when two `io.stat` devices
  missing from the disk map both resolve to `device=""`; the first is served
  and the rest dropped, as upstream's client_golang does (#14).

### 2026-09-26
- **docs:** `docs/presentation.md` — 11-slide Marp deck on purpose and
  functionality (#8): place in stormcos per stormcentral's relationships
  graph, architecture diagram, features from the code, planned work,
  interfaces, shipping and status.
- **docs:** #7 verified on dev via `sc-build` (64 tests pass; `--help`
  matches the README flag table); work plan in `CLAUDE.md` updated.

### 2026-09-24
- **docs:** README rewritten from the code (#7): what it does today, every
  flag with its real (kebab-case) name and default, endpoints, health paths,
  ports (8080 upstream/RPM, 9096 on stormcos), how it ships as a stormcos
  golden (#6) and as RPM/DEB, building with `sc-build`, and what is not
  implemented (#4, #9, #10, #11).
- **docs:** project `CLAUDE.md` added with version, shipping and work plan.
- **docs:** stale "deferred (M6/M7/M8)" module comments in `cadvisor-manager`
  and `cadvisor-api` corrected; that work is implemented.
- **docs:** this changelog added.

## [v0.1.0] — 2026-07-16

### Added
- Cargo workspace: `cadvisor`, `cadvisor-model`, `cadvisor-host`,
  `cadvisor-runtime`, `cadvisor-manager`, `cadvisor-metrics`, `cadvisor-api`.
- v0.49.2-pinned wire types with byte-compatible serde and captured fixtures.
- cgroup v2 / procfs / sysfs data plane.
- containerd (gRPC) and CRI-O (unix-socket HTTP) metadata clients.
- Manager: registry, TimedStore, adaptive housekeeping, inotify discovery,
  events.
- Prometheus `/metrics` exposition conformant with v0.49.2.
- REST API v1.0–v1.3 and v2.0/v2.1.
- Server binary with upstream-style flags.
- Conformance harness (`conformance/`), Terragrunt runtime-test VM, RPM/DEB
  packaging via nfpm.

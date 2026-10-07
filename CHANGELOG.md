# Changelog

## [Unreleased]

### 2026-10-06
- **feat:** must-gather collector `gather/cadvisor/status.sh` added to stormcos_qa (391cf16, #5). It covers cadvisor's API and self-metrics, stormd's view of the process and its log, and the node's cgroup and block trees. It was verified under sc-build against a live cadvisor-rs. README notes it.
- **docs:** filed #22 (v0.49.2 conformance not re-run since 2026-07-16) from the comment-mining pass; additions to rustkube-node#84 and stormcentral#134.
- **feat:** `-env_metadata_whitelist` is honoured (#10). For containerd containers, `process.env` is read from the OCI spec, and every variable whose key starts with a whitelisted prefix (upstream's rule) fills `spec.envs` (API) and `container_env_<key>` (`/metrics`). As upstream, env labels are left out when `-store_container_labels=false`, and CRI-O containers get none (its inspect has no env). Tests: prefix matching, OCI spec parsing, label gating.
- **docs:** README env metadata section and flag table; presentation and CLAUDE.md drop #10.
- **docs:** argv, port and the golden recipe come from stormcentral's component registry, which lives in its database (stormcentral#185). `components/stormcos.toml` is only the seed, and service goldens are built by stormcentral, not stormcos's `build-goldens.sh` (#20, stormcos#65). Updated README ports and "How it ships", CLAUDE.md, the presentation and the `test/src/env.rs` comment. Values checked against `stormcentral component export` (unchanged: 9096, `/healthz`, `--port 9096 --listen-ip 0.0.0.0`).
- **chore:** Makefile no longer builds as `root@dev.g8.lo` (#11). Removed `REMOTE`, `sync`, `test-linux`, `build-linux` and the `fixtures` stub. `package` now does a local release build and nfpm `.rpm` + `.deb` (runnable as `sc-build 'make package'`). `conformance` runs `conformance/diff-metrics.sh` and `diff-api.py`. `diff-metrics.sh` writes its scratch files to a private `mktemp -d` under `$TMPDIR` instead of fixed `/tmp` paths.
- **docs:** README Makefile note; presentation and CLAUDE.md drop #11.
- **fix:** Machine info is refreshed (#18). New flag `-update_machine_info_interval` (alias `--update-machine-info-interval`, default `5m0s`, as upstream) re-reads it, and the disk map is re-scanned as soon as an `io.stat` `major:minor` is missing from it, once per new device. Hot-attached volumes (stormblock ublk / nvme-tcp) now get `device="/dev/<name>"`, so they are no longer collapsed into one `device=""` series and dropped. Tests: fixture `/sys/block` gaining devices, unmapped-key detection, flag parsing.
- **docs:** README machine-info section and flag table; presentation drops #18 from planned work and the issue table.
- **docs:** CLAUDE.md work plan: #15 (per-VM stats) checked. It is blocked on rustkube-node#84 (identity), with #18 (disk) and stormvm#16 (network, SLIRP has no interface) as further prerequisites.
- **fix:** Upstream flag names are accepted (#9): every flag's primary name is now upstream v0.49.2's (`-listen_ip`, `-housekeeping_interval`, `-store_container_labels`, `-containerd-namespace`, …) with the other spelling (`--listen-ip`, `--containerd_namespace`, `--tls_cert_file`) as an alias, so the stormcos golden's `--listen-ip` keeps working. A bare boolean flag (`-store_container_labels`) means `true`, as in Go. Unit tests parse an upstream argv, the golden's argv and the defaults.
- **docs:** README flag table lists upstream spellings; `main.rs` module comment, `deploy/systemd/cadvisor.example`, presentation and CLAUDE.md no longer say underscore names are rejected.
- **docs:** CLAUDE.md work plan: #3 re-checked; its remaining work (stormpump pod identity) is blocked on rustkube-node#84.

### 2026-09-28
- **docs:** Refresh from the code since 2026-09-18 (third pass): README says machine info is read once at startup (#18), updates the #12 node-run status and notes the suites speak plain HTTP (#19); presentation issue table and planned slide updated (#4 done, #18, #19); CLAUDE.md work plan.
- **docs:** Issue validation pass: #4 closed (72 tests incl. `tls_auth.rs` on d5e94bd); CLAUDE.md work plan updated.

### 2026-09-27
- **docs:** `CLAUDE.md` known gaps and open issues: #18 (machine info never
  refreshed), found while mining issue comments.
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

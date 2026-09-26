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
- 2026-09-26 — #7 docs rewritten from the code (README, CLAUDE.md,
  CHANGELOG.md, module comments); also covers #6 (golden shipping documented,
  linking `stormcos/docs/goldens.md` — the `stormpump/docs/goldens.md` path
  named in #6 does not exist). Verified with `sc-build 'cargo build && cargo
  test && cargo run -q -p cadvisor -- --help'` on ccf57ba: exit 0, 64 tests
  passed, 0 failed; `--help` lists the same 15 flags with the same defaults as
  the README table. Gaps filed as #9, #10, #11 (and #4 already open).

### In progress
- 2026-09-26 — #8 presentation: Marp deck at `docs/presentation.md` (8–15
  slides: purpose, place in stormcos per stormcentral's relationships graph,
  architecture diagram, features from the code, planned work, interfaces,
  shipping/operation, status). Next after: #12 (P1 test containers).

### Known gaps (code does not do what upstream/docs imply) — tracked as issues
- Upstream underscore flag names (`-listen_ip`, `-housekeeping_interval`, …)
  are rejected: clap derives kebab-case (`--listen-ip`). See the issue filed
  from #7.
- `--env-metadata-whitelist` is parsed and ignored.
- Makefile `sync`/`test-linux`/`build-linux`/`package` rsync to
  `root@dev.g8.lo` — predates sc-build; do not use them from stormcentral
  sessions.
- #4 TLS / bearer-token auth — not implemented; the server is plain HTTP.

### Open issues (2026-09-26)
- #3 container discovery via CRI + cgroups — largely done in-tree (raw cgroup
  discovery + containerd/CRI-O enrichment); pod/namespace labels come from
  runtime labels, not CRI sandbox calls.
- #4 TLS and bearer-token auth — not started.
- #5 QA tests + must-gather collectors in stormcos_qa — not started.
- #8 presentation of purpose and functionality — not started.

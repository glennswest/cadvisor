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

### In progress
- 2026-09-24 — #7 docs rewritten from the code: README (flags, endpoints,
  ports, build via sc-build, shipping as a golden), CLAUDE.md created,
  CHANGELOG.md created, stale "deferred (M6/M7/M8)" module comments fixed.
  Also covers #6 (repo ships in a golden and did not say so).
  Status: docs pushed (3fb2fa9). `sc-build` compiled 3fb2fa9 cleanly (exit
  code not captured); the re-run to confirm `cargo test` and check `--help`
  against the README flag table was stopped by the local host (low memory)
  before output. Next: re-run `sc-build`, then close #7/#6 and request the golden.

### Known gaps (code does not do what upstream/docs imply) — tracked as issues
- Upstream underscore flag names (`-listen_ip`, `-housekeeping_interval`, …)
  are rejected: clap derives kebab-case (`--listen-ip`). See the issue filed
  from #7.
- `--env-metadata-whitelist` is parsed and ignored.
- Makefile `sync`/`test-linux`/`build-linux`/`package` rsync to
  `root@dev.g8.lo` — predates sc-build; do not use them from stormcentral
  sessions.
- #4 TLS / bearer-token auth — not implemented; the server is plain HTTP.

### Open issues (2026-09-24)
- #3 container discovery via CRI + cgroups — largely done in-tree (raw cgroup
  discovery + containerd/CRI-O enrichment); pod/namespace labels come from
  runtime labels, not CRI sandbox calls.
- #4 TLS and bearer-token auth — not started.
- #5 QA tests + must-gather collectors in stormcos_qa — not started.
- #8 presentation of purpose and functionality — not started.

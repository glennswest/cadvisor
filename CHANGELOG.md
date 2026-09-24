# Changelog

## [Unreleased]

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

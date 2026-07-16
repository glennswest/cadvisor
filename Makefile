# cadvisor-rs build/test targets.
#
# Local (macOS): unit tests only — all parsers are &str-in and run anywhere.
# Linux work happens on dev.g8.lo (Fedora 43, cgroup v2, podman, rust toolchain):
# the repo is rsynced to /root/cadvisor and built natively there.

REMOTE      ?= root@dev.g8.lo
REMOTE_DIR  ?= /root/cadvisor
CADVISOR_IMG ?= gcr.io/cadvisor/cadvisor:v0.49.2

.PHONY: test sync test-linux build-linux fixtures conformance

test:
	cargo test

sync:
	rsync -a --delete --exclude target --exclude .git ./ $(REMOTE):$(REMOTE_DIR)/

test-linux: sync
	ssh $(REMOTE) 'cd $(REMOTE_DIR) && cargo test'

build-linux: sync
	ssh $(REMOTE) 'cd $(REMOTE_DIR) && cargo build --release'

# Re-capture golden fixtures from a real cadvisor on the dev host.
# See conformance/capture-fixtures.sh (added with the conformance harness, M4).
fixtures:
	@echo "see crates/cadvisor-model/fixtures/ — captured from $(CADVISOR_IMG) on $(REMOTE)"

conformance:
	@echo "conformance harness lands in M4"

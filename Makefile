# cadvisor-rs build/test targets. Every target runs where it is invoked.
#
# From a stormcentral session, run them on the build box through sc-build,
# which builds the pushed commit (there is no checkout on dev.g8.lo):
#   sc-build                       cargo build && cargo test
#   sc-build 'make package'        .rpm + .deb into dist/ (needs nfpm)
# Unit tests run anywhere (`make test`); the binary itself is Linux-only.

CADVISOR_IMG ?= gcr.io/cadvisor/cadvisor:v0.49.2

.PHONY: test conformance package

test:
	cargo test

# Structural diff of a real cadvisor ($(CADVISOR_IMG)) against cadvisor-rs,
# both already running on the same host: REF on :18080, OURS on :18081
# (override with REF_URL/OURS_URL and REF_BASE/OURS_BASE).
conformance:
	conformance/diff-metrics.sh
	python3 conformance/diff-api.py

# .rpm + .deb via nfpm (rustkube/fastetcd convention).
VERSION  := $(shell grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
PKG_ARCH ?= amd64
package:
	cargo build --release
	mkdir -p dist tmp
	# The target dir is wherever cargo puts it (sc-build moves it off ./target).
	bin_dir=$$(cargo metadata --format-version 1 --no-deps | \
	    sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')/release && \
	sed -e 's|$${PKG_ARCH}|$(PKG_ARCH)|g' -e 's|$${PKG_VERSION}|$(VERSION)|g' \
	    -e "s|\$${BIN_DIR}|$$bin_dir|g" deploy/packaging/nfpm.yaml > tmp/nfpm-resolved.yaml
	grep -n 'cadvisor"$$' tmp/nfpm-resolved.yaml
	nfpm package -f tmp/nfpm-resolved.yaml -p rpm -t dist/
	nfpm package -f tmp/nfpm-resolved.yaml -p deb -t dist/
	ls -la dist/

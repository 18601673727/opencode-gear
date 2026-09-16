# OpenCode Gear - development targets
#
# `make test` runs the Rust test suite and the installer shell tests.
# `make check` runs the full gate: formatting, clippy with warnings denied, tests.
# `make validate` validates the shipped configuration.
# `make package` builds the current platform artifact and SHA256SUMS locally.

CARGO ?= cargo

.PHONY: all build test test-rust test-installer check fmt fmt-check clippy validate package clean

all: check

build:
	$(CARGO) build

test: test-rust test-installer

test-rust:
	$(CARGO) test

test-installer:
	sh tests/installer_test.sh

check: fmt-check clippy test

fmt:
	$(CARGO) fmt

fmt-check:
	$(CARGO) fmt --check

clippy:
	$(CARGO) clippy --all-targets --all-features -- -D warnings

validate:
	$(CARGO) run --quiet -- validate

package:
	sh scripts/package-release.sh

clean:
	$(CARGO) clean

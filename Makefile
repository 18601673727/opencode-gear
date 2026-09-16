# OpenCode Gear - development targets
#
# `make test` runs the Rust test suite.
# `make check` runs the full gate: formatting, clippy with warnings denied, tests.
# `make validate` validates the shipped configuration.

CARGO ?= cargo

.PHONY: all build test check fmt fmt-check clippy validate clean

all: check

build:
	$(CARGO) build

test:
	$(CARGO) test

check: fmt-check clippy test

fmt:
	$(CARGO) fmt

fmt-check:
	$(CARGO) fmt --check

clippy:
	$(CARGO) clippy --all-targets -- -D warnings

validate:
	$(CARGO) run --quiet -- validate

clean:
	$(CARGO) clean

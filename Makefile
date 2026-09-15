# OpenCode Gear - development targets
#
# `make test` runs the unit tests and the CLI smoke tests.
# `make validate` validates the shipped configuration.

PYTHON ?= python3

.PHONY: test unit cli validate

test: unit cli

unit:
	$(PYTHON) -m unittest discover -s tests -v

cli:
	bash tests/test_cli.sh

validate:
	$(PYTHON) bin/oc_config.py validate

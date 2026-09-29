# Targets grow as phases land (bench/analysis/backtest come with benches, python/).
MSG    ?= data/raw/message.csv
REF    ?= data/raw/orderbook.csv
MINE   ?= data/processed/orderbook_reconstructed.csv
LEVELS ?=
SYN    := data/synthetic

.PHONY: test reconstruct validate selftest

test:
	cargo test --workspace

reconstruct:
	@mkdir -p $(dir $(MINE))
	cargo run --release -q -p lob-cli -- $(MSG) $(REF) $(MINE)

VENV   := .venv
PYTHON := $(VENV)/bin/python

validate:
	$(PYTHON) scripts/validate_book.py $(REF) data/processed/orderbook_reconstructed.csv $(LEVELS)

# End-to-end check against an independent naive reference book (no real data needed).
selftest:
	python scripts/synthetic_check.py $(SYN)
	cargo run --release -q -p lob-cli -- $(SYN)/message.csv $(SYN)/orderbook.csv $(SYN)/reconstructed.csv
	python scripts/validate_book.py $(SYN)/orderbook.csv $(SYN)/reconstructed.csv



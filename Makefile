# mattermost-rs — a native Linux Mattermost client.
#
# Run `make` or `make help` for the available targets.

CARGO ?= cargo
BIN   := mattermost-rs

# Homebrew's 403 .pc files shadow the system ones and report GTK 4.22 where
# this machine actually runs apt's 4.14 -- the same version a GitHub
# ubuntu-24.04 runner has. Exported here as well as in .cargo/config.toml so
# that pkg-config invoked from a recipe agrees with pkg-config invoked from a
# build script. `make doctor` asserts the result.
export PKG_CONFIG_LIBDIR := /usr/lib/x86_64-linux-gnu/pkgconfig:/usr/share/pkgconfig
export PKG_CONFIG_PATH   :=

# The 100% line-coverage gate applies to these and only these. The list is
# duplicated in xtask/src/main.rs, which fails the build if a workspace member
# appears in neither list -- so the duplication cannot drift silently.
GATED := -p mm-api -p mm-core -p mm-md -p mm-config -p mm-desktop

# Excluded from the report rather than from the gate:
#   tests|benches|examples  a helper only one test calls reads as uncovered
#   unreachable.rs          the single sanctioned home for genuinely
#                           unreachable branches; its line count is a metric
#   /fakes/                 requiring 100% coverage OF the fakes is circular
COV_IGNORE := '(^|/)(tests|benches|examples)/|unreachable\.rs|/fakes/'

BOLD := $(shell test -t 1 && tput bold 2>/dev/null)
DIM  := $(shell test -t 1 && tput dim 2>/dev/null)
OFF  := $(shell test -t 1 && tput sgr0 2>/dev/null)

.DEFAULT_GOAL := help

.PHONY: help
help: ## Show this help
	@echo "$(BOLD)mattermost-rs$(OFF) — native Linux Mattermost client"
	@echo
	@grep -hE '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) \
		| awk 'BEGIN {FS = ":.*?## "}; {printf "  $(BOLD)%-16s$(OFF) %s\n", $$1, $$2}'
	@echo
	@echo "$(DIM)Variables: CARGO, TAG$(OFF)"

# ---------------------------------------------------------------- build

.PHONY: build
build: ## Debug build of the whole workspace
	$(CARGO) build --workspace

.PHONY: release
release: ## Optimised build of the client binary
	$(CARGO) build --release --locked --bin $(BIN)

.PHONY: run
run: ## Run the client from the build tree
	$(CARGO) run --bin $(BIN)

.PHONY: install
install: ## Install the binary into ~/.cargo/bin
	$(CARGO) install --path . --locked

.PHONY: clean
clean: ## Remove build artefacts
	$(CARGO) clean

# ---------------------------------------------------------------- quality

.PHONY: fmt
fmt: ## Format the source
	$(CARGO) fmt --all

.PHONY: fmt-check
fmt-check: ## Fail if the source is not formatted
	$(CARGO) fmt --all --check

.PHONY: clippy
clippy: ## Lint the whole workspace, warnings are errors
	$(CARGO) clippy --workspace --all-targets --all-features --locked -- -D warnings

.PHONY: test
test: ## Run the unit and property tests (no Docker, no lab)
	$(CARGO) test --workspace --all-features --locked

.PHONY: test-doc
test-doc: ## Run the doctests (they do NOT count toward the coverage gate)
	$(CARGO) test --workspace --doc --locked

.PHONY: check-deps
check-deps: ## Assert every member is classified and no gated crate can reach I/O
	$(CARGO) run -q -p xtask -- check-deps

.PHONY: check
check: fmt-check clippy test check-deps cover-gate ## Everything CI's check job runs

.PHONY: audit
audit: ## Check dependencies for advisories (needs cargo-audit)
	@command -v cargo-audit >/dev/null 2>&1 \
		|| { echo "cargo-audit not installed: cargo install cargo-audit"; exit 1; }
	$(CARGO) audit

.PHONY: version-check
version-check: ## Assert TAG=vX.Y.Z matches the version in Cargo.toml
	@test -n "$(TAG)" || { echo "usage: make version-check TAG=v0.1.0"; exit 1; }
	@v=$$($(CARGO) metadata --no-deps --format-version 1 \
		| sed -n 's/.*"name":"mattermost-rs","version":"\([^"]*\)".*/\1/p'); \
	 test "v$$v" = "$(TAG)" \
	   || { echo "tag $(TAG) does not match Cargo.toml version v$$v"; exit 1; }; \
	 echo "$(TAG) matches Cargo.toml"

# ---------------------------------------------------------------- coverage

.PHONY: cover-gate
cover-gate: ## 100% line coverage on the gated crates, or fail
	@command -v cargo-llvm-cov >/dev/null 2>&1 \
		|| { echo "cargo-llvm-cov not installed: cargo install cargo-llvm-cov"; exit 1; }
	@# A stale .profraw set from a differently-configured run both inflates and
	@# corrupts the merge. This is where go-tun's `-count=1` paranoia belongs in
	@# Rust: cargo does not cache test results, but it does keep profile data.
	$(CARGO) llvm-cov clean --workspace
	@# Gated on LINES, deliberately, and on nothing else:
	@#
	@#   --fail-under-regions  cannot reach 100%. A region is also emitted for
	@#     code that only runs when an assertion fails, so the format argument
	@#     in `assert_eq!(a, b, "{raw}")` is a permanently-missed region. The
	@#     price of 100% regions is assertions with no failure messages.
	@#   --fail-under-file-lines  is broken in cargo-llvm-cov 0.9.1. It exits 1
	@#     even when every file in the report is at 100.00% lines -- verified
	@#     against a single-file report -- and it prints nothing when it does.
	@#     Re-test it on a later version before reaching for it again; the
	@#     attraction was that it names the offending file.
	@#
	@# --show-missing-lines covers that need instead: it prints the uncovered
	@# line numbers, so a red gate is actionable from the job log alone.
	$(CARGO) llvm-cov --locked --all-features $(GATED) \
		--ignore-filename-regex $(COV_IGNORE) \
		--show-missing-lines \
		--fail-under-lines 100

.PHONY: cover-report
cover-report: ## HTML coverage for the whole workspace (informational, never gates)
	$(CARGO) llvm-cov clean --workspace
	$(CARGO) llvm-cov --locked --all-features --workspace --html

.PHONY: cover-open
cover-open: cover-report ## cover-report, then open it
	xdg-open target/llvm-cov/html/index.html

.PHONY: cover-missing
cover-missing: ## List the uncovered lines in the gated crates
	$(CARGO) llvm-cov clean --workspace
	$(CARGO) llvm-cov --locked --all-features $(GATED) \
		--ignore-filename-regex $(COV_IGNORE) --show-missing-lines

# ---------------------------------------------------------------- environment

.PHONY: apt-deps
apt-deps: ## Install the system build dependencies (needs sudo)
	./scripts/apt-deps.sh

.PHONY: doctor
doctor: ## Check the build environment, especially which GTK pkg-config finds
	@./scripts/doctor.sh

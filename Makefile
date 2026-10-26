# mattermost-rs — a native Linux Mattermost client.
#
# Run `make` or `make help` for the available targets.
#
# Recipes here are one-line delegations. Anything with real logic in it lives
# in the xtask crate, in Rust, where it is compiled, typed and linted by
# `make check` like the rest of the tree -- rather than in shell scripts that
# nothing checks. `cargo xtask` with no arguments lists the tasks.

CARGO ?= cargo
BIN   := mattermost-rs
XTASK := $(CARGO) run -q -p xtask --

# Homebrew's several hundred .pc files shadow the system ones and report GTK
# 4.22 where this machine actually runs apt's 4.14 -- the same version a GitHub
# ubuntu-24.04 runner has. Exported here as well as in .cargo/config.toml so
# that pkg-config invoked from a recipe agrees with pkg-config invoked from a
# build script. `make doctor` asserts the result.
export PKG_CONFIG_LIBDIR := /usr/lib/x86_64-linux-gnu/pkgconfig:/usr/share/pkgconfig
export PKG_CONFIG_PATH   :=

# The 100% line-coverage gate applies to these and only these. The list is
# duplicated in xtask/src/deps.rs, which fails the build if a workspace member
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
	@echo "$(DIM)Variables: CARGO, TAG, OUT$(OFF)"

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

OUT ?= dist

.PHONY: package
package: release ## Strip, smoke-test and checksum the release binary into $(OUT)
	$(XTASK) package --out $(OUT)

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
	$(XTASK) check-deps

.PHONY: check
check: fmt-check clippy test check-deps cover-gate ## Fast local checks (no network)

.PHONY: ci
ci: check audit ## Exactly what CI runs — use this before pushing
	@# `check` deliberately leaves `audit` out: it needs the network and a
	@# tool that is not part of the toolchain, so `make check` still works on
	@# a train. But that split is a trap as well as a convenience -- a green
	@# `check` does not mean a green CI, which is exactly how an
	@# unmaintained-dependency advisory landed on main.
	@echo "ci: everything CI runs has passed"

.PHONY: audit
audit: ## Check dependencies for advisories (needs cargo-audit)
	@# --deny warnings on purpose: a new advisory anywhere in the tree should
	@# turn the build red while somebody decides. Assessed ones are recorded
	@# in .cargo/audit.toml with the reasoning, rather than weakening this.
	@command -v cargo-audit >/dev/null 2>&1 \
		|| { echo "cargo-audit is not installed: cargo install cargo-audit"; exit 1; }
	$(CARGO) audit --deny warnings

.PHONY: version-check
version-check: ## Assert TAG=vX.Y.Z matches the version in Cargo.toml
	$(XTASK) version-check --tag $(TAG)

# ---------------------------------------------------------------- coverage

.PHONY: cover-gate
cover-gate: ## 100% line coverage on the gated crates, or name the gaps
	@# The verdict comes from cargo-llvm-cov's JSON export, computed in xtask,
	@# NOT from its --fail-under-* flags. Two accounting bugs in 0.9.1 forced
	@# that: --fail-under-file-lines exits 1 even when every file reports
	@# 100.00%, and the summary table claimed a missed line in a file whose
	@# JSON export and generated per-line HTML both showed fully covered.
	@# The JSON is the data the HTML renders from, so reading it is reading the
	@# measurement rather than a derived summary -- and it lets the gate print
	@# the offending file, line and source text, which no flag does.
	$(XTASK) coverage

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
	$(XTASK) apt-deps

.PHONY: doctor
doctor: ## Check the build environment, especially which GTK pkg-config finds
	$(XTASK) doctor

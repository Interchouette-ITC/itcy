.PHONY: lint test run build clean tor-up tor-down linkedin-enrich check-license-headers apply-license-headers corpus-rebuild obscura-parity \
	coverage coverage-summary coverage-html audit deny machete outdated

# Paths filtered from llvm-cov / Codecov upload.
COVERAGE_IGNORE := examples/|benches/

check-license-headers:
	@if [ -f .cursor/scripts/check-license-headers.mjs ]; then \
		node .cursor/scripts/check-license-headers.mjs; \
	else \
		echo "skip license-header check (operator scripts not present)"; \
	fi

apply-license-headers:
	@if [ -f .cursor/scripts/apply-license-headers.mjs ]; then \
		node .cursor/scripts/apply-license-headers.mjs; \
	else \
		echo "skip apply-license-headers (operator scripts not present)"; \
	fi

lint: check-license-headers
	cd backend && cargo fmt --check
	cd backend && cargo clippy --workspace --all-targets -- \
		-D warnings -D clippy::all -D clippy::pedantic -D clippy::nursery

test:
	cd backend && cargo test --workspace
	node --test scripts/x-ship-resolve.test.mjs scripts/tweet-href-urls.test.mjs

## Requires cargo-llvm-cov + llvm-tools-preview. Writes coverage/lcov.info.
coverage:
	mkdir -p coverage
	cd backend && RUSTUP_TOOLCHAIN=stable cargo llvm-cov --workspace --locked --lcov \
		--ignore-filename-regex '$(COVERAGE_IGNORE)' \
		--output-path ../coverage/lcov.info

coverage-summary:
	cd backend && RUSTUP_TOOLCHAIN=stable cargo llvm-cov --workspace --locked --summary-only \
		--ignore-filename-regex '$(COVERAGE_IGNORE)'

coverage-html:
	mkdir -p coverage
	cd backend && RUSTUP_TOOLCHAIN=stable cargo llvm-cov --workspace --locked --html \
		--ignore-filename-regex '$(COVERAGE_IGNORE)' \
		--output-dir ../coverage/html

## Requires `cargo install cargo-audit`.
audit:
	cd backend && cargo audit

## Requires `cargo install cargo-deny` (config: backend/deny.toml).
deny:
	cd backend && cargo deny check

## Unused workspace dependencies. Requires `cargo install cargo-machete`.
machete:
	cd backend && cargo machete

## Outdated crates report. Requires `cargo install cargo-outdated`.
outdated:
	cd backend && cargo outdated --workspace

# Drop cargo build artifacts only. Does not touch sql/ or .env.
clean:
	cd backend && cargo clean

# RUST_LOG controls which levels appear (all levels share one stdout stream).
# Default for interactive `make run` if unset: itcy debug + quiet deps.
# Always clear the terminal first so screen attach shows a fresh log pane.
# Build then exec the binary (do not leave `cargo run` holding the target lock
# for the whole process lifetime - that blocks the next make run / lint).
run:
	@clear 2>/dev/null || true
	cd backend && cargo build -p itcy
	cd backend && RUST_LOG="$${RUST_LOG:-warn,itcy=debug}" ./target/debug/itcy

build:
	cd backend && cargo build -p itcy --release
	cd backend && cargo build -p linkedin-itcy-gr-mcp --release

# Wipe sources+chunks then re-import curated LinkedIn export (keeps Slack memory).
# Weekly curated refresh without wipe: restart itcy (merge+dedupe) or:
#   cargo run -p itcy --bin import-linkedin-export
corpus-rebuild:
	cd backend && ITCY_LINKEDIN_EXPORT_DIR="$${ITCY_LINKEDIN_EXPORT_DIR:-../linkedin-export}" \
		ITCY_STATE_DB="$${ITCY_STATE_DB:-../sql/runtime.db}" \
		cargo run -p itcy --bin import-linkedin-export -- --wipe

# Tor (SOCKS 9050 + control 9051). Set ITCY_TOR_BIN to the tor binary.
tor-up:
	bash scripts/tor-up.sh

tor-down:
	bash scripts/tor-down.sh

# Tor-up + enrich-linkedin-urls --loop (link-only post/repost stubs). Restart-safe via DB.
linkedin-enrich:
	bash scripts/linkedin-enrich.sh

# Lane C baseline vs ITCY_PW_BROWSER=obscura (manual; requires tools/obscura/obscura).
obscura-parity:
	bash scripts/obscura-parity-harness.sh

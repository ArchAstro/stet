.PHONY: all build build-release install test test-all fuzz bench clean release-check fmt lint check setup

# Default target
all: build

# Debug build
build:
	cargo build

# Release build
build-release:
	cargo build --release

# Install md to ~/.cargo/bin and link the Claude Code skill
install:
	sh scripts/install.sh

# Validate a release candidate. Tagging and pushing stay explicit human actions.
release-check:
	cargo fmt --all -- --check
	cargo clippy --locked --all-targets --all-features -- -D warnings
	RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps --all-features
	cargo test --locked --all-targets --all-features
	cargo audit

# Run all tests
test:
	cargo test

# Run the complete test suite.
test-all:
	cargo test --all-targets --all-features

# Soak the incremental markdown analysis against full parses.
fuzz:
	MD_FUZZ=50000 cargo test --release -p md-core incremental_analysis

# Hot-path timings on the example document
bench: build-release
	target/release/md --bench examples/tour.md

# Clean build artifacts
clean:
	cargo clean

# Format code
fmt:
	cargo fmt

# Lint
lint:
	cargo clippy --all-targets --all-features -- -D warnings

# Check (fast compile check without codegen)
check:
	cargo check

# Setup development environment (install git hooks)
setup:
	cp scripts/pre-commit .git/hooks/pre-commit
	chmod +x .git/hooks/pre-commit
	@echo "Git hooks installed"

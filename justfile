# =============================================================================
# Justfile for refranero
# =============================================================================
# Run `just --list` to see every recipe.

set shell := ["bash", "-uc"]

# List the available recipes
default:
    @just --list

# Format the Rust sources
fmt:
    cargo fmt --all

# Check the formatting without writing changes
fmt-check:
    cargo fmt --all -- --check

# Run clippy with warnings denied
clippy:
    cargo clippy --all-targets -- -D warnings

# Run the Rust test suite
test:
    cargo test --all-targets

# Build the release binary
build:
    cargo build --release

# Lint the Markdown files
lint-md:
    pnpm exec markdownlint-cli2

# Run the Node wrapper and npm packaging checks
npm-check:
    node --check npm/wrapper/bin/refranero.mjs
    node --check scripts/prepare-npm.mjs
    node --test tests/npm-packaging.test.mjs
    cd npm/wrapper && npm pack --dry-run

# Run the release tarball packaging checks
release-check:
    node --check scripts/prepare-release-tarball.mjs
    node --test tests/release-tarball.test.mjs

# Run every local quality gate
ci: fmt-check clippy test lint-md npm-check release-check

# Install the JavaScript tooling and the git hooks
install-hooks:
    pnpm install
    pnpm exec lefthook install

# Remove the build artifacts
clean:
    cargo clean

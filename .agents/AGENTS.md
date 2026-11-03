# Repository guidance

## Project shape
- This repository builds one Rust CLI (`refranero`, `../src/main.rs`); `domain.rs`, `store.rs`, `ingest.rs`, and `refresh.rs` hold domain types, SQLite persistence, remote import adapters, and automatic refresh policy.
- SQLite is bundled through `rusqlite`; the starter catalog is compiled into the binary from `../data/seed.json` with `include_str!` in `src/store.rs`. Keep the seed JSON aligned with its deserialization model.
- Network imports are explicit `sync` operations. Random reads use the local catalog and may schedule a background refresh unless `--offline` is set.
- `../npm/wrapper` contains the Node launcher and wrapper manifest; `npm/platform` is a template. `scripts/prepare-npm.mjs` generates the six native platform packages under ignored `npm/dist/`. The launcher dispatches to an installed optional native package; it does not download binaries at install time.

## Verification
- Rust CI order: `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`, then `cargo test --all-targets` (stable Rust; CI installs `clippy` and `rustfmt`).
- npm checks: `node --check npm/wrapper/bin/refranero.mjs`, `node --check scripts/prepare-npm.mjs`, `node --test tests/npm-packaging.test.mjs`, then `cd npm/wrapper && npm pack --dry-run`.
- There is no root npm workspace or package-level JS test script; use the explicit Node commands above for wrapper/packaging changes.

## Local hooks and commands
- `pnpm install` installs the developer tooling (lefthook, commitlint, markdownlint-cli2) and activates the git hooks through the `prepare` script.
- Git hooks come from `lefthook.yml`: `pre-commit` runs `cargo fmt --all`, clippy, markdownlint, and the Node syntax checks; `pre-push` runs the ggshield secret scan, clippy, the Rust tests, and the npm packaging checks; `commit-msg` runs commitlint; the `post-*` hooks re-apply the AgentSync configuration.
- `just --list` lists the local recipes; `just ci` runs every quality gate, and `make <target>` delegates to the same recipes.
- The clippy lint groups in `Cargo.toml` mirror `dallay/cortex`: `correctness` is denied, and `style`, `complexity`, `perf`, `nursery`, and `suspicious` are warnings. Keep the tree clean under `-D warnings`.
- `rust-toolchain.toml` pins the stable channel with the `rustfmt` and `clippy` components; `.rustfmt.toml` sets edition 2021, a width of 100 columns, and Unix newlines.

## Data and packaging constraints
- The README states that source content and compilations are not covered by the MIT license; do not distribute bulk-harvested databases in the repository or npm packages without rightsholder authorization.
- Native npm packages support macOS, Linux glibc, and Windows on x64/arm64. Linux musl is not a configured target.
- For package-preparation changes, pass the intended Rust target and version to `../scripts/prepare-npm.mjs`; its platform mapping is the executable source of truth. Avoid hand-editing generated `npm/dist/` output.

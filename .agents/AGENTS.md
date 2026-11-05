# Repository guidance

## Project shape
- One Rust CLI binary: `src/main.rs` orchestrates; `domain.rs`, `store.rs`, `ingest.rs`, and `refresh.rs` contain domain types, SQLite persistence, source adapters, and refresh policy.
- SQLite uses bundled `rusqlite`. `src/store.rs` compiles in `data/seed.json`; keep seed entries compatible with `NewSaying`.
- Random reads use the local catalog and can start a background refresh unless `--offline` is set. Network imports are explicit `sync` commands.
- `npm/wrapper` is the Node launcher; `npm/platform` is a template. `scripts/prepare-npm.mjs` generates six native packages in ignored `npm/dist/`. Do not hand-edit generated output or assume installation downloads binaries.

## Commands
- `just --list` shows recipes; `just ci` runs all local gates. `make <target>` delegates to `just`.
- Rust CI order: `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all-targets` (stable toolchain with `rustfmt` and `clippy`).
- Focused recipes: `just fmt-check`, `just clippy`, `just test`, `just npm-check`, and `just release-check`.
- JavaScript has no root test script. `just npm-check` verifies the launcher and npm generator, tests `tests/npm-packaging.test.mjs`, and dry-runs the wrapper package. `just release-check` checks and tests release tarball preparation.
- `pnpm install` installs the root tooling and installs Lefthook through `prepare`. See `lefthook.yml` for hooks; `pre-push` may run Rust tests and packaging checks.

## Packaging and data constraints
- `scripts/prepare-npm.mjs` maps Rust targets to the six supported packages (macOS, Linux glibc, Windows; x64/arm64). Linux musl is not supported. Pass the intended Rust target, binary path, and version to the script; do not edit `npm/dist/`.
- The MIT license covers source code, not third-party source content or compilations. Do not distribute bulk-harvested databases in the repository or npm packages without rightsholder authorization; see the README for provenance and terms.

## Agent instructions
- Root `AGENTS.md` and other tool entrypoints are generated symlinks to this tracked file by AgentSync. Edit `.agents/AGENTS.md`, not the generated links.

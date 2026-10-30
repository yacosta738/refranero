# Refranero (Rust CLI)

Offline-first Rust command-line reader for traditional Spanish sayings, with optional imports from the **Biblioteca Virtual Miguel de Cervantes** and the historical **Centro Virtual Cervantes** via the **Wayback Machine**.

> **Status: prototype, not yet published to npm.** The runtime has local test coverage and the current-portal import has been exercised against the live site. Online import still depends on external websites' availability and HTML structures.

## Releases

Release Please tracks Conventional Commits on `main` and opens or updates a release pull request. After approval and merge, it creates the version tag and GitHub Release. As in AgentSync, the workflow uses a GitHub App with the repository secrets `APP_ID` and `APP_PRIVATE_KEY`, so its PR, tag, and release events can start other workflows. Configure those secrets before enabling releases. The workflow does not publish npm packages or binaries.

## Quick start

Rust toolchain (1.85+) is required to build from source:

```sh
cargo run --
cargo run -- --count 3
cargo run -- --json
cargo run -- --lang en
cargo run -- open
cargo run -- stats
```

After the npm packages are published, install and run the CLI with:

```sh
npm install -g @yacosta738/refranero
refranero
refranero --count 3
refranero --browser
refranero open 1
refranero stats
```

## Import more local data (explicit network access)

The default invocation selects only `refran`, so other categories are always opt-in:

```sh
refranero                    # One Spanish refrán
refranero --count 5          # Five random Spanish refranes
refranero --category adivinanza
refranero --category proverbio
refranero --lang en          # Limited verified English seed (not automatic translation)
refranero --browser          # Show and open the selected source page
refranero open 1             # Open an exact catalog ID
```

Catalog maintenance also supports offline JSON arrays with a source URL and explicit provenance:

```sh
refranero import data/import-example.json
```


Import a small portion of current category pages:

```sh
refranero sync virtual --kind refran --limit 20
refranero sync virtual --kind proverbio --limit 20
refranero sync virtual --kind adivinanza --limit 20
```

Import a historical **index of Spanish paremias**, discovered using the Wayback availability API. `--year` selects an approximate capture year. This phase extracts linked Spanish entries, **not multilingual translations**:

```sh
refranero sync wayback --letter A --year 2020 --limit 20
refranero --category paremia
```

Manual `sync` commands download records when requested. A normal random invocation first reads and prints from SQLite; if no successful full portal refresh exists, or the last one was more than 24 hours ago, it launches a detached background worker and exits without waiting for the network. The worker fetches the five current-portal categories and atomically replaces only the `cervantes_virtual` snapshot. CVC/Wayback records remain untouched. A SQLite lease prevents duplicate workers and expires after five minutes if a worker is interrupted. Worker output is appended to a `catalog.refresh.log` file next to the database. Worker failure leaves the cache intact and releases the lease; the next normal invocation can retry. Each HTTP request has a 5-second timeout; the portal's full refrain page currently exceeds 2 MiB, so each response is bounded at 8 MiB. A valid non-empty snapshot is required; on request, parse, or validation failure the previous snapshot and refresh timestamp remain unchanged. Use `refranero --offline` to skip refresh scheduling. `stats`, `open`, and explicit category-limited `sync` do not trigger automatic refresh. The default path is the system's per-user local data directory (`refranero/catalog.sqlite3`). Change it via `--db path/to/catalog.sqlite3`.

The program seeds a **small starter dataset of 16 sayings** from category pages and a few CVC fichas. Current-portal entries link to the **category page** containing the entry; the CVC seed entries have individual ficha URLs. Some CVC URLs may temporarily fail during site maintenance. Imports are idempotent by `(text, category, language, source)`. Historical captures record `original_url`, `captured_at`, `group_id=cvc:Par` and a Wayback replay lookup URL. **A replay lookup does not guarantee the individual ficha was archived.** The indexed Wayback import currently stores *only alphabetical-list links*, not their full historical ficha HTML or translations; extracting those is a future work item.

### Known limitations

- The starter dataset includes 14 Spanish sayings plus one English (`--lang en`) and one Catalan (`--lang ca`) CVC correspondence, both under `group_id=cvc:59530`. Other languages have no starter entries. No translations are invented.
- The CVC site has indexed multilingual fichas, but direct access is intermittently unavailable; the archive lookup API and captured pages were not reachable from the authoring environment. Historical scraping code has only synthetic HTML fixtures, **not an end-to-end capture test**.
- The Cervantes Virtual adapter currently extracts `<blockquote>` text and links to each category page. It deliberately does **not** claim to extract and validate each entry's exact bibliographic URL, meaning or riddle answer.
- The old CVC alphabetical listing covers paremias of different subtypes; they are classified as `paremia` until individual fichas are parsed.
- `sync` is intentionally limited to 50 entries per page/letter and is not a bulk crawling engine. Respect robots restrictions, site rate limits and legal terms.
- No automatic deduplication *across sources*, as otherwise provenance would be lost.

## Architecture

```text
src/
  main.rs       CLI and orchestration
  domain.rs     Category, NewSaying and Saying
  store.rs      SQLite catalog repository with validation
  ingest.rs     Opt-in current-portal and historical-Wayback adapters

data/seed.json    Small offline bootstrap catalog
data/import-example.json  JSON format example
npm/wrapper/      Cross-platform npm command (no postinstall HTTP downloads)
npm/platform/     Template for native platform npm packages
scripts/          Prepare platform packages from release binaries
.github/workflows/ci.yml
```

The **binary is native Rust**. npm provides only a Node launcher that selects a prebuilt package for the platform and forwards arguments/exit codes. The planned package matrix is `darwin-{arm64,x64}`, `linux-{arm64,x64}` (glibc), and `win32-{arm64,x64}`. Linux musl variants and additional hosts require separate handling in later releases.

## Distribution preparation

Build a native binary and stage its platform package locally:

```sh
cargo build --release --target aarch64-apple-darwin
node scripts/prepare-npm.mjs aarch64-apple-darwin target/aarch64-apple-darwin/release/refranero 0.1.0
cd npm/dist/darwin-arm64 && npm pack --dry-run
```

The supported package targets are `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `x86_64-apple-darwin`, `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`, and `aarch64-pc-windows-msvc`. The CI workflow builds these six targets with a platform matrix on native GitHub-hosted runners and runs package-generation tests.

Before an npm release, build and test every native package, publish all six `@yacosta738/refranero-<platform>` packages first, then publish `@yacosta738/refranero`. Keep versions aligned across `Cargo.toml`, platform packages, and wrapper optional dependencies. The workflow does not publish packages automatically. npm credentials and package namespace access must be confirmed before adding a gated release workflow. Do not publish or advertise `@yacosta738/refranero` as available yet.

## Data provenance, legal boundaries and attribution

- [CVC Refranero Multilingüe](https://cvc.cervantes.es/lengua/refranero/) — different project to the Biblioteca Virtual Miguel de Cervantes.
- [Biblioteca Virtual Miguel de Cervantes — Refranes](https://www.cervantesvirtual.com/portales/refranes_dichos_proverbios_citas_adivinanzas/refran/).
- [Biblioteca Virtual Miguel de Cervantes — Terms](https://www.cervantesvirtual.com/marco-legal/).
- [Wayback Machine availability API](https://archive.org/wayback/available).

The source code is MIT licensed; this does **not** extend a license to third-party page content or compilations. The Biblioteca Virtual Miguel de Cervantes's terms explicitly distinguish personal storage from redistributing their content and claim protection of their databases. Historical availability on archive.org does not remove original copyright or database rights. **Do not distribute a bulk harvested database with an npm package or a Git repository without the rightsholder's authorization.** Source references and a small common-tradition bootstrap are included instead. Users are responsible for permitted private imports.

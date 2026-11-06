# Refranero

Refranero is an offline-first command-line reader for Spanish proverbs (*refranes*) and related
sayings. It stores its catalog in a local SQLite database and can refresh entries from the
Biblioteca Virtual Miguel de Cervantes and historical Centro Virtual Cervantes records in the
Wayback Machine.

## Requirements

- Node.js 18 or newer
- macOS, Windows, or Linux with glibc on a supported x64 or arm64 platform

## Run without installing

Use `npx`:

```sh
npx --yes @yacosta738/refranero@latest
```

Or use `pnpm`:

```sh
pnpm dlx @yacosta738/refranero@latest
```

In an interactive terminal, Refranero opens a reader with a random saying. To print one saying and
exit, use `--plain`:

```sh
npx --yes @yacosta738/refranero@latest --plain
```

## Common commands

```sh
refranero                  # Open the interactive reader
refranero --offline        # Read from the local catalog without refreshing online
refranero --count 3        # Print three random sayings
refranero --json           # Print JSON output
refranero stats            # Show catalog entry counts
```

The reader supports keyboard navigation, language and category selection, and opening a saying's
source in a browser. See the [project README](https://github.com/yacosta738/refranero#readme) for
the full command reference
and [data provenance and terms](https://github.com/yacosta738/refranero#data-provenance-legal-boundaries-and-attribution).

## License

The Refranero source code is available under the MIT License. Third-party source content and
compilations may have separate terms; review
the [project's source and data terms](https://github.com/yacosta738/refranero#data-provenance-legal-boundaries-and-attribution)
before redistributing catalog data.

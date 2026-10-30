//! Refranero command-line interface with a local-first cache and background refresh.
mod domain;
mod ingest;
mod refresh;
mod store;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use domain::Category;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use store::Store;

#[derive(Debug, Parser)]
#[command(
    name = "refranero",
    version,
    about = "Refranes, proverbios y paremias en tu terminal"
)]
struct Cli {
    /// SQLite catalog path. Defaults to the per-user data directory.
    #[arg(long, global = true)]
    db: Option<PathBuf>,
    /// Language code (for example es, en or ca). The starter catalog is mostly Spanish.
    #[arg(long, global = true, default_value = "es")]
    lang: String,
    /// Limit results to one paremiological category.
    #[arg(long, global = true, value_enum)]
    category: Option<Category>,
    /// Number of random items to print (maximum 20).
    #[arg(long, global = true, default_value_t = 1)]
    count: usize,
    /// Print machine-readable JSON instead of terminal-friendly text.
    #[arg(long, global = true)]
    json: bool,
    /// Skip automatic online refresh and use only the local catalog.
    #[arg(long, global = true)]
    offline: bool,
    /// Open the selected item's source URL in the default web browser.
    #[arg(long, global = true)]
    browser: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Show random sayings (default command).
    Random,
    /// Open one entry by ID, or a random entry when omitted.
    Open { id: Option<i64> },
    /// Show how many entries are stored by source, language and category.
    Stats,
    /// Import validated records from a locally prepared JSON file.
    Import {
        /// JSON array of saying records (see data/import-example.json).
        file: PathBuf,
    },
    /// Internal background refresh worker. Do not call directly.
    #[command(hide = true)]
    RefreshWorker {
        #[arg(long)]
        owner: String,
    },
    /// Explicitly import source pages to the local SQLite catalog.
    Sync {
        #[command(subcommand)]
        source: SyncSource,
    },
}

#[derive(Debug, Subcommand)]
enum SyncSource {
    /// Import quotations from Cervantes Virtual's five thematic pages.
    Virtual {
        /// Select one thematic page (defaults to all five).
        #[arg(long, value_enum)]
        kind: Option<Category>,
        /// Per-page limit; hard-capped to 50 per invocation.
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Import a historical CVC alphabetic index via Wayback Machine.
    Wayback {
        /// First letter of the index (A-Z).
        #[arg(long, default_value_t = 'A')]
        letter: char,
        /// Approximate year of the archival capture.
        #[arg(long, default_value_t = 2020)]
        year: u16,
        /// Maximum number of paremia links (1-50).
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let db = match cli.db.as_ref() {
        Some(path) => path.clone(),
        None => dirs::data_local_dir()
            .context("could not determine user data directory; pass --db <path>")?
            .join("refranero")
            .join("catalog.sqlite3"),
    };
    if let Some(Command::RefreshWorker { owner }) = cli.command.as_ref() {
        return run_refresh_worker(&db, owner);
    }
    let mut store = Store::open(&db)?;

    match cli.command.as_ref() {
        None | Some(Command::Random) => {
            let result = random(&cli, &store);
            if result.is_ok() {
                schedule_background_refresh(&cli, &db, &mut store);
            }
            result
        }
        Some(Command::Open { id }) => {
            let entry = match id {
                Some(id) => store
                    .get(*id)?
                    .with_context(|| format!("no entry with id {id}"))?,
                None => store
                    .random(
                        &cli.lang,
                        cli.category.map(Category::as_str).or(Some("refran")),
                        1,
                    )?
                    .into_iter()
                    .next()
                    .context("no matching sayings in the local catalog")?,
            };
            println!("{}\n{}", entry.entry.text, entry.entry.source_url);
            webbrowser::open(&entry.entry.source_url)
                .context("could not open the default web browser")?;
            Ok(())
        }
        Some(Command::Import { file }) => {
            let raw = std::fs::read_to_string(file)
                .with_context(|| format!("reading {}", file.display()))?;
            let entries: Vec<domain::NewSaying> = serde_json::from_str(&raw)
                .with_context(|| format!("parsing {}", file.display()))?;
            if entries.len() > 10_000 {
                bail!("a single JSON import cannot exceed 10,000 records");
            }
            let inserted = store.insert(&entries)?;
            println!(
                "{} entries processed, {} inserted; catalog total: {}",
                entries.len(),
                inserted,
                store.count()?
            );
            Ok(())
        }
        Some(Command::Stats) => {
            println!("SQLite: {} ({} entries)", db.display(), store.count()?);
            for (source, language, category, count) in store.summary()? {
                println!("{source:20} {language:5} {category:12} {count:5}");
            }
            Ok(())
        }
        Some(Command::Sync { source }) => sync(source, &mut store),
        Some(Command::RefreshWorker { .. }) => {
            unreachable!("worker command is handled before opening the catalog")
        }
    }
}

fn schedule_background_refresh(cli: &Cli, db: &std::path::Path, store: &mut Store) {
    if cli.offline {
        return;
    }
    let now = unix_timestamp();
    let last_refresh = match store.last_successful_refresh() {
        Ok(value) => value,
        Err(error) => {
            eprintln!("Warning: could not check catalog age: {error:#}");
            return;
        }
    };
    if refresh::refresh_decision(last_refresh, now, false) != refresh::RefreshDecision::Stale {
        return;
    }
    let owner = worker_owner();
    match store.try_acquire_refresh_lease(now, &owner) {
        Ok(true) => {}
        Ok(false) => return,
        Err(error) => {
            eprintln!("Warning: could not schedule catalog refresh: {error:#}");
            return;
        }
    }
    if let Err(error) = spawn_refresh_worker(db, &owner) {
        let _ = store.release_refresh_lease(&owner);
        eprintln!("Warning: could not start catalog refresh worker: {error:#}");
    }
}

fn worker_owner() -> String {
    format!("{}-{}", std::process::id(), unix_timestamp())
}

fn spawn_refresh_worker(db: &std::path::Path, owner: &str) -> Result<()> {
    let executable = std::env::current_exe().context("locating current executable")?;
    let log_path = db.with_extension("refresh.log");
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;
    let error_log = log.try_clone()?;
    let mut command = std::process::Command::new(executable);
    command
        .arg("--db")
        .arg(db)
        .arg("refresh-worker")
        .arg("--owner")
        .arg(owner)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log))
        .stderr(std::process::Stdio::from(error_log));
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    command.spawn().context("spawning refresh worker")?;
    Ok(())
}

fn run_refresh_worker(db: &std::path::Path, owner: &str) -> Result<()> {
    let mut store = Store::open(db)?;
    let mut snapshot = Vec::new();
    let refresh_result = (|| -> Result<()> {
        for category in Category::all() {
            let lease_time = unix_timestamp();
            if !store.renew_refresh_lease(lease_time, owner)? {
                bail!(
                    "background refresh lease expired before updating {}",
                    category.as_str()
                );
            }
            let client = ingest::http_client_with_timeout(std::time::Duration::from_secs(5))?;
            let entries = ingest::import_virtual(&client, category, 50)?;
            if entries.is_empty() {
                bail!(
                    "no entries parsed from portal category {}; keeping cached catalog",
                    category.as_str()
                );
            }
            snapshot.extend(entries);
        }
        store.replace_virtual_snapshot(&snapshot, unix_timestamp())?;
        let total = store.count_source("cervantes_virtual")?;
        eprintln!("Catalog refresh completed with {total} records.");
        Ok(())
    })();
    let release_result = store.release_refresh_lease(owner);
    refresh_result?;
    release_result?;
    Ok(())
}

fn unix_timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

fn random(cli: &Cli, store: &Store) -> Result<()> {
    if !(1..=20).contains(&cli.count) {
        bail!("--count must be between 1 and 20");
    }
    // Default experience deliberately shows refranes, not all imported categories.
    let category = cli.category.map(Category::as_str).or(Some("refran"));
    let sayings = store.random(&cli.lang, category, cli.count)?;
    if sayings.is_empty() {
        bail!(
            "no local entries for lang={} category={}; use `refranero sync ...` to import more",
            cli.lang,
            category.unwrap_or("refran")
        );
    }
    if cli.json {
        println!("{}", serde_json::to_string_pretty(&sayings)?);
    } else {
        for saying in &sayings {
            println!("\n“{}”", saying.entry.text);
            println!(
                "  [{} · {} · #{}]",
                saying.entry.category, saying.entry.source, saying.id
            );
            println!("  {}", saying.entry.source_url);
        }
    }
    if cli.browser {
        for saying in &sayings {
            webbrowser::open(&saying.entry.source_url)
                .context("could not open the default web browser")?;
        }
    }
    Ok(())
}

fn sync(source: &SyncSource, store: &mut Store) -> Result<()> {
    let client = ingest::http_client()?;
    let mut inserted = 0;
    match *source {
        SyncSource::Virtual { kind, limit } => {
            validate_import_limit(limit)?;
            let categories = kind.map_or_else(|| Category::all().to_vec(), |kind| vec![kind]);
            let refreshed_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs() as i64;
            let mut snapshot = Vec::new();
            for category in categories {
                let entries = ingest::import_virtual(&client, category, limit)?;
                if entries.is_empty() {
                    bail!("no entries parsed from virtual portal category: {} (site structure may have changed)", category.as_str());
                }
                if kind.is_none() {
                    snapshot.extend(entries);
                } else {
                    let created = store.insert(&entries)?;
                    println!(
                        "{}: {} parsed, {} new",
                        category.as_str(),
                        entries.len(),
                        created
                    );
                    inserted += created;
                }
                if kind.is_none() {
                    std::thread::sleep(std::time::Duration::from_millis(1200));
                }
            }
            if kind.is_none() {
                inserted = store.replace_virtual_snapshot(&snapshot, refreshed_at)?;
                store.record_successful_refresh(refreshed_at)?;
                println!(
                    "Portal snapshot replaced; {} prior records changed.",
                    inserted
                );
            }
        }
        SyncSource::Wayback {
            letter,
            year,
            limit,
        } => {
            validate_import_limit(limit)?;
            let entries = ingest::import_wayback(&client, letter, year, limit)?;
            if entries.is_empty() {
                bail!("no CVC fichas discovered in archived index; inspect the snapshot markup");
            }
            println!("Wayback: {} historical index links", entries.len());
            inserted = store.insert(&entries)?;
        }
    }
    println!(
        "Imported {inserted} new entries; local total: {}",
        store.count()?
    );
    Ok(())
}

fn validate_import_limit(limit: usize) -> Result<()> {
    if !(1..=50).contains(&limit) {
        bail!("import --limit must be between 1 and 50");
    }
    Ok(())
}

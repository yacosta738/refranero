//! Refranero command-line interface with a local-first cache and background refresh.
mod domain;
mod ingest;
mod presentation;
mod refresh;
mod session;
mod store;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use domain::Category;
use std::io::IsTerminal;
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
    /// Colorize terminal output: auto, always or never.
    #[arg(long, global = true, value_enum, default_value_t = presentation::ColorMode::Auto)]
    color: presentation::ColorMode,
    /// Print once and exit instead of opening the interactive reader.
    #[arg(long, global = true)]
    plain: bool,
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
        /// Also refresh the Wayback source when it is due.
        #[arg(long)]
        wayback: bool,
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
    /// Import an archived CVC index and its available language fichas via Wayback Machine.
    Wayback {
        /// First letter of the index (A-Z).
        #[arg(long, default_value_t = 'A')]
        letter: char,
        /// Approximate year of the archival capture.
        #[arg(long, default_value_t = 2020)]
        year: u16,
        /// Maximum number of unique Spanish root fichas (1-50).
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
    if let Some(Command::RefreshWorker { owner, wayback }) = cli.command.as_ref() {
        return run_refresh_worker(&db, owner, *wayback);
    }
    let mut store = Store::open(&db)?;

    match cli.command.as_ref() {
        None | Some(Command::Random) => {
            if interactive_reader(&cli) {
                // Launch the worker before entering the alternate screen so warnings stay visible
                // and the catalog keeps updating while the reader is open.
                schedule_background_refresh(&cli, &db, &mut store, true);
                session::Session::start(&store, &cli.lang, cli.category, cli.color)
                    .and_then(|mut session| session.run())
            } else {
                let result = random(&cli, &store);
                if result.is_ok() {
                    schedule_background_refresh(&cli, &db, &mut store, false);
                }
                result
            }
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
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&entry)?);
            } else {
                let options = presentation::detect(cli.color);
                println!(
                    "{}",
                    presentation::render(std::slice::from_ref(&entry), &options)
                );
            }
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

fn schedule_background_refresh(
    cli: &Cli,
    db: &std::path::Path,
    store: &mut Store,
    allow_wayback: bool,
) {
    if cli.offline {
        return;
    }
    let now = unix_timestamp();
    let virtual_due = match store.last_successful_refresh() {
        Ok(value) => {
            refresh::refresh_decision(value, now, false) == refresh::RefreshDecision::Stale
        }
        Err(error) => {
            eprintln!("Warning: could not check catalog age: {error:#}");
            return;
        }
    };
    let wayback_due = allow_wayback
        && match store.last_wayback_refresh() {
            Ok(value) => {
                refresh::wayback_refresh_decision(value, now, false)
                    == refresh::RefreshDecision::Stale
            }
            Err(error) => {
                eprintln!("Warning: could not check Wayback age: {error:#}");
                return;
            }
        };
    if !virtual_due && !wayback_due {
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
    if let Err(error) = spawn_refresh_worker(db, &owner, wayback_due) {
        let _ = store.release_refresh_lease(&owner);
        eprintln!("Warning: could not start catalog refresh worker: {error:#}");
    }
}

fn worker_owner() -> String {
    format!("{}-{}", std::process::id(), unix_timestamp())
}

fn spawn_refresh_worker(db: &std::path::Path, owner: &str, wayback: bool) -> Result<()> {
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
        .arg(owner);
    if wayback {
        command.arg("--wayback");
    }
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log))
        .stderr(std::process::Stdio::from(error_log));
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    command.spawn().context("spawning refresh worker")?;
    Ok(())
}

fn run_refresh_worker(db: &std::path::Path, owner: &str, allow_wayback: bool) -> Result<()> {
    let mut store = Store::open(db)?;
    let now = unix_timestamp();
    let virtual_due = refresh::refresh_decision(store.last_successful_refresh()?, now, false)
        == refresh::RefreshDecision::Stale;
    let wayback_due = allow_wayback
        && refresh::wayback_refresh_decision(store.last_wayback_refresh()?, now, false)
            == refresh::RefreshDecision::Stale;
    if !virtual_due && !wayback_due {
        return store.release_refresh_lease(owner);
    }

    let result = (|| -> Result<()> {
        if virtual_due {
            refresh_virtual(&mut store, owner)?;
        }
        if wayback_due {
            refresh_wayback(&store, owner)?;
        }
        Ok(())
    })();
    let cleared = store.clear_refresh_progress();
    let released = store.release_refresh_lease(owner);
    result?;
    cleared?;
    released?;
    Ok(())
}

fn publish_progress(store: &Store, phase: &str, done: usize, total: usize) {
    let _ = store.set_refresh_progress(&store::RefreshProgress {
        phase: phase.to_owned(),
        done,
        total,
        updated_at: unix_timestamp(),
    });
}

fn refresh_virtual(store: &mut Store, owner: &str) -> Result<()> {
    let categories = Category::all();
    let total = categories.len();
    let mut snapshot = Vec::new();
    for (index, category) in categories.into_iter().enumerate() {
        if !store.renew_refresh_lease(unix_timestamp(), owner)? {
            bail!(
                "background refresh lease expired before updating {}",
                category.as_str()
            );
        }
        publish_progress(store, "Cervantes Virtual", index, total);
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
    publish_progress(store, "Cervantes Virtual", total, total);
    store.replace_virtual_snapshot(&snapshot, unix_timestamp())?;
    let count = store.count_source("cervantes_virtual")?;
    eprintln!("Cervantes Virtual refresh completed with {count} records.");
    Ok(())
}

fn refresh_wayback(store: &Store, owner: &str) -> Result<()> {
    let letter = refresh::next_letter(store.wayback_cursor()?);
    let phase = format!("Wayback {letter}");
    let client = ingest::http_client_with_timeout(std::time::Duration::from_secs(5))?;
    let report = ingest::import_wayback_with_progress(
        &client,
        letter,
        refresh::year_from_unix(unix_timestamp()),
        refresh::AUTOMATIC_WAYBACK_LIMIT,
        |done, total| {
            let _ = store.renew_refresh_lease(unix_timestamp(), owner);
            publish_progress(store, &phase, done, total);
        },
    )?;
    if report.entries.is_empty() {
        bail!("no CVC fichas discovered in the archived index for letter {letter}");
    }
    let inserted = store.insert_wayback(&report.entries, &report.captured_spanish_pars)?;
    store.record_wayback_refresh(unix_timestamp())?;
    store.set_wayback_cursor(letter)?;
    eprintln!(
        "Wayback {letter} refresh: {} roots, {inserted} new entries.",
        report.roots_processed
    );
    Ok(())
}

fn unix_timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

fn interactive_reader(cli: &Cli) -> bool {
    !cli.plain
        && !cli.json
        && !cli.browser
        && cli.count == 1
        && std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal()
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
        let options = presentation::detect(cli.color);
        println!("{}", presentation::render(&sayings, &options));
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
            let report = ingest::import_wayback(&client, letter, year, limit)?;
            if report.entries.is_empty() {
                bail!("no CVC fichas discovered in archived index; inspect the snapshot markup");
            }
            println!(
                "Wayback: {} root fichas processed; {} captures available; {} captures unavailable; {} partial failures.",
                report.roots_processed,
                report.variants_available,
                report.variants_unavailable,
                report.warnings.len()
            );
            for warning in &report.warnings {
                eprintln!(
                    "Warning: Wayback Par={} language={}: {}",
                    warning.par,
                    warning.language.as_deref().unwrap_or("unknown"),
                    warning.reason
                );
            }
            inserted = store.insert_wayback(&report.entries, &report.captured_spanish_pars)?;
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

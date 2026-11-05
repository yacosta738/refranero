//! Refranero command-line interface with a local-first cache and background refresh.
mod domain;
mod ingest;
mod presentation;
mod refresh;
mod session;
mod store;
mod update;
mod wayback_request;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use domain::Category;
use std::io::IsTerminal;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
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
    /// Interface language. Defaults to the supported system locale, or Spanish.
    #[arg(long, global = true, value_enum)]
    ui_lang: Option<presentation::UiLanguage>,
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
        /// Refresh every allowed source even when its cache is fresh (manual update).
        #[arg(long)]
        force: bool,
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
    let ui_language = presentation::resolve_ui_language(cli.ui_lang, |name| std::env::var_os(name));
    let db = match cli.db.as_ref() {
        Some(path) => path.clone(),
        None => dirs::data_local_dir()
            .context("could not determine user data directory; pass --db <path>")?
            .join("refranero")
            .join("catalog.sqlite3"),
    };
    if let Some(Command::RefreshWorker {
        owner,
        wayback,
        force,
    }) = cli.command.as_ref()
    {
        return run_refresh_worker(&db, owner, *wayback, *force);
    }
    let mut store = Store::open(&db)?;

    match cli.command.as_ref() {
        None | Some(Command::Random) => {
            if interactive_reader(&cli) {
                // Launch the worker before entering the alternate screen so warnings stay visible
                // and the catalog keeps updating while the reader is open.
                schedule_background_refresh(&cli, &db, &store, true);
                session::Session::start(&store, &cli.lang, cli.category, cli.color, ui_language)
                    .map(|session| {
                        session.with_updates(db.clone(), cli.offline, Box::new(spawn_manual_update))
                    })
                    .and_then(|mut session| session.run())
            } else {
                let result = random(&cli, &store, ui_language);
                if result.is_ok() {
                    schedule_background_refresh(&cli, &db, &store, false);
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
                let mut options = presentation::detect(cli.color);
                options.language = ui_language;
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

fn schedule_background_refresh(cli: &Cli, db: &Path, store: &Store, allow_wayback: bool) {
    if cli.offline {
        return;
    }
    let now = unix_timestamp();
    let last_virtual = match store.last_successful_refresh() {
        Ok(value) => value,
        Err(error) => {
            eprintln!("Warning: could not check catalog age: {error:#}");
            return;
        }
    };
    let wayback_bootstrap_pending = allow_wayback
        && match store.wayback_bootstrap_next() {
            Ok(next) => next.is_some(),
            Err(error) => {
                eprintln!("Warning: could not check Wayback bootstrap state: {error:#}");
                false
            }
        };
    let last_wayback = if allow_wayback && !wayback_bootstrap_pending {
        match store.last_wayback_refresh() {
            Ok(value) => value,
            Err(error) => {
                eprintln!("Warning: could not check Wayback age: {error:#}");
                Some(now)
            }
        }
    } else {
        None
    };
    let scope = update::background_scope(
        last_virtual,
        last_wayback,
        now,
        allow_wayback,
        wayback_bootstrap_pending,
    );
    if scope.is_empty() {
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
    if let Err(error) = spawn_refresh_worker(db, &owner, scope.wayback) {
        let _ = store.release_refresh_lease(&owner);
        eprintln!("Warning: could not start catalog refresh worker: {error:#}");
    }
}

fn worker_owner() -> String {
    format!("{}-{}", std::process::id(), unix_timestamp())
}

fn spawn_refresh_worker(db: &Path, owner: &str, wayback: bool) -> Result<()> {
    spawn_refresh_worker_with(db, owner, wayback, false)
}

/// Spawn the forced worker used by the interactive `u` action.
fn spawn_manual_update(db: &Path, owner: &str) -> Result<()> {
    spawn_refresh_worker_with(db, owner, true, true)
}

fn spawn_refresh_worker_with(db: &Path, owner: &str, wayback: bool, force: bool) -> Result<()> {
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
    if force {
        command.arg("--force");
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

fn run_refresh_worker(db: &Path, owner: &str, allow_wayback: bool, force: bool) -> Result<()> {
    let mut store = Store::open(db)?;
    let now = unix_timestamp();
    let last_virtual = store.last_successful_refresh()?;
    let bootstrap_pending = if allow_wayback {
        match store.wayback_bootstrap_next() {
            Ok(next) => next.is_some(),
            Err(error) => {
                eprintln!("Warning: could not check Wayback bootstrap state: {error:#}");
                false
            }
        }
    } else {
        false
    };
    let last_wayback = if allow_wayback && !bootstrap_pending {
        match store.last_wayback_refresh() {
            Ok(value) => value,
            Err(error) => {
                eprintln!("Warning: could not check Wayback age: {error:#}");
                // A read failure must not turn an automatic run into a due crawl.
                Some(now)
            }
        }
    } else {
        None
    };
    let scope = update::update_scope(last_virtual, last_wayback, now, allow_wayback, force);
    update::run_worker(&mut store, owner, scope, refresh_virtual, refresh_wayback)
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
        // Automatic refreshes are full snapshots. Explicit `sync` keeps its user-selected limit.
        let entries = ingest::import_virtual(&client, category, usize::MAX)?;
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

fn refresh_wayback(store: &mut Store, owner: &str) -> Result<()> {
    let client = ingest::wayback_client_with_timeout(std::time::Duration::from_secs(5))?;
    let mut requests = wayback_request::WaybackRequests::new();
    update::run_wayback_updates(
        store,
        |store, letter, bootstrap, _position| {
            if !store.renew_refresh_lease(unix_timestamp(), owner)? {
                bail!("refresh lease lost before the Wayback letter {letter}");
            }
            let phase = wayback_phase(letter, bootstrap);
            let report = ingest::import_wayback_stream(
                &client,
                &mut requests,
                letter,
                refresh::year_from_unix(unix_timestamp()),
                if bootstrap {
                    usize::MAX
                } else {
                    refresh::AUTOMATIC_WAYBACK_LIMIT
                },
                |event| handle_wayback_event(store, owner, &phase, event),
            )?;
            for warning in &report.warnings {
                eprintln!(
                    "Wayback {letter} warning: Par={} language={}: {}",
                    warning.par,
                    warning.language.as_deref().unwrap_or("unknown"),
                    warning.reason
                );
            }
            eprintln!(
                "Wayback {letter} refresh: {} roots saved continuously, {} warnings.",
                report.roots_processed,
                report.warnings.len()
            );
            Ok(report.entries.len())
        },
        |_, _, _, _| {},
    )?;
    Ok(())
}

fn wayback_phase(letter: char, bootstrap: bool) -> String {
    if bootstrap {
        let position = u32::from(letter) - u32::from('A') + 1;
        format!("Wayback bootstrap {position}/26 ({letter})")
    } else {
        format!("Wayback {letter}")
    }
}

fn handle_wayback_event(
    store: &Store,
    owner: &str,
    phase: &str,
    event: ingest::WaybackEvent<'_>,
) -> Result<()> {
    if !store.renew_refresh_lease(unix_timestamp(), owner)? {
        bail!("refresh lease lost during the Wayback run; letter interrupted");
    }
    match event {
        ingest::WaybackEvent::Entries(entries, pars) => {
            store.insert_wayback(entries, pars)?;
        }
        ingest::WaybackEvent::Progress {
            activity,
            done,
            total,
        } => {
            store.set_refresh_progress(&store::RefreshProgress {
                phase: format!("{phase} {activity}"),
                done,
                total,
                updated_at: unix_timestamp(),
            })?;
        }
    }
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

fn random(cli: &Cli, store: &Store, ui_language: presentation::UiLanguage) -> Result<()> {
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
        let mut options = presentation::detect(cli.color);
        options.language = ui_language;
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
            let client = ingest::wayback_client_with_timeout(std::time::Duration::from_secs(20))?;
            let mut requests = wayback_request::WaybackRequests::new();
            let report = ingest::import_wayback_stream(
                &client,
                &mut requests,
                letter,
                year,
                limit,
                |event| {
                    if let ingest::WaybackEvent::Entries(entries, pars) = event {
                        inserted += store.insert_wayback(entries, pars)?;
                    }
                    Ok(())
                },
            )?;
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

#[cfg(test)]
mod wayback_progress_tests {
    use super::*;
    #[test]
    fn resumed_bootstrap_identifies_absolute_letter_position() {
        assert_eq!(wayback_phase('D', true), "Wayback bootstrap 4/26 (D)");
        assert_eq!(wayback_phase('Z', true), "Wayback bootstrap 26/26 (Z)");
        assert_eq!(wayback_phase('D', false), "Wayback D");
    }

    #[test]
    fn bootstrap_progress_uses_fichas_not_letters() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        store
            .try_acquire_refresh_lease(unix_timestamp(), "test")
            .unwrap();
        handle_wayback_event(
            &store,
            "test",
            "Wayback bootstrap 1/26 (A)",
            ingest::WaybackEvent::Progress {
                done: 1,
                total: 50,
                activity: "",
            },
        )
        .unwrap();
        let progress = store.refresh_progress().unwrap().unwrap();
        assert_eq!((progress.done, progress.total), (1, 50));
        assert!(progress.phase.contains("1/26 (A)"));
    }
}

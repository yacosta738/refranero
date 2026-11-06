//! SQLite storage adapter. All reads and writes are local.
use crate::domain::{NewSaying, Saying};
use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

// Increment when a later release changes the bundled source-link mappings.
const SOURCE_LINK_MIGRATION_KEY: &str = "source_link_migration_version";
const SOURCE_LINK_MIGRATION_VERSION: &str = "1";
const REFRESH_PROGRESS_KEY: &str = "refresh_progress";
const REFRESH_ERROR_KEY: &str = "last_refresh_error";
const WAYBACK_REFRESH_KEY: &str = "last_wayback_refresh";
const WAYBACK_CURSOR_KEY: &str = "wayback_cursor";
const WAYBACK_BOOTSTRAP_NEXT_KEY: &str = "wayback_bootstrap_next";
const WAYBACK_BOOTSTRAP_COMPLETE_KEY: &str = "wayback_bootstrap_complete";
const GROUP_UNIQUENESS_MIGRATION_KEY: &str = "group_uniqueness_migration_version";
const CATALOG_GENERATION_KEY: &str = "catalog_generation";
const WAYBACK_UNRESOLVED_KEY: &str = "wayback_unresolved_letters";
const WAYBACK_RETRY_DEADLINE_KEY: &str = "wayback_retry_deadline";

/// Progress of the background refresh, published by the worker for the reader.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefreshProgress {
    pub phase: String,
    pub done: usize,
    pub total: usize,
    pub updated_at: i64,
}

/// Last failed update, published by the worker for the reader.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefreshError {
    pub message: String,
    pub updated_at: i64,
}

/// Owns an initialized SQLite connection.
pub struct Store {
    connection: Connection,
    path: Option<PathBuf>,
}

impl Store {
    /// Open the catalog, create tables and install the small public-tradition seed.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let connection =
            Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        // WAL lets the interactive reader keep working while the refresh worker commits; the
        // timeout absorbs the short write windows. WAL is unavailable on some filesystems, so the
        // fallback to the rollback journal must not abort the command.
        let _ = connection.execute_batch("PRAGMA journal_mode = WAL;");
        connection.execute_batch("PRAGMA busy_timeout = 5000;")?;
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS sayings (
               id INTEGER PRIMARY KEY,
               text TEXT NOT NULL,
               category TEXT NOT NULL,
               language TEXT NOT NULL,
               source TEXT NOT NULL,
               source_url TEXT NOT NULL,
               original_url TEXT,
               captured_at TEXT,
                group_id TEXT
              );
              CREATE INDEX IF NOT EXISTS idx_sayings_category_lang
                ON sayings(category, language);
              CREATE UNIQUE INDEX IF NOT EXISTS idx_sayings_grouped_unique
                ON sayings(text, category, language, source, group_id) WHERE group_id IS NOT NULL;
              CREATE UNIQUE INDEX IF NOT EXISTS idx_sayings_ungrouped_unique
                ON sayings(text, category, language, source) WHERE group_id IS NULL;
             CREATE TABLE IF NOT EXISTS metadata (
               key TEXT PRIMARY KEY,
               value TEXT NOT NULL
             );",
        )?;
        let path = (path != Path::new(":memory:")).then(|| path.to_path_buf());
        let mut store = Self { connection, path };
        store.migrate_group_uniqueness()?;
        let seeds: Vec<NewSaying> = serde_json::from_str(include_str!("../data/seed.json"))?;
        let is_new_catalog = store.count()? == 0;
        if is_new_catalog {
            store.insert(&seeds)?;
        }
        store.migrate_seed_source_links(&seeds, is_new_catalog)?;
        Ok(store)
    }

    fn migrate_group_uniqueness(&mut self) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let version = transaction
            .query_row(
                "SELECT value FROM metadata WHERE key = ?1",
                [GROUP_UNIQUENESS_MIGRATION_KEY],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if version.as_deref() == Some("1") {
            transaction.commit()?;
            return Ok(());
        }
        let has_sayings: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'sayings')",
            [],
            |row| row.get(0),
        )?;
        if has_sayings {
            let has_legacy_unique = {
                let mut indexes = transaction.prepare("PRAGMA index_list(sayings)")?;
                let rows = indexes.query_map([], |row| {
                    Ok((row.get::<_, String>(1)?, row.get::<_, String>(3)?))
                })?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
                    .iter()
                    .any(|(_, origin)| origin == "u")
            };
            if has_legacy_unique {
                transaction.execute_batch(
                    "CREATE TABLE sayings_migrated (
                       id INTEGER PRIMARY KEY, text TEXT NOT NULL, category TEXT NOT NULL,
                       language TEXT NOT NULL, source TEXT NOT NULL, source_url TEXT NOT NULL,
                       original_url TEXT, captured_at TEXT, group_id TEXT
                     );
                     INSERT INTO sayings_migrated SELECT id, text, category, language, source,
                       source_url, original_url, captured_at, group_id FROM sayings;
                     DROP TABLE sayings;
                     ALTER TABLE sayings_migrated RENAME TO sayings;
                     CREATE INDEX idx_sayings_category_lang ON sayings(category, language);
                     CREATE UNIQUE INDEX idx_sayings_grouped_unique
                       ON sayings(text, category, language, source, group_id) WHERE group_id IS NOT NULL;
                     CREATE UNIQUE INDEX idx_sayings_ungrouped_unique
                       ON sayings(text, category, language, source) WHERE group_id IS NULL;"
                )?;
                // Associations silently discarded by the old constraint cannot be reconstructed.
                // Force a complete bootstrap so surviving records are re-associated from source.
                for key in [
                    WAYBACK_BOOTSTRAP_COMPLETE_KEY,
                    WAYBACK_BOOTSTRAP_NEXT_KEY,
                    WAYBACK_CURSOR_KEY,
                    WAYBACK_REFRESH_KEY,
                    WAYBACK_UNRESOLVED_KEY,
                ] {
                    transaction.execute("DELETE FROM metadata WHERE key = ?1", [key])?;
                }
                transaction.execute(
                    "INSERT INTO metadata(key, value) VALUES (?1, 'A') ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    [WAYBACK_BOOTSTRAP_NEXT_KEY],
                )?;
            }
        }
        transaction.execute(
            "INSERT INTO metadata(key, value) VALUES (?1, '1') ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [GROUP_UNIQUENESS_MIGRATION_KEY],
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn migrate_seed_source_links(
        &mut self,
        seeds: &[NewSaying],
        is_new_catalog: bool,
    ) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let applied_version = transaction
            .query_row(
                "SELECT value FROM metadata WHERE key = ?1",
                [SOURCE_LINK_MIGRATION_KEY],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if applied_version.as_deref() == Some(SOURCE_LINK_MIGRATION_VERSION) {
            transaction.commit()?;
            return Ok(());
        }

        for seed in seeds {
            transaction.execute(
                "UPDATE sayings
                 SET source_url = ?1, original_url = ?2, captured_at = ?3
                 WHERE text = ?4 AND category = ?5 AND language = ?6 AND source = ?7
                   AND group_id IS ?8",
                params![
                    seed.source_url,
                    seed.original_url,
                    seed.captured_at,
                    seed.text,
                    seed.category,
                    seed.language,
                    seed.source,
                    seed.group_id,
                ],
            )?;
        }

        if !is_new_catalog {
            transaction.execute(
                "INSERT INTO metadata(key, value) VALUES ('last_successful_refresh', '0')
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [],
            )?;
        }
        transaction.execute(
            "INSERT INTO metadata(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![SOURCE_LINK_MIGRATION_KEY, SOURCE_LINK_MIGRATION_VERSION],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Return the Unix timestamp of the last successful automatic refresh.
    pub fn last_successful_refresh(&self) -> Result<Option<i64>> {
        let value = self
            .connection
            .query_row(
                "SELECT value FROM metadata WHERE key = 'last_successful_refresh'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        value
            .map(|value| {
                value
                    .parse::<i64>()
                    .context("invalid last_successful_refresh timestamp")
            })
            .transpose()
    }

    /// Monotonic generation for persisted catalog content changes.
    pub fn catalog_generation(&self) -> Result<i64> {
        self.metadata(CATALOG_GENERATION_KEY)?
            .map_or(Ok(0), |value| {
                value.parse::<i64>().context("invalid catalog generation")
            })
    }

    /// Create the separate-connection adapter used by Wayback request policy.
    pub fn wayback_retry_deadline_persistence(
        &self,
        owner: &str,
    ) -> Option<Box<dyn crate::wayback_request::RetryDeadlinePersistence>> {
        self.path.as_ref().map(|path| {
            Box::new(SqliteRetryDeadlinePersistence {
                path: path.clone(),
                owner: owner.to_owned(),
            }) as Box<dyn crate::wayback_request::RetryDeadlinePersistence>
        })
    }

    /// Create shared Wayback request coordination for file-backed catalogs.
    /// In-memory catalogs retain the request controller's local pacing behavior.
    pub fn wayback_request_coordination(
        &self,
    ) -> Result<Option<Box<dyn crate::wayback_request::SharedRequestCoordination>>> {
        let Some(path) = self.path.as_ref() else {
            return Ok(None);
        };
        let connection = Connection::open(path)
            .with_context(|| format!("opening Wayback coordination catalog {}", path.display()))?;
        connection.execute_batch("PRAGMA busy_timeout = 5000;")?;
        Ok(Some(Box::new(SqliteWaybackRequestCoordination {
            connection,
        })))
    }

    /// Acquire the background refresh lease if no active worker owns it.
    ///
    /// The caller only needs shared access: the session may hold the lease while the reader
    /// keeps using the catalog.
    pub fn try_acquire_refresh_lease(&self, now: i64, owner: &str) -> Result<bool> {
        // No other transaction runs on this connection at this point, so the unchecked
        // transaction is safe and does not require `&mut self`.
        let transaction = self.connection.unchecked_transaction()?;
        transaction.execute(
            "INSERT INTO metadata(key, value) VALUES ('refresh_lease_until', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value
             WHERE CAST(metadata.value AS INTEGER) <= ?2",
            params![now + crate::refresh::REFRESH_LEASE_SECONDS, now],
        )?;
        let acquired = transaction.changes() == 1;
        if acquired {
            transaction.execute(
                "INSERT INTO metadata(key, value) VALUES ('refresh_lease_owner', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [owner],
            )?;
        }
        transaction.commit()?;
        Ok(acquired)
    }

    /// Release the background refresh lease only when the caller owns it.
    pub fn release_refresh_lease(&self, owner: &str) -> Result<()> {
        let transaction = self.connection.unchecked_transaction()?;
        let deleted_owner = transaction.execute(
            "DELETE FROM metadata WHERE key = 'refresh_lease_owner' AND value = ?1",
            [owner],
        )?;
        if deleted_owner == 1 {
            transaction.execute("DELETE FROM metadata WHERE key = 'refresh_lease_until'", [])?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Renew a lease only if it is unexpired and the caller owns it.
    pub fn renew_refresh_lease(&self, now: i64, owner: &str) -> Result<bool> {
        let changed = self.connection.execute(
            "UPDATE metadata SET value = ?1
             WHERE key = 'refresh_lease_until' AND CAST(value AS INTEGER) > ?2
             AND EXISTS (SELECT 1 FROM metadata WHERE key = 'refresh_lease_owner' AND value = ?3)",
            params![now + crate::refresh::REFRESH_LEASE_SECONDS, now, owner],
        )?;
        Ok(changed == 1)
    }

    /// Record a successful source refresh.
    pub fn record_successful_refresh(&self, timestamp: i64) -> Result<()> {
        self.connection.execute(
            "INSERT INTO metadata(key, value) VALUES ('last_successful_refresh', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [timestamp.to_string()],
        )?;
        Ok(())
    }

    /// Timestamp of the last successful Wayback refresh.
    pub fn last_wayback_refresh(&self) -> Result<Option<i64>> {
        self.metadata(WAYBACK_REFRESH_KEY)?
            .map(|value| {
                value
                    .parse::<i64>()
                    .context("invalid last_wayback_refresh timestamp")
            })
            .transpose()
    }

    /// Record a successful Wayback run and the processed letter in one transaction.
    pub fn record_wayback_success(&mut self, letter: char, timestamp: i64) -> Result<()> {
        let transaction = self.connection.transaction()?;
        let unresolved: String = transaction
            .query_row(
                "SELECT value FROM metadata WHERE key = ?1",
                [WAYBACK_UNRESOLVED_KEY],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or_default();
        transaction.execute(
            "INSERT INTO metadata(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![WAYBACK_UNRESOLVED_KEY, unresolved.chars().filter(|item| *item != letter).collect::<String>()],
        )?;
        for (key, value) in [
            (WAYBACK_REFRESH_KEY, timestamp.to_string()),
            (WAYBACK_CURSOR_KEY, letter.to_string()),
        ] {
            transaction.execute(
                "INSERT INTO metadata(key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Return the next bootstrap letter, or `None` after the full index is imported.
    /// Missing state means a legacy or new catalog still needs its initial A-Z pass.
    pub fn wayback_bootstrap_next(&self) -> Result<Option<char>> {
        if self.metadata(WAYBACK_BOOTSTRAP_COMPLETE_KEY)?.as_deref() == Some("true") {
            return Ok(None);
        }
        let value = self.metadata(WAYBACK_BOOTSTRAP_NEXT_KEY)?;
        let letter = value
            .as_deref()
            .and_then(|value| value.chars().next())
            .unwrap_or('A')
            .to_ascii_uppercase();
        if !letter.is_ascii_uppercase() {
            anyhow::bail!("invalid Wayback bootstrap letter in catalog metadata");
        }
        Ok(Some(letter))
    }

    /// Commit one successful bootstrap letter and its resume point atomically.
    pub fn record_wayback_bootstrap_success(&mut self, letter: char, timestamp: i64) -> Result<()> {
        self.record_wayback_bootstrap(letter, timestamp, false)
    }

    fn record_wayback_bootstrap(
        &mut self,
        letter: char,
        timestamp: i64,
        unavailable: bool,
    ) -> Result<()> {
        let letter = letter.to_ascii_uppercase();
        if !letter.is_ascii_uppercase() {
            anyhow::bail!("Wayback bootstrap letter must be A-Z");
        }
        let transaction = self.connection.transaction()?;
        let unresolved: String = transaction
            .query_row(
                "SELECT value FROM metadata WHERE key = ?1",
                [WAYBACK_UNRESOLVED_KEY],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or_default();
        let unresolved = if unavailable {
            let mut letters: Vec<char> = unresolved.chars().collect();
            if !letters.contains(&letter) {
                letters.push(letter);
            }
            letters.sort_unstable();
            letters.iter().collect::<String>()
        } else {
            unresolved.chars().filter(|item| *item != letter).collect()
        };
        transaction.execute(
            "INSERT INTO metadata(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![WAYBACK_UNRESOLVED_KEY, unresolved],
        )?;
        for (key, value) in [
            (WAYBACK_REFRESH_KEY, timestamp.to_string()),
            (WAYBACK_CURSOR_KEY, letter.to_string()),
        ] {
            transaction.execute(
                "INSERT INTO metadata(key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )?;
        }
        if letter == 'Z' {
            transaction.execute(
                "INSERT INTO metadata(key, value) VALUES (?1, 'true')
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [WAYBACK_BOOTSTRAP_COMPLETE_KEY],
            )?;
            transaction.execute(
                "DELETE FROM metadata WHERE key = ?1",
                [WAYBACK_BOOTSTRAP_NEXT_KEY],
            )?;
        } else {
            transaction.execute(
                "INSERT INTO metadata(key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![
                    WAYBACK_BOOTSTRAP_NEXT_KEY,
                    (((letter as u8) + 1) as char).to_string()
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Letter the automatic Wayback job should process next.
    pub fn wayback_cursor(&self) -> Result<Option<char>> {
        Ok(self
            .metadata(WAYBACK_CURSOR_KEY)?
            .and_then(|value| value.chars().next()))
    }

    pub fn wayback_unresolved_letters(&self) -> Result<Vec<char>> {
        Ok(self
            .metadata(WAYBACK_UNRESOLVED_KEY)?
            .unwrap_or_default()
            .chars()
            .collect())
    }

    pub fn record_wayback_bootstrap_unavailable(
        &mut self,
        letter: char,
        timestamp: i64,
    ) -> Result<()> {
        self.record_wayback_bootstrap(letter, timestamp, true)
    }

    pub fn clear_wayback_unresolved(&self, letter: char) -> Result<()> {
        let remaining: String = self
            .wayback_unresolved_letters()?
            .into_iter()
            .filter(|item| *item != letter)
            .collect();
        self.set_metadata(WAYBACK_UNRESOLVED_KEY, &remaining)
    }

    /// Publish refresh progress for the interactive reader.
    pub fn set_refresh_progress(&self, progress: &RefreshProgress) -> Result<()> {
        let value = serde_json::to_string(progress)?;
        self.set_metadata(REFRESH_PROGRESS_KEY, &value)
    }

    /// Read the latest refresh progress. Corrupt or missing values read as `None`.
    pub fn refresh_progress(&self) -> Result<Option<RefreshProgress>> {
        let Some(value) = self.metadata(REFRESH_PROGRESS_KEY)? else {
            return Ok(None);
        };
        Ok(serde_json::from_str(&value).ok())
    }

    /// Publish the last update failure for the interactive reader.
    pub fn record_refresh_error(&self, message: &str, updated_at: i64) -> Result<()> {
        let value = serde_json::to_string(&RefreshError {
            message: message.to_owned(),
            updated_at,
        })?;
        self.set_metadata(REFRESH_ERROR_KEY, &value)
    }

    /// Read the last update failure. Corrupt or missing values read as `None`.
    pub fn refresh_error(&self) -> Result<Option<RefreshError>> {
        let Some(value) = self.metadata(REFRESH_ERROR_KEY)? else {
            return Ok(None);
        };
        Ok(serde_json::from_str(&value).ok())
    }

    /// Remove the last update failure after a successful run.
    pub fn clear_refresh_error(&self) -> Result<()> {
        self.connection
            .execute("DELETE FROM metadata WHERE key = ?1", [REFRESH_ERROR_KEY])?;
        Ok(())
    }

    /// Remove published progress, but only while `owner` still holds the refresh lease.
    pub fn clear_refresh_progress(&self, owner: &str) -> Result<()> {
        self.connection.execute(
            "DELETE FROM metadata WHERE key = ?1
             AND EXISTS (
               SELECT 1 FROM metadata WHERE key = 'refresh_lease_owner' AND value = ?2
             )",
            params![REFRESH_PROGRESS_KEY, owner],
        )?;
        Ok(())
    }

    fn metadata(&self, key: &str) -> Result<Option<String>> {
        self.connection
            .query_row("SELECT value FROM metadata WHERE key = ?1", [key], |row| {
                row.get::<_, String>(0)
            })
            .optional()
            .map_err(Into::into)
    }

    fn set_metadata(&self, key: &str, value: &str) -> Result<()> {
        self.connection.execute(
            "INSERT INTO metadata(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// Replace the current portal snapshot and update its successful-refresh marker atomically.
    pub fn replace_virtual_snapshot(
        &mut self,
        entries: &[NewSaying],
        timestamp: i64,
    ) -> Result<usize> {
        if entries.is_empty() {
            bail!("refusing to replace the Cervantes Virtual snapshot with an empty result");
        }
        let transaction = self.connection.transaction()?;
        let inserted = {
            for entry in entries {
                if !matches!(
                    entry.category.as_str(),
                    "refran" | "paremia" | "proverbio" | "dicho" | "adivinanza" | "cita"
                ) {
                    bail!("unsupported category: {}", entry.category);
                }
                if entry.text.trim().is_empty() || entry.text.chars().count() > 3000 {
                    bail!("text must contain between 1 and 3000 characters");
                }
                let source_url = url::Url::parse(&entry.source_url)
                    .with_context(|| format!("invalid source URL: {}", entry.source_url))?;
                if source_url.scheme() != "https"
                    || !matches!(
                        source_url.host_str(),
                        Some("www.cervantesvirtual.com")
                            | Some("cvc.cervantes.es")
                            | Some("cervantesvirtual.com")
                            | Some("web.archive.org")
                            | Some("archive.org")
                    )
                {
                    bail!("source URL must use HTTPS and an approved Cervantes or archive host");
                }
                if let Some(original) = entry.original_url.as_deref() {
                    let original = url::Url::parse(original)
                        .with_context(|| format!("invalid original URL: {original}"))?;
                    if original.scheme() != "https"
                        || !matches!(
                            original.host_str(),
                            Some("www.cervantesvirtual.com")
                                | Some("cvc.cervantes.es")
                                | Some("cervantesvirtual.com")
                                | Some("web.archive.org")
                                | Some("archive.org")
                        )
                    {
                        bail!(
                            "original URL must use HTTPS and an approved Cervantes or archive host"
                        );
                    }
                }
            }
            let expected_source = "cervantes_virtual";
            if entries.iter().any(|entry| entry.source != expected_source) {
                bail!("automatic portal refresh accepts only cervantes_virtual records");
            }
            let deleted =
                transaction.execute("DELETE FROM sayings WHERE source = ?1", [expected_source])?;
            let mut inserted = 0;
            {
                let mut statement = transaction.prepare(
                    "INSERT OR IGNORE INTO sayings
                     (text, category, language, source, source_url, original_url, captured_at, group_id)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                )?;
                for entry in entries {
                    inserted += statement.execute(params![
                        entry.text.trim(),
                        entry.category,
                        entry.language,
                        entry.source,
                        entry.source_url,
                        entry.original_url,
                        entry.captured_at,
                        entry.group_id,
                    ])?;
                }
            }
            transaction.execute(
                "INSERT INTO metadata(key, value) VALUES ('last_successful_refresh', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [timestamp.to_string()],
            )?;
            if deleted > 0 || inserted > 0 {
                bump_catalog_generation(&transaction)?;
            }
            inserted
        };
        transaction.commit()?;
        Ok(inserted)
    }

    /// Insert only unseen tuples. Returns the number of new rows.
    pub fn insert(&self, entries: &[NewSaying]) -> Result<usize> {
        self.insert_with_capture_updates(entries, &HashSet::new())
    }

    /// Insert Wayback records and refresh only Spanish roots with confirmed captures.
    pub fn insert_wayback(
        &self,
        entries: &[NewSaying],
        captured_spanish_pars: &HashSet<String>,
    ) -> Result<usize> {
        self.insert_with_capture_updates(entries, captured_spanish_pars)
    }

    fn insert_with_capture_updates(
        &self,
        entries: &[NewSaying],
        captured_spanish_pars: &HashSet<String>,
    ) -> Result<usize> {
        // A local import file must not be able to inject file:// or javascript: links.
        for entry in entries {
            if !matches!(
                entry.category.as_str(),
                "refran" | "paremia" | "proverbio" | "dicho" | "adivinanza" | "cita"
            ) {
                bail!("unsupported category: {}", entry.category);
            }
            if !(2..=15).contains(&entry.language.len())
                || !entry
                    .language
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c == b'-')
            {
                bail!("invalid language code: {}", entry.language);
            }
            let url = url::Url::parse(&entry.source_url)
                .with_context(|| format!("invalid source URL: {}", entry.source_url))?;
            if !matches!(url.scheme(), "https" | "http") {
                bail!("source URLs must use HTTP(S)");
            }
            if let Some(original) = &entry.original_url {
                let parsed = url::Url::parse(original)
                    .with_context(|| format!("invalid original URL: {original}"))?;
                if !matches!(parsed.scheme(), "https" | "http") {
                    bail!("original URLs must use HTTP(S)");
                }
            }
        }
        let transaction = self.connection.unchecked_transaction()?;
        let mut created = 0;
        let mut changed_any = false;
        let mut insert_ignored = transaction.prepare(
            "INSERT OR IGNORE INTO sayings
             (text, category, language, source, source_url, original_url, captured_at, group_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        let mut upsert_captured = transaction.prepare(
            "INSERT INTO sayings
             (text, category, language, source, source_url, original_url, captured_at, group_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(text, category, language, source, group_id) WHERE group_id IS NOT NULL DO UPDATE SET
               source_url = excluded.source_url,
               original_url = excluded.original_url,
               captured_at = excluded.captured_at
             WHERE sayings.group_id = excluded.group_id",
        )?;
        for e in entries {
            if e.text.trim().is_empty() || e.text.chars().count() > 3000 {
                continue;
            }
            let captured_spanish_par = e
                .group_id
                .as_deref()
                .and_then(|group_id| group_id.strip_prefix("cvc:"));
            let update_captured_ficha = e.source == "cvc_wayback"
                && e.language == "es"
                && captured_spanish_par.is_some_and(|par| captured_spanish_pars.contains(par));
            let params = params![
                e.text,
                e.category,
                e.language,
                e.source,
                e.source_url,
                e.original_url,
                e.captured_at,
                e.group_id
            ];
            if update_captured_ficha {
                let existed: bool = transaction.query_row(
                     "SELECT EXISTS(
                       SELECT 1 FROM sayings
                        WHERE text = ?1 AND category = ?2 AND language = ?3 AND source = ?4 AND group_id IS ?5
                      )",
                    params![e.text, e.category, e.language, e.source, e.group_id],
                    |row| row.get(0),
                )?;
                let changed = upsert_captured.execute(params)?;
                changed_any |= changed > 0;
                if !existed {
                    created += changed;
                }
            } else {
                let changed = insert_ignored.execute(params)?;
                changed_any |= changed > 0;
                created += changed;
            }
            if let Some(group_id) = e.group_id.as_deref() {
                let exists: bool = transaction.query_row(
                    "SELECT EXISTS(SELECT 1 FROM sayings WHERE text = ?1 AND category = ?2 AND language = ?3 AND source = ?4 AND group_id = ?5)",
                    params![e.text, e.category, e.language, e.source, group_id],
                    |row| row.get(0),
                )?;
                if !exists {
                    bail!("grouped saying was not persisted: {} ({group_id})", e.text);
                }
            }
        }
        drop(insert_ignored);
        drop(upsert_captured);
        if changed_any {
            bump_catalog_generation(&transaction)?;
        }
        transaction.commit()?;
        Ok(created)
    }

    /// Count entries from one source.
    pub fn count_source(&self, source: &str) -> Result<i64> {
        Ok(self.connection.query_row(
            "SELECT COUNT(*) FROM sayings WHERE source = ?1",
            [source],
            |row| row.get(0),
        )?)
    }

    /// Count all locally stored sayings.
    pub fn count(&self) -> Result<i64> {
        Ok(self
            .connection
            .query_row("SELECT COUNT(*) FROM sayings", [], |r| r.get(0))?)
    }

    /// Query random entries, optionally filtering on category.
    pub fn random(
        &self,
        language: &str,
        category: Option<&str>,
        count: usize,
    ) -> Result<Vec<Saying>> {
        let mut stmt = self.connection.prepare(
            "SELECT id, text, category, language, source, source_url, original_url, captured_at, group_id
             FROM sayings WHERE language = ?1 AND (?2 IS NULL OR category = ?2)
             ORDER BY RANDOM() LIMIT ?3"
        )?;
        let rows = stmt.query_map(params![language, category, count as i64], read_saying)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Fetch an exact row, including its source URL.
    pub fn get(&self, id: i64) -> Result<Option<Saying>> {
        self.connection.query_row(
            "SELECT id, text, category, language, source, source_url, original_url, captured_at, group_id
             FROM sayings WHERE id = ?1",
            [id], read_saying
        ).optional().map_err(Into::into)
    }

    /// Entries that share a multilingual group, Spanish first and then by language code.
    pub fn by_group(&self, group_id: &str) -> Result<Vec<Saying>> {
        let mut statement = self.connection.prepare(
            "SELECT id, text, category, language, source, source_url, original_url, captured_at, group_id
             FROM sayings WHERE group_id = ?1
             ORDER BY (language = 'es') DESC, language",
        )?;
        let rows = statement.query_map([group_id], read_saying)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Count entries by source, language and category.
    pub fn summary(&self) -> Result<Vec<(String, String, String, i64)>> {
        let mut stmt = self.connection.prepare(
            "SELECT source, language, category, COUNT(*) FROM sayings
             GROUP BY source, language, category ORDER BY source, language, category",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Count entries by category, biggest first.
    ///
    /// The tie break on the identifier keeps the order stable between calls.
    pub fn category_totals(&self) -> Result<Vec<(String, i64)>> {
        self.grouped_counts("category")
    }

    /// Count entries by language, biggest first.
    pub fn language_totals(&self) -> Result<Vec<(String, i64)>> {
        self.grouped_counts("language")
    }

    fn grouped_counts(&self, column: &str) -> Result<Vec<(String, i64)>> {
        // The column comes from the code, never from user input.
        let sql = format!(
            "SELECT {column}, COUNT(*) FROM sayings GROUP BY {column}
             ORDER BY COUNT(*) DESC, {column} ASC"
        );
        let mut stmt = self.connection.prepare(&sql)?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }
}

struct SqliteRetryDeadlinePersistence {
    path: PathBuf,
    owner: String,
}

struct SqliteWaybackRequestCoordination {
    connection: Connection,
}

impl crate::wayback_request::SharedRequestCoordination for SqliteWaybackRequestCoordination {
    fn gate(&mut self, now_millis: i64) -> Result<crate::wayback_request::RequestGate> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let next_request: Option<i64> = transaction
            .query_row(
                "SELECT CAST(value AS INTEGER) FROM metadata WHERE key = 'wayback_next_request_millis'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let cooldown: Option<i64> = transaction
            .query_row(
                "SELECT CAST(value AS INTEGER) FROM metadata WHERE key = 'wayback_cooldown_until_millis'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let available_at = next_request
            .unwrap_or_default()
            .max(cooldown.unwrap_or_default());
        if available_at > now_millis {
            transaction.commit()?;
            return Ok(crate::wayback_request::RequestGate::WaitUntil(available_at));
        }

        let next_at = now_millis.saturating_add(
            i64::try_from(crate::wayback_request::SHARED_MIN_PAUSE.as_millis()).unwrap_or(i64::MAX),
        );
        transaction.execute(
            "INSERT INTO metadata(key, value) VALUES ('wayback_next_request_millis', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [next_at.to_string()],
        )?;
        transaction.commit()?;
        Ok(crate::wayback_request::RequestGate::Granted)
    }

    fn record_cooldown(&mut self, deadline_millis: i64, _now_millis: i64) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let previous: Option<i64> = transaction
            .query_row(
                "SELECT CAST(value AS INTEGER) FROM metadata WHERE key = 'wayback_cooldown_until_millis'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let deadline = previous.unwrap_or_default().max(deadline_millis);
        transaction.execute(
            "INSERT INTO metadata(key, value) VALUES ('wayback_cooldown_until_millis', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [deadline.to_string()],
        )?;
        transaction.commit()?;
        Ok(())
    }
}

impl SqliteRetryDeadlinePersistence {
    fn connection(&self) -> Result<Connection> {
        let connection = Connection::open(&self.path)
            .with_context(|| format!("opening retry-deadline catalog {}", self.path.display()))?;
        connection.execute_batch("PRAGMA busy_timeout = 5000;")?;
        Ok(connection)
    }
}

impl crate::wayback_request::RetryDeadlinePersistence for SqliteRetryDeadlinePersistence {
    fn load_deadline(&mut self, now: i64) -> Result<Option<i64>> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_refresh_owner(&transaction, &self.owner, now)?;
        let value = transaction
            .query_row(
                "SELECT value FROM metadata WHERE key = ?1",
                [WAYBACK_RETRY_DEADLINE_KEY],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        transaction.commit()?;
        value
            .map(|value| {
                value
                    .parse::<i64>()
                    .context("invalid Wayback retry deadline")
            })
            .transpose()
    }

    fn save_deadline(&mut self, deadline: i64, now: i64) -> Result<()> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_refresh_owner(&transaction, &self.owner, now)?;
        transaction.execute(
            "INSERT INTO metadata(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![WAYBACK_RETRY_DEADLINE_KEY, deadline.to_string()],
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn clear_if_due(&mut self, now: i64) -> Result<()> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_refresh_owner(&transaction, &self.owner, now)?;
        transaction.execute(
            "DELETE FROM metadata WHERE key = ?1 AND CAST(value AS INTEGER) <= ?2",
            params![WAYBACK_RETRY_DEADLINE_KEY, now],
        )?;
        transaction.commit()?;
        Ok(())
    }
}

fn ensure_refresh_owner(
    transaction: &rusqlite::Transaction<'_>,
    owner: &str,
    now: i64,
) -> Result<()> {
    let active: bool = transaction.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM metadata AS lease
           JOIN metadata AS owner ON owner.key = 'refresh_lease_owner'
           WHERE lease.key = 'refresh_lease_until' AND CAST(lease.value AS INTEGER) > ?1
             AND owner.value = ?2
         )",
        params![now, owner],
        |row| row.get(0),
    )?;
    if !active {
        bail!("refresh lease lost while handling Wayback retry deadline");
    }
    Ok(())
}

fn bump_catalog_generation(connection: &Connection) -> Result<()> {
    connection.execute(
        "INSERT INTO metadata(key, value) VALUES (?1, '1')
         ON CONFLICT(key) DO UPDATE SET value = CAST(CAST(metadata.value AS INTEGER) + 1 AS TEXT)",
        [CATALOG_GENERATION_KEY],
    )?;
    Ok(())
}

fn read_saying(row: &rusqlite::Row<'_>) -> rusqlite::Result<Saying> {
    Ok(Saying {
        id: row.get(0)?,
        entry: NewSaying {
            text: row.get(1)?,
            category: row.get(2)?,
            language: row.get(3)?,
            source: row.get(4)?,
            source_url: row.get(5)?,
            original_url: row.get(6)?,
            captured_at: row.get(7)?,
            group_id: row.get(8)?,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counted_entry(text: &str, category: &str, language: &str) -> NewSaying {
        NewSaying {
            text: text.to_owned(),
            category: category.to_owned(),
            language: language.to_owned(),
            source: "manual".to_owned(),
            source_url: "https://cvc.cervantes.es/lengua/refranero/ficha.aspx?Par=1&Lng=1"
                .to_owned(),
            original_url: None,
            captured_at: None,
            group_id: None,
        }
    }

    #[test]
    fn category_totals_group_by_category_and_order_by_volume() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        let before = store.category_totals().unwrap();
        let base = |id: &str| {
            before
                .iter()
                .find(|(key, _)| key == id)
                .map(|(_, count)| *count)
                .unwrap_or(0)
        };
        let entries = vec![
            counted_entry("A", "refran", "es"),
            counted_entry("B", "refran", "es"),
            counted_entry("C", "refran", "es"),
            counted_entry("D", "cita", "es"),
        ];
        store.insert(&entries).unwrap();

        let totals = store.category_totals().unwrap();
        let refranes = totals.iter().find(|(id, _)| id == "refran").unwrap();
        let citas = totals.iter().find(|(id, _)| id == "cita").unwrap();
        assert_eq!(refranes.1, base("refran") + 3);
        assert_eq!(citas.1, base("cita") + 1);
        // The biggest category comes first.
        assert_eq!(totals.first().map(|(id, _)| id.as_str()), Some("refran"));
    }

    #[test]
    fn language_totals_group_by_language_and_order_by_volume() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        let before = store.language_totals().unwrap();
        let base = |id: &str| {
            before
                .iter()
                .find(|(key, _)| key == id)
                .map(|(_, count)| *count)
                .unwrap_or(0)
        };
        let entries = vec![
            counted_entry("A", "refran", "es"),
            counted_entry("B", "refran", "es"),
            counted_entry("C", "refran", "en"),
        ];
        store.insert(&entries).unwrap();

        let totals = store.language_totals().unwrap();
        assert_eq!(totals.first().map(|(id, _)| id.as_str()), Some("es"));
        assert_eq!(
            totals.first().map(|(_, count)| *count),
            Some(base("es") + 2)
        );
        assert_eq!(
            totals
                .iter()
                .find(|(id, _)| id == "en")
                .map(|(_, count)| *count),
            Some(base("en") + 1)
        );
    }

    #[test]
    fn totals_include_the_seeded_catalog() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        let total: i64 = store
            .category_totals()
            .unwrap()
            .iter()
            .map(|(_, count)| count)
            .sum();
        assert_eq!(total, store.count().unwrap());
        let languages: i64 = store
            .language_totals()
            .unwrap()
            .iter()
            .map(|(_, count)| count)
            .sum();
        assert_eq!(languages, store.count().unwrap());
    }

    #[test]
    fn virtual_snapshot_replacement_preserves_other_sources_and_updates_timestamp() {
        let mut store = Store::open(Path::new(":memory:")).unwrap();
        store.record_successful_refresh(100).unwrap();
        let replacement = NewSaying {
            text: "Nuevo refrán".to_owned(),
            category: "refran".to_owned(),
            language: "es".to_owned(),
            source: "cervantes_virtual".to_owned(),
            source_url: "https://www.cervantesvirtual.com/portales/refranes_dichos_proverbios_citas_adivinanzas/refran/".to_owned(),
            original_url: None,
            captured_at: None,
            group_id: None,
        };

        store.replace_virtual_snapshot(&[replacement], 200).unwrap();

        assert_eq!(store.last_successful_refresh().unwrap(), Some(200));
        assert_eq!(store.count_source("cervantes_virtual").unwrap(), 1);
        assert_eq!(store.count_source("cvc").unwrap(), 5);
        assert!(store
            .random("es", Some("refran"), 20)
            .unwrap()
            .iter()
            .any(|s| s.entry.text == "Nuevo refrán"));
    }

    #[test]
    fn invalid_virtual_snapshot_rolls_back_catalog_and_timestamp() {
        let mut store = Store::open(Path::new(":memory:")).unwrap();
        let previous_count = store.count().unwrap();
        store.record_successful_refresh(100).unwrap();
        let invalid = NewSaying {
            text: "Entrada insegura".to_owned(),
            category: "refran".to_owned(),
            language: "es".to_owned(),
            source: "cervantes_virtual".to_owned(),
            source_url: "https://example.com/evil".to_owned(),
            original_url: None,
            captured_at: None,
            group_id: None,
        };

        assert!(store.replace_virtual_snapshot(&[invalid], 200).is_err());

        assert_eq!(store.count().unwrap(), previous_count);
        assert_eq!(store.last_successful_refresh().unwrap(), Some(100));
    }

    #[test]
    fn empty_virtual_snapshot_does_not_delete_catalog_or_advance_timestamp() {
        let mut store = Store::open(Path::new(":memory:")).unwrap();
        let previous_count = store.count().unwrap();
        store.record_successful_refresh(100).unwrap();

        assert!(store.replace_virtual_snapshot(&[], 200).is_err());

        assert_eq!(store.count().unwrap(), previous_count);
        assert_eq!(store.last_successful_refresh().unwrap(), Some(100));
    }

    #[test]
    fn stores_last_successful_refresh_timestamp() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        assert_eq!(store.last_successful_refresh().unwrap(), None);

        store.record_successful_refresh(1_700_000_000).unwrap();
        assert_eq!(
            store.last_successful_refresh().unwrap(),
            Some(1_700_000_000)
        );

        store.record_successful_refresh(1_700_000_100).unwrap();
        assert_eq!(
            store.last_successful_refresh().unwrap(),
            Some(1_700_000_100)
        );
    }

    #[test]
    fn initializes_small_offline_catalog_and_randomizes() {
        let conn = Store::open(Path::new(":memory:")).unwrap();
        assert!(conn.count().unwrap() >= 10);
        assert_eq!(conn.random("xx", None, 3).unwrap().len(), 0);
        assert!(!conn.random("es", Some("refran"), 1).unwrap().is_empty());
    }

    #[test]
    fn bundled_legacy_cvc_entries_have_openable_reference_urls() {
        let seeds: Vec<NewSaying> =
            serde_json::from_str(include_str!("../data/seed.json")).unwrap();
        let cvc_seed = |text: &str| {
            seeds
                .iter()
                .find(|entry| entry.source == "cvc" && entry.text == text)
                .unwrap_or_else(|| panic!("missing seed entry: {text}"))
        };

        assert!(cvc_seed("He that would learn to pray, let him go to sea")
            .source_url
            .starts_with("https://idiomingo.com/"));
        assert!(cvc_seed("Si Déu vols cridar, fica't a la mar")
            .source_url
            .starts_with("https://pccd.dites.cat/"));
        for (text, timestamp) in [
            ("A buen entendedor, pocas palabras bastan", "20261011171016"),
            ("A quien madruga, Dios le ayuda", "20230522065136"),
            ("Haz bien y no mires a quién", "20240511091123"),
        ] {
            let entry = cvc_seed(text);
            assert!(entry
                .source_url
                .starts_with(&format!("https://web.archive.org/web/{timestamp}/")));
            assert!(entry
                .original_url
                .as_deref()
                .is_some_and(|url| url.contains("cvc.cervantes.es")));
            assert!(entry.captured_at.as_deref() == Some(timestamp));
        }
    }

    #[test]
    fn bundled_current_portal_entries_link_to_their_cited_references() {
        let seeds: Vec<NewSaying> =
            serde_json::from_str(include_str!("../data/seed.json")).unwrap();
        let portal_entries: Vec<_> = seeds
            .iter()
            .filter(|entry| entry.source == "cervantes_virtual")
            .collect();

        assert_eq!(portal_entries.len(), 11);
        for entry in portal_entries {
            let source = url::Url::parse(&entry.source_url).unwrap();
            assert_eq!(source.scheme(), "https");
            assert_eq!(source.host_str(), Some("www.cervantesvirtual.com"));
            assert!(
                source.path().starts_with("/obra/") || source.path().starts_with("/obra-visor/")
            );
            assert!(entry.source_url != entry.original_url.as_deref().unwrap_or_default());
            assert!(entry
                .original_url
                .as_deref()
                .is_some_and(|url| url.contains("/portales/")));
        }
    }

    #[test]
    fn opening_an_older_catalog_migrates_seed_urls_and_invalidates_the_portal_snapshot() {
        let path = std::env::temp_dir().join(format!(
            "refranero-source-link-migration-{}.sqlite3",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let old_cvc_url = "https://cvc.cervantes.es/lengua/refranero/ficha.aspx?Lng=0&Par=58807";
        let old_portal_url =
            "https://www.cervantesvirtual.com/portales/refranes_dichos_proverbios_citas_adivinanzas/refran/";
        let user_entry = NewSaying {
            text: "User-added record".into(),
            category: "refran".into(),
            language: "es".into(),
            source: "manual".into(),
            source_url: "https://example.org/user-entry".into(),
            original_url: None,
            captured_at: None,
            group_id: None,
        };

        {
            let store = Store::open(&path).unwrap();
            store.insert(&[user_entry]).unwrap();
            let legacy_cvc_id = store
                .random("es", Some("refran"), 50)
                .unwrap()
                .into_iter()
                .find(|saying| saying.entry.text == "Haz bien y no mires a quién")
                .unwrap()
                .id;
            let legacy_portal_id = store
                .random("es", Some("refran"), 50)
                .unwrap()
                .into_iter()
                .find(|saying| saying.entry.text == "Los truenos y la mar enseñan a rezar.")
                .unwrap()
                .id;
            store
                .connection
                .execute(
                    "UPDATE sayings SET source_url = ?1, captured_at = NULL WHERE group_id = 'cvc:58807'",
                    [old_cvc_url],
                )
                .unwrap();
            store
                .connection
                .execute(
                    "INSERT INTO metadata(key, value) VALUES ('test_legacy_cvc_id', ?1), ('test_legacy_portal_id', ?2)",
                    params![legacy_cvc_id.to_string(), legacy_portal_id.to_string()],
                )
                .unwrap();
            store
                .connection
                .execute(
                    "UPDATE sayings SET source_url = ?1 WHERE text = ?2 AND source = 'cervantes_virtual'",
                    params![old_portal_url, "Los truenos y la mar enseñan a rezar."],
                )
                .unwrap();
            store
                .connection
                .execute(
                    "DELETE FROM metadata WHERE key = ?1",
                    [SOURCE_LINK_MIGRATION_KEY],
                )
                .unwrap();
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64;
            store.record_successful_refresh(now).unwrap();
        }

        let store = Store::open(&path).unwrap();
        let legacy_cvc = store
            .random("es", Some("refran"), 50)
            .unwrap()
            .into_iter()
            .find(|saying| saying.entry.text == "Haz bien y no mires a quién")
            .unwrap();
        let legacy_portal = store
            .random("es", Some("refran"), 50)
            .unwrap()
            .into_iter()
            .find(|saying| saying.entry.text == "Los truenos y la mar enseñan a rezar.")
            .unwrap();

        assert!(legacy_cvc
            .entry
            .source_url
            .starts_with("https://web.archive.org/web/20240511091123/"));
        assert_eq!(
            legacy_cvc.entry.captured_at.as_deref(),
            Some("20240511091123")
        );
        assert_eq!(
            legacy_portal.entry.source_url,
            "https://www.cervantesvirtual.com/obra-visor/refranes-alusivos-al-tiempo/html/"
        );
        let legacy_cvc_id: i64 = store
            .connection
            .query_row(
                "SELECT value FROM metadata WHERE key = 'test_legacy_cvc_id'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap()
            .parse()
            .unwrap();
        let legacy_portal_id: i64 = store
            .connection
            .query_row(
                "SELECT value FROM metadata WHERE key = 'test_legacy_portal_id'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(legacy_cvc.id, legacy_cvc_id);
        assert_eq!(legacy_portal.id, legacy_portal_id);
        assert_eq!(store.last_successful_refresh().unwrap(), Some(0));
        assert_eq!(store.count().unwrap(), 17);
        assert_eq!(store.count_source("manual").unwrap(), 1);
        store.record_successful_refresh(123).unwrap();
        drop(store);

        let reopened = Store::open(&path).unwrap();
        assert_eq!(reopened.last_successful_refresh().unwrap(), Some(123));
        assert_eq!(reopened.count().unwrap(), 17);

        drop(reopened);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn rejects_unsafe_local_urls() {
        let conn = Store::open(Path::new(":memory:")).unwrap();
        let unsafe_entry = NewSaying {
            text: "Sample".into(),
            category: "refran".into(),
            language: "es".into(),
            source: "fixture".into(),
            source_url: "javascript:alert(1)".into(),
            original_url: None,
            captured_at: None,
            group_id: None,
        };
        assert!(conn.insert(&[unsafe_entry]).is_err());
    }

    #[test]
    fn reimport_is_idempotent() {
        let conn = Store::open(Path::new(":memory:")).unwrap();
        let seeds: Vec<NewSaying> =
            serde_json::from_str(include_str!("../data/seed.json")).unwrap();
        assert_eq!(conn.insert(&seeds).unwrap(), 0);
    }

    #[test]
    fn identical_text_in_different_groups_is_preserved() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        let first = NewSaying {
            text: "Shared text".into(),
            category: "paremia".into(),
            language: "es".into(),
            source: "cvc_wayback".into(),
            source_url: "https://cvc.cervantes.es/a".into(),
            original_url: None,
            captured_at: None,
            group_id: Some("cvc:1".into()),
        };
        let second = NewSaying {
            group_id: Some("cvc:2".into()),
            source_url: "https://cvc.cervantes.es/b".into(),
            ..first.clone()
        };

        assert_eq!(store.insert(&[first, second]).unwrap(), 2);
        assert_eq!(store.by_group("cvc:1").unwrap().len(), 1);
        assert_eq!(store.by_group("cvc:2").unwrap().len(), 1);
    }

    #[test]
    fn legacy_group_uniqueness_migration_preserves_rows_and_resets_bootstrap() {
        let path = std::env::temp_dir().join(format!(
            "refranero-group-migration-{}.sqlite3",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        {
            let connection = Connection::open(&path).unwrap();
            connection.execute_batch("CREATE TABLE sayings (id INTEGER PRIMARY KEY, text TEXT NOT NULL, category TEXT NOT NULL, language TEXT NOT NULL, source TEXT NOT NULL, source_url TEXT NOT NULL, original_url TEXT, captured_at TEXT, group_id TEXT, UNIQUE(text, category, language, source)); CREATE INDEX idx_sayings_category_lang ON sayings(category, language); CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL); INSERT INTO sayings VALUES (77, 'Legacy', 'paremia', 'es', 'cvc_wayback', 'https://example.org/a', 'https://example.org/original', '20200101000000', 'cvc:77'); INSERT INTO metadata VALUES ('wayback_bootstrap_complete', 'true'), ('wayback_cursor', 'Z');").unwrap();
        }
        let store = Store::open(&path).unwrap();
        let row = store.get(77).unwrap().unwrap();
        assert_eq!(row.entry.group_id.as_deref(), Some("cvc:77"));
        assert_eq!(row.entry.captured_at.as_deref(), Some("20200101000000"));
        assert_eq!(store.wayback_bootstrap_next().unwrap(), Some('A'));
        assert_eq!(store.wayback_cursor().unwrap(), None);
        let nullable = counted_entry("Nullable", "refran", "es");
        assert_eq!(store.insert(std::slice::from_ref(&nullable)).unwrap(), 1);
        assert_eq!(store.insert(&[nullable]).unwrap(), 0);
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn captured_spanish_ficha_updates_existing_index_provenance_only_when_marked() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        let fallback = NewSaying {
            text: "Haz bien y no mires a quién".into(),
            category: "paremia".into(),
            language: "es".into(),
            source: "cvc_wayback".into(),
            source_url: "https://web.archive.org/web/20240511091123/https://cvc.cervantes.es/lengua/refranero/ficha.aspx?Par=58807&Lng=0".into(),
            original_url: Some(
                "https://cvc.cervantes.es/lengua/refranero/ficha.aspx?Par=58807&Lng=0".into(),
            ),
            captured_at: Some("20240511091123".into()),
            group_id: Some("cvc:58807".into()),
        };
        assert_eq!(store.insert(std::slice::from_ref(&fallback)).unwrap(), 1);

        let captured = NewSaying {
            source_url: "https://web.archive.org/web/20240512091123/https://cvc.cervantes.es/lengua/refranero/ficha.aspx?Par=58807&Lng=0".into(),
            captured_at: Some("20240512091123".into()),
            ..fallback.clone()
        };
        let captured_pars = std::collections::HashSet::from(["58807".to_string()]);
        assert_eq!(
            store.insert_wayback(&[captured], &captured_pars).unwrap(),
            0
        );

        let later_fallback = NewSaying {
            source_url: "https://web.archive.org/web/20240513091123/https://cvc.cervantes.es/lengua/refranero/ficha.aspx?Par=58807&Lng=0".into(),
            captured_at: Some("20240513091123".into()),
            ..fallback
        };
        store
            .insert_wayback(&[later_fallback], &std::collections::HashSet::new())
            .unwrap();

        let stored: (String, Option<String>) = store
            .connection
            .query_row(
                "SELECT source_url, captured_at FROM sayings WHERE text = ?1 AND source = 'cvc_wayback' AND language = 'es'",
                ["Haz bien y no mires a quién"],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert!(stored
            .0
            .starts_with("https://web.archive.org/web/20240512091123/"));
        assert_eq!(stored.1.as_deref(), Some("20240512091123"));
    }

    #[test]
    fn file_catalogs_use_wal_and_readers_survive_a_writer() {
        let path = std::env::temp_dir().join(format!(
            "refranero-wal-{}-{:?}.sqlite3",
            std::process::id(),
            std::thread::current().id()
        ));
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }

        let reader = Store::open(&path).unwrap();
        let mut writer = Store::open(&path).unwrap();

        let journal: String = reader
            .connection
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(journal.to_lowercase(), "wal");
        let timeout: i64 = reader
            .connection
            .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .unwrap();
        assert_eq!(timeout, 5000);

        // An open write transaction must not block a reader.
        let transaction = writer.connection.transaction().unwrap();
        transaction
            .execute("UPDATE sayings SET text = text WHERE id = 1", [])
            .unwrap();
        assert!(reader.count().unwrap() > 0);
        transaction.rollback().unwrap();

        drop(reader);
        drop(writer);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }

    #[test]
    fn finds_multilingual_variants_by_group() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        let variants = store.by_group("cvc:59530").unwrap();
        let languages: Vec<&str> = variants
            .iter()
            .map(|saying| saying.entry.language.as_str())
            .collect();
        assert_eq!(languages, ["ca", "en"]);
        assert!(store.by_group("cvc:missing").unwrap().is_empty());
    }

    #[test]
    fn publishes_and_clears_refresh_progress_only_for_the_lease_owner() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        assert!(store.refresh_progress().unwrap().is_none());
        assert!(store.try_acquire_refresh_lease(1_000, "worker-a").unwrap());

        let progress = RefreshProgress {
            phase: "Wayback".into(),
            done: 3,
            total: 10,
            updated_at: 100,
        };
        store.set_refresh_progress(&progress).unwrap();
        assert_eq!(store.refresh_progress().unwrap(), Some(progress.clone()));

        store.clear_refresh_progress("worker-b").unwrap();
        assert_eq!(
            store.refresh_progress().unwrap(),
            Some(progress),
            "another owner must not clear the bar"
        );

        store.clear_refresh_progress("worker-a").unwrap();
        assert!(store.refresh_progress().unwrap().is_none());
    }

    #[test]
    fn corrupt_progress_reads_as_none() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        store.set_metadata("refresh_progress", "not json").unwrap();
        assert!(store.refresh_progress().unwrap().is_none());
    }

    #[test]
    fn records_and_clears_the_last_refresh_error() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        assert!(store.refresh_error().unwrap().is_none());

        store.record_refresh_error("network down", 42).unwrap();
        assert_eq!(
            store.refresh_error().unwrap(),
            Some(RefreshError {
                message: "network down".into(),
                updated_at: 42,
            })
        );

        store.clear_refresh_error().unwrap();
        assert!(store.refresh_error().unwrap().is_none());

        store
            .set_metadata("last_refresh_error", "not json")
            .unwrap();
        assert!(store.refresh_error().unwrap().is_none());
    }

    #[test]
    fn stores_wayback_marker_and_cursor_atomically() {
        let mut store = Store::open(Path::new(":memory:")).unwrap();
        assert_eq!(store.last_wayback_refresh().unwrap(), None);
        assert_eq!(store.wayback_cursor().unwrap(), None);

        store.record_wayback_success('C', 42).unwrap();
        assert_eq!(store.last_wayback_refresh().unwrap(), Some(42));
        assert_eq!(store.wayback_cursor().unwrap(), Some('C'));
    }

    #[test]
    fn wayback_bootstrap_starts_at_a_and_advances_only_after_success() {
        let mut store = Store::open(Path::new(":memory:")).unwrap();

        assert_eq!(store.wayback_bootstrap_next().unwrap(), Some('A'));
        assert!(store.record_wayback_bootstrap_success('?', 41).is_err());
        assert_eq!(store.wayback_bootstrap_next().unwrap(), Some('A'));
        store.record_wayback_bootstrap_success('A', 42).unwrap();
        assert_eq!(store.wayback_bootstrap_next().unwrap(), Some('B'));
        assert_eq!(store.wayback_cursor().unwrap(), Some('A'));
    }

    #[test]
    fn resumes_wayback_bootstrap_after_reopening_the_catalog() {
        let path = std::env::temp_dir().join(format!(
            "refranero-wayback-bootstrap-{}-{:?}.sqlite3",
            std::process::id(),
            std::thread::current().id()
        ));
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }

        {
            let mut store = Store::open(&path).unwrap();
            store.record_wayback_bootstrap_success('A', 42).unwrap();
        }

        let store = Store::open(&path).unwrap();
        assert_eq!(store.wayback_bootstrap_next().unwrap(), Some('B'));
        drop(store);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }

    #[test]
    fn wayback_bootstrap_is_complete_only_after_z() {
        let mut store = Store::open(Path::new(":memory:")).unwrap();
        let mut processed = Vec::new();
        for letter in 'A'..='Y' {
            store
                .record_wayback_bootstrap_success(letter, i64::from(letter as u32))
                .unwrap();
            processed.push(letter);
        }
        assert_eq!(store.wayback_bootstrap_next().unwrap(), Some('Z'));

        store.record_wayback_bootstrap_success('Z', 100).unwrap();
        processed.push('Z');
        assert_eq!(processed, ('A'..='Z').collect::<Vec<_>>());
        assert_eq!(store.wayback_bootstrap_next().unwrap(), None);
        assert_eq!(store.wayback_cursor().unwrap(), Some('Z'));
    }
}

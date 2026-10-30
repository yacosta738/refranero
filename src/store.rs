//! SQLite storage adapter. All reads and writes are local.
use crate::domain::{NewSaying, Saying};
use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

/// Owns an initialized SQLite connection.
pub struct Store {
    connection: Connection,
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
               group_id TEXT,
               UNIQUE(text, category, language, source)
             );
             CREATE INDEX IF NOT EXISTS idx_sayings_category_lang
               ON sayings(category, language);
             CREATE TABLE IF NOT EXISTS metadata (
               key TEXT PRIMARY KEY,
               value TEXT NOT NULL
             );",
        )?;
        let store = Self { connection };
        if store.count()? == 0 {
            let seeds: Vec<NewSaying> = serde_json::from_str(include_str!("../data/seed.json"))?;
            store.insert(&seeds)?;
        }
        Ok(store)
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

    /// Acquire the background refresh lease if no active worker owns it.
    pub fn try_acquire_refresh_lease(&mut self, now: i64, owner: &str) -> Result<bool> {
        let transaction = self.connection.transaction()?;
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
            inserted
        };
        transaction.commit()?;
        Ok(inserted)
    }

    /// Insert only unseen tuples. Returns the number of new rows.
    pub fn insert(&self, entries: &[NewSaying]) -> Result<usize> {
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
        let mut created = 0;
        let mut stmt = self.connection.prepare_cached(
            "INSERT OR IGNORE INTO sayings
             (text, category, language, source, source_url, original_url, captured_at, group_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        for e in entries {
            if e.text.trim().is_empty() || e.text.chars().count() > 3000 {
                continue;
            }
            created += stmt.execute(params![
                e.text,
                e.category,
                e.language,
                e.source,
                e.source_url,
                e.original_url,
                e.captured_at,
                e.group_id
            ])?;
        }
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
}

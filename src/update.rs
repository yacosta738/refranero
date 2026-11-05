//! Background update orchestration shared by the automatic and manual refresh paths.

use anyhow::{anyhow, Result};

use crate::refresh::{refresh_decision, wayback_refresh_decision, RefreshDecision};
use crate::store::Store;

/// Sources that one worker run must refresh.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpdateScope {
    /// Refresh the current Cervantes Virtual snapshot.
    pub virtual_source: bool,
    /// Refresh the archived CVC index through Wayback.
    pub wayback: bool,
}

impl UpdateScope {
    /// Whether the worker has nothing to do.
    pub const fn is_empty(self) -> bool {
        !self.virtual_source && !self.wayback
    }
}

/// Decide which sources a worker must run.
///
/// `force` is the manual update path: it refreshes every allowed source even when the cache is
/// still fresh. The automatic path keeps the normal expiry policy.
pub fn update_scope(
    last_virtual: Option<i64>,
    last_wayback: Option<i64>,
    now: i64,
    allow_wayback: bool,
    force: bool,
) -> UpdateScope {
    UpdateScope {
        virtual_source: force
            || refresh_decision(last_virtual, now, false) == RefreshDecision::Stale,
        wayback: allow_wayback
            && (force
                || wayback_refresh_decision(last_wayback, now, false) == RefreshDecision::Stale),
    }
}

/// Run the selected sources, clear published progress and release the lease.
///
/// A current-portal failure never cancels a due Wayback run. A lost lease always cancels the next
/// source, so a stale worker cannot keep writing. The first error is returned after cleanup.
pub fn run_worker<FV, FW>(
    store: &mut Store,
    owner: &str,
    scope: UpdateScope,
    refresh_virtual: FV,
    refresh_wayback: FW,
) -> Result<()>
where
    FV: FnOnce(&mut Store, &str) -> Result<()>,
    FW: FnOnce(&mut Store, &str) -> Result<()>,
{
    if scope.is_empty() {
        return store.release_refresh_lease(owner);
    }

    let mut first_error: Option<anyhow::Error> = None;
    if scope.virtual_source {
        if let Err(error) = refresh_virtual(store, owner) {
            first_error = Some(error);
        }
    }
    if scope.wayback {
        match store.renew_refresh_lease(unix_timestamp(), owner) {
            Ok(true) => {
                if let Err(error) = refresh_wayback(store, owner) {
                    first_error.get_or_insert(error);
                }
            }
            Ok(false) => {
                first_error
                    .get_or_insert_with(|| anyhow!("refresh lease lost before the Wayback run"));
            }
            Err(error) => {
                first_error.get_or_insert(error);
            }
        }
    }

    match first_error.as_ref() {
        Some(error) => {
            let _ = store.record_refresh_error(&error.to_string(), unix_timestamp());
        }
        None => {
            let _ = store.clear_refresh_error();
        }
    }

    let cleared = store.clear_refresh_progress(owner);
    let released = store.release_refresh_lease(owner);
    if let Some(error) = first_error {
        return Err(error);
    }
    cleared?;
    released?;
    Ok(())
}

fn unix_timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::{run_worker, update_scope, UpdateScope};
    use crate::refresh::{REFRESH_INTERVAL_SECONDS, WAYBACK_REFRESH_INTERVAL_SECONDS};
    use crate::store::{RefreshProgress, Store};

    const NOW: i64 = 1_700_000_000;

    fn fresh_virtual() -> Option<i64> {
        Some(NOW - 10)
    }

    fn fresh_wayback() -> Option<i64> {
        Some(NOW - 10)
    }

    fn store_with_lease() -> Store {
        let store = Store::open(std::path::Path::new(":memory:")).unwrap();
        assert!(store
            .try_acquire_refresh_lease(super::unix_timestamp(), "worker")
            .unwrap());
        store
    }

    #[test]
    fn a_forced_run_refreshes_every_allowed_source_even_when_fresh() {
        let scope = update_scope(fresh_virtual(), fresh_wayback(), NOW, true, true);
        assert_eq!(
            scope,
            UpdateScope {
                virtual_source: true,
                wayback: true,
            }
        );

        let scope = update_scope(fresh_virtual(), fresh_wayback(), NOW, false, true);
        assert_eq!(
            scope,
            UpdateScope {
                virtual_source: true,
                wayback: false,
            },
            "Wayback must stay optional"
        );
    }

    #[test]
    fn a_fresh_catalog_without_force_has_nothing_to_do() {
        let scope = update_scope(fresh_virtual(), fresh_wayback(), NOW, true, false);
        assert!(scope.is_empty());
    }

    #[test]
    fn only_stale_sources_are_scheduled() {
        let stale_virtual = Some(NOW - REFRESH_INTERVAL_SECONDS - 1);
        let scope = update_scope(stale_virtual, fresh_wayback(), NOW, true, false);
        assert_eq!(
            scope,
            UpdateScope {
                virtual_source: true,
                wayback: false,
            }
        );

        let stale_wayback = Some(NOW - WAYBACK_REFRESH_INTERVAL_SECONDS - 1);
        let scope = update_scope(fresh_virtual(), stale_wayback, NOW, true, false);
        assert_eq!(
            scope,
            UpdateScope {
                virtual_source: false,
                wayback: true,
            }
        );
    }

    #[test]
    fn wayback_only_runs_when_allowed() {
        let stale_wayback = Some(NOW - WAYBACK_REFRESH_INTERVAL_SECONDS - 1);
        let scope = update_scope(fresh_virtual(), stale_wayback, NOW, false, false);
        assert_eq!(
            scope,
            UpdateScope {
                virtual_source: false,
                wayback: false,
            }
        );
    }

    #[test]
    fn forced_worker_runs_both_sources_clears_progress_and_releases_the_lease() {
        let mut store = store_with_lease();
        store
            .set_refresh_progress(&RefreshProgress {
                phase: "Preparando".into(),
                done: 0,
                total: 0,
                updated_at: NOW,
            })
            .unwrap();
        let virtual_called = Cell::new(false);
        let wayback_called = Cell::new(false);

        run_worker(
            &mut store,
            "worker",
            UpdateScope {
                virtual_source: true,
                wayback: true,
            },
            |_, _| {
                virtual_called.set(true);
                Ok(())
            },
            |_, _| {
                wayback_called.set(true);
                Ok(())
            },
        )
        .unwrap();

        assert!(virtual_called.get());
        assert!(wayback_called.get());
        assert!(store.refresh_progress().unwrap().is_none());
        assert!(
            store.try_acquire_refresh_lease(NOW + 1, "other").unwrap(),
            "the lease must be released"
        );
    }

    #[test]
    fn a_virtual_failure_does_not_cancel_wayback() {
        let mut store = store_with_lease();
        let wayback_called = Cell::new(false);

        let result = run_worker(
            &mut store,
            "worker",
            UpdateScope {
                virtual_source: true,
                wayback: true,
            },
            |_, _| anyhow::bail!("virtual failed"),
            |_, _| {
                wayback_called.set(true);
                Ok(())
            },
        );

        assert!(wayback_called.get());
        assert!(result.unwrap_err().to_string().contains("virtual failed"));
        assert!(store.try_acquire_refresh_lease(NOW + 1, "other").unwrap());
    }

    #[test]
    fn a_lost_lease_cancels_wayback() {
        let mut store = store_with_lease();
        let wayback_called = Cell::new(false);

        let result = run_worker(
            &mut store,
            "worker",
            UpdateScope {
                virtual_source: true,
                wayback: true,
            },
            |store, owner| store.release_refresh_lease(owner),
            |_, _| {
                wayback_called.set(true);
                Ok(())
            },
        );

        assert!(!wayback_called.get(), "a stale worker must not crawl");
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("lease lost before the Wayback run"));
    }

    #[test]
    fn a_failed_run_publishes_the_error_for_the_reader() {
        let mut store = store_with_lease();

        let result = run_worker(
            &mut store,
            "worker",
            UpdateScope {
                virtual_source: true,
                wayback: false,
            },
            |_, _| anyhow::bail!("network down"),
            |_, _| Ok(()),
        );

        assert!(result.is_err());
        let error = store.refresh_error().unwrap().expect("published error");
        assert_eq!(error.message, "network down");
    }

    #[test]
    fn a_successful_run_clears_a_previous_error() {
        let mut store = store_with_lease();
        store.record_refresh_error("old failure", 1).unwrap();

        run_worker(
            &mut store,
            "worker",
            UpdateScope {
                virtual_source: true,
                wayback: false,
            },
            |_, _| Ok(()),
            |_, _| Ok(()),
        )
        .unwrap();

        assert!(store.refresh_error().unwrap().is_none());
    }

    #[test]
    fn an_empty_scope_only_releases_the_lease() {
        let mut store = store_with_lease();

        run_worker(
            &mut store,
            "worker",
            UpdateScope {
                virtual_source: false,
                wayback: false,
            },
            |_, _| panic!("Virtual must not run"),
            |_, _| panic!("Wayback must not run"),
        )
        .unwrap();

        assert!(store.try_acquire_refresh_lease(NOW + 1, "other").unwrap());
    }
}

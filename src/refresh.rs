pub const REFRESH_INTERVAL_SECONDS: i64 = 24 * 60 * 60;
pub const REFRESH_LEASE_SECONDS: i64 = 5 * 60;

pub fn is_refresh_stale(last_successful_refresh: Option<i64>, now: i64) -> bool {
    last_successful_refresh
        .map(|last| now.saturating_sub(last) > REFRESH_INTERVAL_SECONDS)
        .unwrap_or(true)
}

#[derive(Debug, PartialEq, Eq)]
pub enum RefreshDecision {
    Offline,
    Fresh,
    Stale,
}

pub fn refresh_decision(
    last_successful_refresh: Option<i64>,
    now: i64,
    offline: bool,
) -> RefreshDecision {
    if offline {
        RefreshDecision::Offline
    } else if is_refresh_stale(last_successful_refresh, now) {
        RefreshDecision::Stale
    } else {
        RefreshDecision::Fresh
    }
}

#[cfg(test)]
mod tests {
    use super::{is_refresh_stale, refresh_decision, RefreshDecision, REFRESH_INTERVAL_SECONDS};

    #[test]
    fn cache_is_fresh_at_exactly_24_hours_and_stale_afterward() {
        assert!(!is_refresh_stale(
            Some(1_000),
            1_000 + REFRESH_INTERVAL_SECONDS
        ));
        assert!(is_refresh_stale(
            Some(1_000),
            1_001 + REFRESH_INTERVAL_SECONDS
        ));
    }

    #[test]
    fn never_refreshed_catalog_is_stale() {
        assert!(is_refresh_stale(None, 1_000));
    }

    #[test]
    fn offline_and_fresh_cache_do_not_request_refresh() {
        assert_eq!(
            refresh_decision(None, 1_000, true),
            RefreshDecision::Offline
        );
        assert_eq!(
            refresh_decision(Some(1_000), 1_000 + REFRESH_INTERVAL_SECONDS, false),
            RefreshDecision::Fresh
        );
    }

    #[test]
    fn concurrent_invocations_request_only_one_worker() {
        let mut store = crate::store::Store::open(std::path::Path::new(":memory:")).unwrap();
        assert!(store.try_acquire_refresh_lease(10_000, "worker-a").unwrap());
        assert!(!store.try_acquire_refresh_lease(10_001, "worker-b").unwrap());
        store.release_refresh_lease("worker-b").unwrap();
        assert!(!store.try_acquire_refresh_lease(10_002, "worker-c").unwrap());
        assert!(store.renew_refresh_lease(10_003, "worker-a").unwrap());
    }

    #[test]
    fn expired_worker_lease_can_be_reclaimed() {
        let mut store = crate::store::Store::open(std::path::Path::new(":memory:")).unwrap();
        assert!(store.try_acquire_refresh_lease(10_000, "worker-a").unwrap());
        assert!(store
            .try_acquire_refresh_lease(10_000 + super::REFRESH_LEASE_SECONDS + 1, "worker-b")
            .unwrap());
        assert!(!store.renew_refresh_lease(20_000, "worker-a").unwrap());
        store.release_refresh_lease("worker-a").unwrap();
        assert!(store.try_acquire_refresh_lease(20_001, "worker-c").unwrap());
    }

    #[test]
    fn stale_cache_requests_background_refresh() {
        assert_eq!(refresh_decision(None, 1_000, false), RefreshDecision::Stale);
    }
}

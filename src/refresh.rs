/// Cache lifetime for the current-portal snapshot.
pub const REFRESH_INTERVAL_SECONDS: i64 = 24 * 60 * 60;
/// Wayback is a heavier crawl, so it refreshes on its own slower cycle.
pub const WAYBACK_REFRESH_INTERVAL_SECONDS: i64 = 7 * 24 * 60 * 60;
/// How long a worker owns the refresh lease before another worker may take over.
pub const REFRESH_LEASE_SECONDS: i64 = 5 * 60;
/// Fichas per automatic Wayback run. Manual `sync wayback` keeps its own limit.
pub const AUTOMATIC_WAYBACK_LIMIT: usize = 10;

/// Whether a cache last refreshed at `last` is older than `interval`.
pub fn is_stale_after(last_successful_refresh: Option<i64>, now: i64, interval: i64) -> bool {
    last_successful_refresh
        .map(|last| now.saturating_sub(last) > interval)
        .unwrap_or(true)
}

/// Whether the current-portal snapshot is older than its daily interval.
pub fn is_refresh_stale(last_successful_refresh: Option<i64>, now: i64) -> bool {
    is_stale_after(last_successful_refresh, now, REFRESH_INTERVAL_SECONDS)
}

/// Whether the Wayback snapshot is older than its weekly interval.
pub fn is_wayback_stale(last_successful_refresh: Option<i64>, now: i64) -> bool {
    is_stale_after(
        last_successful_refresh,
        now,
        WAYBACK_REFRESH_INTERVAL_SECONDS,
    )
}

/// Calendar year of a Unix timestamp, used to ask Wayback for a recent capture.
pub fn year_from_unix(timestamp: i64) -> u16 {
    // Howard Hinnant's civil_from_days, with no date dependency.
    let days = timestamp.div_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year_of_era = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 {
        year_of_era + 1
    } else {
        year_of_era
    };
    year.clamp(1970, 2100) as u16
}

/// Next letter of the automatic Wayback cursor, wrapping from Z to A.
pub fn next_letter(current: Option<char>) -> char {
    match current.map(|letter| letter.to_ascii_uppercase()) {
        Some('Z') | None => 'A',
        Some(letter) if letter.is_ascii_alphabetic() => char::from(letter as u8 + 1),
        Some(_) => 'A',
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum RefreshDecision {
    Offline,
    Fresh,
    Stale,
}

/// Decide whether the current-portal snapshot needs a background refresh.
pub fn refresh_decision(
    last_successful_refresh: Option<i64>,
    now: i64,
    offline: bool,
) -> RefreshDecision {
    decision(is_refresh_stale(last_successful_refresh, now), offline)
}

/// Same decision shape for the slower Wayback cycle.
pub fn wayback_refresh_decision(
    last_successful_refresh: Option<i64>,
    now: i64,
    offline: bool,
) -> RefreshDecision {
    decision(is_wayback_stale(last_successful_refresh, now), offline)
}

const fn decision(stale: bool, offline: bool) -> RefreshDecision {
    if offline {
        RefreshDecision::Offline
    } else if stale {
        RefreshDecision::Stale
    } else {
        RefreshDecision::Fresh
    }
}

#[cfg(test)]
mod tests {
    use super::{
        is_refresh_stale, is_wayback_stale, next_letter, refresh_decision,
        wayback_refresh_decision, year_from_unix, RefreshDecision, REFRESH_INTERVAL_SECONDS,
        WAYBACK_REFRESH_INTERVAL_SECONDS,
    };

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
        let store = crate::store::Store::open(std::path::Path::new(":memory:")).unwrap();
        assert!(store.try_acquire_refresh_lease(10_000, "worker-a").unwrap());
        assert!(!store.try_acquire_refresh_lease(10_001, "worker-b").unwrap());
        store.release_refresh_lease("worker-b").unwrap();
        assert!(!store.try_acquire_refresh_lease(10_002, "worker-c").unwrap());
        assert!(store.renew_refresh_lease(10_003, "worker-a").unwrap());
    }

    #[test]
    fn expired_worker_lease_can_be_reclaimed() {
        let store = crate::store::Store::open(std::path::Path::new(":memory:")).unwrap();
        assert!(store.try_acquire_refresh_lease(10_000, "worker-a").unwrap());
        assert!(store
            .try_acquire_refresh_lease(10_000 + super::REFRESH_LEASE_SECONDS + 1, "worker-b")
            .unwrap());
        assert!(!store.renew_refresh_lease(20_000, "worker-a").unwrap());
        store.release_refresh_lease("worker-a").unwrap();
        assert!(store.try_acquire_refresh_lease(20_001, "worker-c").unwrap());
    }

    #[test]
    fn wayback_uses_a_weekly_cycle() {
        assert!(!is_wayback_stale(
            Some(1_000),
            1_000 + REFRESH_INTERVAL_SECONDS
        ));
        assert!(is_wayback_stale(
            Some(1_000),
            1_001 + WAYBACK_REFRESH_INTERVAL_SECONDS
        ));
        assert_eq!(
            wayback_refresh_decision(None, 1_000, false),
            RefreshDecision::Stale
        );
        assert_eq!(
            wayback_refresh_decision(None, 1_000, true),
            RefreshDecision::Offline
        );
    }

    #[test]
    fn converts_timestamps_to_calendar_years() {
        assert_eq!(year_from_unix(0), 1970);
        assert_eq!(year_from_unix(1_700_000_000), 2023);
        assert_eq!(year_from_unix(1_767_225_600), 2026);
    }

    #[test]
    fn wayback_cursor_advances_and_wraps() {
        assert_eq!(next_letter(None), 'A');
        assert_eq!(next_letter(Some('A')), 'B');
        assert_eq!(next_letter(Some('c')), 'D');
        assert_eq!(next_letter(Some('Z')), 'A');
        assert_eq!(next_letter(Some('?')), 'A');
    }

    #[test]
    fn stale_cache_requests_background_refresh() {
        assert_eq!(refresh_decision(None, 1_000, false), RefreshDecision::Stale);
    }
}

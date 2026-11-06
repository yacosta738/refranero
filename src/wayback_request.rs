//! Control secuencial de solicitudes Wayback, con esperas cancelables.
use std::time::{Duration, Instant, SystemTime};

use anyhow::{bail, Result};
use reqwest::blocking::{Client, Response};

const MIN_PAUSE: Duration = Duration::from_millis(1200);
const WAIT_BUDGET: Duration = Duration::from_secs(120);
const MAX_ATTEMPTS: usize = 3;

pub trait Clock {
    fn now(&self) -> Duration;
    fn wait(&mut self, duration: Duration);

    fn unix_now(&self) -> i64 {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|duration| i64::try_from(duration.as_secs()).unwrap_or(i64::MAX))
            .unwrap_or_default()
    }
}

/// Persistence boundary for an absolute Wayback retry deadline.
pub trait RetryDeadlinePersistence {
    fn load_deadline(&mut self, now: i64) -> Result<Option<i64>>;
    fn save_deadline(&mut self, deadline: i64, now: i64) -> Result<()>;
    fn clear_if_due(&mut self, now: i64) -> Result<()>;
}

pub struct RealClock(Instant);

impl Clock for RealClock {
    fn now(&self) -> Duration {
        self.0.elapsed()
    }
    fn wait(&mut self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

enum Attempt<T> {
    Ready(T),
    Retry {
        after: Option<Duration>,
        reason: String,
    },
}

enum Redirect<T> {
    Ready(T),
    Location(String),
}

pub struct WaybackRequests<C = RealClock> {
    clock: C,
    last_request: Option<Duration>,
    retry_deadline: Option<Box<dyn RetryDeadlinePersistence>>,
}

impl WaybackRequests {
    pub fn new() -> Self {
        Self {
            clock: RealClock(Instant::now()),
            last_request: None,
            retry_deadline: None,
        }
    }

    pub fn get(
        &mut self,
        client: &Client,
        url: &str,
        activity: &mut (impl FnMut(&str) -> Result<()> + ?Sized),
    ) -> Result<Response> {
        self.get_with(url, |url| client.get(url).send(), activity)
    }
}

impl<C: Clock> WaybackRequests<C> {
    #[cfg(test)]
    pub const fn with_clock(clock: C) -> Self {
        Self {
            clock,
            last_request: None,
            retry_deadline: None,
        }
    }

    pub fn with_retry_deadline_persistence(
        mut self,
        persistence: Box<dyn RetryDeadlinePersistence>,
    ) -> Self {
        self.retry_deadline = Some(persistence);
        self
    }

    pub fn get_with(
        &mut self,
        url: &str,
        mut send: impl FnMut(&str) -> reqwest::Result<Response>,
        activity: &mut (impl FnMut(&str) -> Result<()> + ?Sized),
    ) -> Result<Response> {
        let wall_now = self.clock.unix_now();
        self.follow(
            url,
            |url| match send(url) {
                Ok(response) => {
                    let status = response.status();
                    if status == reqwest::StatusCode::TOO_MANY_REQUESTS
                        || status == reqwest::StatusCode::REQUEST_TIMEOUT
                        || status.is_server_error()
                    {
                        let after = response
                            .headers()
                            .get(reqwest::header::RETRY_AFTER)
                            .and_then(|value| value.to_str().ok())
                            .and_then(|value| retry_after_unix(value, wall_now));
                        Ok(Attempt::Retry {
                            after,
                            reason: format!("HTTP {status}"),
                        })
                    } else if status.is_redirection() {
                        let location = response
                            .headers()
                            .get(reqwest::header::LOCATION)
                            .ok_or_else(|| anyhow::anyhow!("Wayback redirect missing Location"))?
                            .to_str()?
                            .to_owned();
                        Ok(Attempt::Ready(Redirect::Location(location)))
                    } else {
                        Ok(Attempt::Ready(Redirect::Ready(
                            response.error_for_status()?,
                        )))
                    }
                }
                Err(error) if error.is_timeout() || error.is_connect() => Ok(Attempt::Retry {
                    after: None,
                    reason: error.to_string(),
                }),
                Err(error) => Err(error.into()),
            },
            activity,
        )
    }
}

#[cfg(test)]
fn retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    let now = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or_default();
    retry_after_unix(value, now)
}

fn retry_after_unix(value: &str, now: i64) -> Option<Duration> {
    if let Ok(seconds) = value.trim().parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let date = httpdate::parse_http_date(value).ok()?;
    let deadline = date
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or_default();
    Some(Duration::from_secs(
        deadline.saturating_sub(now).max(0) as u64
    ))
}

impl<C: Clock> WaybackRequests<C> {
    fn follow<T>(
        &mut self,
        url: &str,
        mut send: impl FnMut(&str) -> Result<Attempt<Redirect<T>>>,
        activity: &mut (impl FnMut(&str) -> Result<()> + ?Sized),
    ) -> Result<T> {
        let mut target = url::Url::parse(url)?;
        for redirects in 0..=5 {
            if target.scheme() != "https"
                || !matches!(target.host_str(), Some("archive.org" | "web.archive.org"))
                || !target.username().is_empty()
                || target.password().is_some()
                || target.port_or_known_default() != Some(443)
            {
                bail!("unexpected Wayback request destination");
            }
            match self.run(|| send(target.as_str()), activity)? {
                Redirect::Ready(value) => return Ok(value),
                Redirect::Location(location) => {
                    if redirects == 5 {
                        bail!("Wayback redirect limit exceeded");
                    }
                    target = target.join(&location)?;
                }
            }
        }
        unreachable!("final redirect returns")
    }

    fn run<T>(
        &mut self,
        mut send: impl FnMut() -> Result<Attempt<T>>,
        activity: &mut (impl FnMut(&str) -> Result<()> + ?Sized),
    ) -> Result<T> {
        self.wait_for_persisted_deadline(activity)?;
        let mut delay = Duration::ZERO;
        let mut waited = Duration::ZERO;
        for attempt in 1..=MAX_ATTEMPTS {
            let pacing = self.last_request.map_or(Duration::ZERO, |last| {
                (last + MIN_PAUSE).saturating_sub(self.clock.now())
            });
            let mut remaining = delay.max(pacing);
            if remaining > WAIT_BUDGET.saturating_sub(waited) {
                bail!("Wayback wait budget exceeded; letter interrupted without retrying before Retry-After");
            }
            waited += remaining;
            while !remaining.is_zero() {
                activity(&format!("waiting/retry {}s", remaining.as_secs().max(1)))?;
                let step = remaining.min(Duration::from_secs(1));
                self.clock.wait(step);
                remaining = remaining.saturating_sub(step);
            }
            activity(&format!("request {attempt}/{MAX_ATTEMPTS}"))?;
            let response = send();
            self.last_request = Some(self.clock.now());
            match response? {
                Attempt::Ready(value) => return Ok(value),
                Attempt::Retry { after, reason } => {
                    if let Some(after) = after {
                        let now = self.clock.unix_now();
                        let seconds = i64::try_from(after.as_secs()).unwrap_or(i64::MAX);
                        let deadline = now.saturating_add(seconds);
                        // Save every server-provided deadline before waiting or returning. A
                        // process can stop during an otherwise in-budget wait, and the final
                        // attempt can also receive a Retry-After before it exhausts retries.
                        self.persist_retry_deadline(deadline)?;
                        if after.max(MIN_PAUSE) > WAIT_BUDGET.saturating_sub(waited) {
                            bail!("Wayback wait budget exceeded; letter interrupted without retrying before Retry-After");
                        }
                        if attempt == MAX_ATTEMPTS {
                            bail!("Wayback retries exhausted ({reason}); letter interrupted");
                        }
                        delay = after;
                    } else {
                        if attempt == MAX_ATTEMPTS {
                            bail!("Wayback retries exhausted ({reason}); letter interrupted");
                        }
                        delay = Duration::from_secs(2_u64.pow(attempt as u32));
                    }
                }
            }
        }
        unreachable!("every final attempt returns")
    }

    fn persist_retry_deadline(&mut self, deadline: i64) -> Result<()> {
        if let Some(persistence) = self.retry_deadline.as_mut() {
            persistence.save_deadline(deadline, self.clock.unix_now())?;
        }
        Ok(())
    }

    fn wait_for_persisted_deadline(
        &mut self,
        activity: &mut (impl FnMut(&str) -> Result<()> + ?Sized),
    ) -> Result<()> {
        let Some(persistence) = self.retry_deadline.as_mut() else {
            return Ok(());
        };
        loop {
            let now = self.clock.unix_now();
            let Some(deadline) = persistence.load_deadline(now)? else {
                return Ok(());
            };
            if deadline <= now {
                activity("retry deadline due")?;
                persistence.clear_if_due(self.clock.unix_now())?;
                // A same-owner update may have moved the deadline while it became due.
                continue;
            }
            let remaining = deadline.saturating_sub(now);
            activity(&format!("waiting/retry {}s", remaining.max(1)))?;
            self.clock
                .wait(Duration::from_secs(1).min(Duration::from_secs(remaining as u64)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::OptionalExtension;
    use std::cell::Cell;
    use std::rc::Rc;

    struct FakeClock(Rc<Cell<Duration>>);
    impl Clock for FakeClock {
        fn now(&self) -> Duration {
            self.0.get()
        }
        fn wait(&mut self, duration: Duration) {
            self.0.set(self.0.get() + duration);
        }
    }

    #[derive(Clone)]
    struct FakeWallClock {
        monotonic: Rc<Cell<Duration>>,
        unix: Rc<Cell<i64>>,
    }

    impl Clock for FakeWallClock {
        fn now(&self) -> Duration {
            self.monotonic.get()
        }

        fn wait(&mut self, duration: Duration) {
            self.monotonic.set(self.monotonic.get() + duration);
            self.unix.set(
                self.unix
                    .get()
                    .saturating_add(i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)),
            );
        }

        fn unix_now(&self) -> i64 {
            self.unix.get()
        }
    }

    #[test]
    fn redirects_share_pacing_and_have_a_finite_limit() {
        let now = Rc::new(Cell::new(Duration::ZERO));
        let mut requests = WaybackRequests {
            clock: FakeClock(now.clone()),
            last_request: None,
            retry_deadline: None,
        };
        let mut calls = Vec::new();
        requests
            .follow(
                "https://web.archive.org/first",
                |url| {
                    calls.push((url.to_owned(), now.get()));
                    Ok(Attempt::Ready(if calls.len() == 1 {
                        Redirect::Location("/second".into())
                    } else {
                        Redirect::Ready(())
                    }))
                },
                &mut |_| Ok(()),
            )
            .unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls[1],
            ("https://web.archive.org/second".into(), MIN_PAUSE)
        );
        let mut calls = 0;
        let result: Result<()> = requests.follow(
            "https://web.archive.org/loop",
            |_| {
                calls += 1;
                assert!(calls <= 6);
                Ok(Attempt::Ready(Redirect::Location("/loop".into())))
            },
            &mut |_| Ok(()),
        );
        assert!(result.is_err());
        assert_eq!(calls, 6);
    }

    #[test]
    fn http_dates_and_numeric_retry_after_are_supported() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let date = httpdate::fmt_http_date(now + Duration::from_secs(17));
        assert_eq!(retry_after(&date, now), Some(Duration::from_secs(17)));
        assert_eq!(retry_after("17", now), Some(Duration::from_secs(17)));
        assert_eq!(
            retry_after(&httpdate::fmt_http_date(now - Duration::from_secs(1)), now),
            Some(Duration::ZERO)
        );
        assert_eq!(retry_after("invalid", now), None);
    }

    #[test]
    fn oversized_retry_after_cannot_overflow_wait_budget() {
        let now = Rc::new(Cell::new(Duration::ZERO));
        let mut requests = WaybackRequests {
            clock: FakeClock(now),
            last_request: None,
            retry_deadline: None,
        };
        let mut calls = 0;
        let result: Result<()> = requests.run(
            || {
                calls += 1;
                Ok(Attempt::Retry {
                    after: if calls == 1 {
                        None
                    } else {
                        Some(Duration::from_secs(u64::MAX))
                    },
                    reason: "429".into(),
                })
            },
            &mut |_| Ok(()),
        );
        assert!(result.unwrap_err().to_string().contains("budget"));
        assert_eq!(calls, 2);
    }

    #[test]
    fn cumulative_wait_budget_is_not_reset_on_retry() {
        let now = Rc::new(Cell::new(Duration::ZERO));
        let mut requests = WaybackRequests {
            clock: FakeClock(now.clone()),
            last_request: None,
            retry_deadline: None,
        };
        let mut calls = 0;
        let result: Result<()> = requests.run(
            || {
                calls += 1;
                Ok(Attempt::Retry {
                    after: Some(Duration::from_secs(70)),
                    reason: "429".into(),
                })
            },
            &mut |_| Ok(()),
        );
        assert!(result.unwrap_err().to_string().contains("budget"));
        assert_eq!(calls, 2);
        assert_eq!(now.get(), Duration::from_secs(70));
    }

    #[test]
    fn callback_failure_cancels_before_request_and_during_wait() {
        for fail_at in [Duration::ZERO, Duration::from_secs(3)] {
            let now = Rc::new(Cell::new(Duration::ZERO));
            let mut requests = WaybackRequests {
                clock: FakeClock(now.clone()),
                last_request: None,
                retry_deadline: None,
            };
            let mut calls = 0;
            let result: Result<()> = requests.run(
                || {
                    calls += 1;
                    Ok(Attempt::Retry {
                        after: Some(Duration::from_secs(10)),
                        reason: "429".into(),
                    })
                },
                &mut |_| {
                    if now.get() >= fail_at {
                        bail!("lease/write failure");
                    }
                    Ok(())
                },
            );
            assert!(result.unwrap_err().to_string().contains("lease/write"));
            assert_eq!(calls, usize::from(!fail_at.is_zero()));
            assert_eq!(now.get(), fail_at);
        }
    }

    #[test]
    fn long_wait_renews_real_sqlite_lease_with_fake_clock() {
        let store = crate::store::Store::open(std::path::Path::new(":memory:")).unwrap();
        store.try_acquire_refresh_lease(1000, "worker").unwrap();
        let now = Rc::new(Cell::new(Duration::ZERO));
        let mut requests = WaybackRequests {
            clock: FakeClock(now.clone()),
            last_request: None,
            retry_deadline: None,
        };
        let mut calls = 0;
        requests
            .run(
                || {
                    calls += 1;
                    Ok(if calls == 1 {
                        Attempt::Retry {
                            after: Some(Duration::from_secs(100)),
                            reason: "429".into(),
                        }
                    } else {
                        Attempt::Ready(())
                    })
                },
                &mut |activity| {
                    let timestamp = 1000 + now.get().as_secs() as i64;
                    assert!(store.renew_refresh_lease(timestamp, "worker")?);
                    store.set_refresh_progress(&crate::store::RefreshProgress {
                        phase: activity.into(),
                        done: 1,
                        total: 2,
                        updated_at: timestamp,
                    })?;
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(calls, 2);
        assert_eq!(store.refresh_progress().unwrap().unwrap().updated_at, 1100);
        assert!(!store
            .try_acquire_refresh_lease(1100 + crate::refresh::REFRESH_LEASE_SECONDS - 1, "other")
            .unwrap());
    }

    #[test]
    fn exhausted_retries_stop_after_bounded_backoff() {
        let now = Rc::new(Cell::new(Duration::ZERO));
        let mut requests = WaybackRequests {
            clock: FakeClock(now.clone()),
            last_request: None,
            retry_deadline: None,
        };
        let mut times = Vec::new();
        let result: Result<()> = requests.run(
            || {
                times.push(now.get());
                assert!(times.len() <= 3, "no debe continuar tras agotar intentos");
                Ok(Attempt::Retry {
                    after: None,
                    reason: "503".into(),
                })
            },
            &mut |_| Ok(()),
        );
        assert!(result.is_err());
        assert_eq!(
            times,
            vec![
                Duration::ZERO,
                Duration::from_secs(2),
                Duration::from_secs(6)
            ]
        );
    }

    #[test]
    fn retry_after_above_budget_stops_without_early_retry() {
        let now = Rc::new(Cell::new(Duration::ZERO));
        let mut requests = WaybackRequests {
            clock: FakeClock(now.clone()),
            last_request: None,
            retry_deadline: None,
        };
        let mut calls = 0;
        let result: Result<()> = requests.run(
            || {
                calls += 1;
                assert_eq!(calls, 1, "no reintentar antes de Retry-After");
                Ok(Attempt::Retry {
                    after: Some(Duration::from_secs(121)),
                    reason: "429".into(),
                })
            },
            &mut |_| Ok(()),
        );
        assert!(result.unwrap_err().to_string().contains("budget"));
        assert_eq!(now.get(), Duration::ZERO);
    }

    #[test]
    fn over_budget_retry_after_is_persisted_before_returning_budget_error() {
        use reqwest::ResponseBuilderExt;
        let path = std::env::temp_dir().join(format!(
            "refranero-retry-deadline-{}.sqlite3",
            std::process::id()
        ));
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
        let start = 1_700_000_000;
        let store = crate::store::Store::open(&path).unwrap();
        assert!(store.try_acquire_refresh_lease(start, "worker").unwrap());
        let wall = FakeWallClock {
            monotonic: Rc::new(Cell::new(Duration::ZERO)),
            unix: Rc::new(Cell::new(start)),
        };
        let mut requests = WaybackRequests::with_clock(wall.clone())
            .with_retry_deadline_persistence(
                store.wayback_retry_deadline_persistence("worker").unwrap(),
            );
        let error = requests
            .get_with(
                "https://web.archive.org/test",
                |url| {
                    Ok(http::Response::builder()
                        .status(429)
                        .header("retry-after", "121")
                        .url(url::Url::parse(url).unwrap())
                        .body(String::new())
                        .unwrap()
                        .into())
                },
                &mut |_| Ok(()),
            )
            .unwrap_err();
        assert!(error.to_string().contains("budget"));
        assert_eq!(store.wayback_bootstrap_next().unwrap(), Some('A'));
        let mut other_worker = store
            .wayback_retry_deadline_persistence("other-worker")
            .unwrap();
        assert!(other_worker.clear_if_due(start + 121).is_err());
        drop(requests);
        drop(store);

        // A new Store and request controller model a worker restart.
        let reopened = crate::store::Store::open(&path).unwrap();
        let connection = rusqlite::Connection::open(&path).unwrap();
        let deadline: i64 = connection
            .query_row(
                "SELECT CAST(value AS INTEGER) FROM metadata WHERE key = 'wayback_retry_deadline'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(deadline, start + 121);
        drop(connection);
        let mut resumed = WaybackRequests::with_clock(wall.clone())
            .with_retry_deadline_persistence(
                reopened
                    .wayback_retry_deadline_persistence("worker")
                    .unwrap(),
            );
        let mut send_times = Vec::new();
        resumed
            .get_with(
                "https://web.archive.org/test",
                |url| {
                    send_times.push(wall.unix_now());
                    Ok(http::Response::builder()
                        .status(200)
                        .url(url::Url::parse(url).unwrap())
                        .body(String::new())
                        .unwrap()
                        .into())
                },
                &mut |activity| {
                    assert!(reopened.renew_refresh_lease(wall.unix_now(), "worker")?);
                    reopened.set_refresh_progress(&crate::store::RefreshProgress {
                        phase: activity.into(),
                        done: 1,
                        total: 1,
                        updated_at: wall.unix_now(),
                    })?;
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(send_times, vec![deadline]);
        assert_eq!(wall.unix_now(), deadline);
        assert_eq!(
            reopened.refresh_progress().unwrap().unwrap().updated_at,
            deadline,
            "the resumed wait publishes progress through the final deadline tick"
        );
        let connection = rusqlite::Connection::open(&path).unwrap();
        let remaining: Option<String> = connection
            .query_row(
                "SELECT value FROM metadata WHERE key = 'wayback_retry_deadline'",
                [],
                |row| row.get(0),
            )
            .optional()
            .unwrap();
        assert!(
            remaining.is_none(),
            "due deadline is cleared under the active lease"
        );
        drop(connection);
        drop(reopened);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }

    #[test]
    fn in_budget_retry_after_is_persisted_before_wait_and_enforced_after_restart() {
        use reqwest::ResponseBuilderExt;
        let path = std::env::temp_dir().join(format!(
            "refranero-in-budget-retry-deadline-{}.sqlite3",
            std::process::id()
        ));
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
        let start = 1_700_000_000;
        let store = crate::store::Store::open(&path).unwrap();
        assert!(store.try_acquire_refresh_lease(start, "worker").unwrap());
        let wall = FakeWallClock {
            monotonic: Rc::new(Cell::new(Duration::ZERO)),
            unix: Rc::new(Cell::new(start)),
        };
        let mut requests = WaybackRequests::with_clock(wall.clone())
            .with_retry_deadline_persistence(
                store.wayback_retry_deadline_persistence("worker").unwrap(),
            );
        let error = requests
            .get_with(
                "https://web.archive.org/test",
                |url| {
                    Ok(http::Response::builder()
                        .status(429)
                        .header("retry-after", "30")
                        .url(url::Url::parse(url).unwrap())
                        .body(String::new())
                        .unwrap()
                        .into())
                },
                &mut |activity| {
                    assert!(store.renew_refresh_lease(wall.unix_now(), "worker")?);
                    if activity.starts_with("waiting/retry") {
                        bail!("simulated worker interruption");
                    }
                    Ok(())
                },
            )
            .unwrap_err();
        assert!(error.to_string().contains("simulated worker interruption"));
        drop(requests);
        drop(store);

        // The next controller must honor the saved deadline before its first request.
        let reopened = crate::store::Store::open(&path).unwrap();
        let mut resumed = WaybackRequests::with_clock(wall.clone())
            .with_retry_deadline_persistence(
                reopened
                    .wayback_retry_deadline_persistence("worker")
                    .unwrap(),
            );
        let mut send_times = Vec::new();
        resumed
            .get_with(
                "https://web.archive.org/test",
                |url| {
                    send_times.push(wall.unix_now());
                    Ok(http::Response::builder()
                        .status(200)
                        .url(url::Url::parse(url).unwrap())
                        .body(String::new())
                        .unwrap()
                        .into())
                },
                &mut |activity| {
                    assert!(reopened.renew_refresh_lease(wall.unix_now(), "worker")?);
                    reopened.set_refresh_progress(&crate::store::RefreshProgress {
                        phase: activity.into(),
                        done: 1,
                        total: 1,
                        updated_at: wall.unix_now(),
                    })?;
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(send_times, vec![start + 30]);
        drop(resumed);
        drop(reopened);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }

    #[test]
    fn final_attempt_retry_after_is_persisted_before_exhausting_retries() {
        use reqwest::ResponseBuilderExt;
        let path = std::env::temp_dir().join(format!(
            "refranero-final-attempt-retry-deadline-{}.sqlite3",
            std::process::id()
        ));
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
        let store = crate::store::Store::open(&path).unwrap();
        let start = 1_700_000_000;
        assert!(store.try_acquire_refresh_lease(start, "worker").unwrap());
        let wall = FakeWallClock {
            monotonic: Rc::new(Cell::new(Duration::ZERO)),
            unix: Rc::new(Cell::new(start)),
        };
        let mut requests = WaybackRequests::with_clock(wall.clone())
            .with_retry_deadline_persistence(
                store.wayback_retry_deadline_persistence("worker").unwrap(),
            );
        let mut calls = 0;
        let error = requests
            .get_with(
                "https://web.archive.org/test",
                |url| {
                    calls += 1;
                    let response = if calls == MAX_ATTEMPTS {
                        http::Response::builder()
                            .status(429)
                            .header("retry-after", "30")
                    } else {
                        http::Response::builder().status(503)
                    };
                    Ok(response
                        .url(url::Url::parse(url).unwrap())
                        .body(String::new())
                        .unwrap()
                        .into())
                },
                &mut |activity| {
                    assert!(store.renew_refresh_lease(wall.unix_now(), "worker")?);
                    let _ = activity;
                    Ok(())
                },
            )
            .unwrap_err();
        assert!(error.to_string().contains("retries exhausted"));
        assert_eq!(calls, MAX_ATTEMPTS);
        drop(requests);

        let mut persistence = store.wayback_retry_deadline_persistence("worker").unwrap();
        assert_eq!(
            persistence.load_deadline(wall.unix_now()).unwrap(),
            Some(start + 36)
        );
        drop(persistence);
        drop(store);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }

    #[test]
    fn retry_after_blocks_requests_and_keeps_activity() {
        let now = Rc::new(Cell::new(Duration::ZERO));
        let mut requests = WaybackRequests {
            clock: FakeClock(now.clone()),
            last_request: None,
            retry_deadline: None,
        };
        let mut times = Vec::new();
        let mut ticks = 0;
        let result = requests.run(
            || {
                times.push(now.get());
                Ok(if times.len() == 1 {
                    Attempt::Retry {
                        after: Some(Duration::from_secs(7)),
                        reason: "429".into(),
                    }
                } else {
                    Attempt::Ready(())
                })
            },
            &mut |_| {
                ticks += 1;
                Ok(())
            },
        );
        assert!(result.is_ok(), "debe reintentar después del plazo");
        assert_eq!(times, vec![Duration::ZERO, Duration::from_secs(7)]);
        assert!(ticks >= 9, "heartbeat durante espera");
        requests
            .run(
                || {
                    assert!(now.get() >= Duration::from_millis(8200));
                    Ok(Attempt::Ready(()))
                },
                &mut |_| Ok(()),
            )
            .unwrap();
    }

    #[test]
    fn resumed_wait_stops_when_the_refresh_lease_is_lost() {
        use reqwest::ResponseBuilderExt;
        let path = std::env::temp_dir().join(format!(
            "refranero-retry-lease-loss-{}.sqlite3",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let start = 1_700_000_000;
        let store = crate::store::Store::open(&path).unwrap();
        assert!(store.try_acquire_refresh_lease(start, "worker").unwrap());
        let mut persistence = store.wayback_retry_deadline_persistence("worker").unwrap();
        persistence.save_deadline(start + 3, start).unwrap();
        let wall = FakeWallClock {
            monotonic: Rc::new(Cell::new(Duration::ZERO)),
            unix: Rc::new(Cell::new(start)),
        };
        let mut requests =
            WaybackRequests::with_clock(wall.clone()).with_retry_deadline_persistence(persistence);
        let mut sends = 0;
        let mut callbacks = 0;
        let result = requests.get_with(
            "https://web.archive.org/test",
            |url| {
                sends += 1;
                Ok(http::Response::builder()
                    .status(200)
                    .url(url::Url::parse(url).unwrap())
                    .body(String::new())
                    .unwrap()
                    .into())
            },
            &mut |_| {
                callbacks += 1;
                if callbacks == 1 {
                    store.release_refresh_lease("worker")?;
                    return Ok(());
                }
                if !store.renew_refresh_lease(wall.unix_now(), "worker")? {
                    bail!("refresh lease lost during resumed deadline wait");
                }
                Ok(())
            },
        );
        assert!(result.unwrap_err().to_string().contains("lease lost"));
        assert_eq!(sends, 0);
        assert_eq!(wall.unix_now(), start + 1);
        drop(requests);
        drop(store);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }

    struct FailingDeadlinePersistence;

    impl RetryDeadlinePersistence for FailingDeadlinePersistence {
        fn load_deadline(&mut self, _: i64) -> Result<Option<i64>> {
            Ok(None)
        }
        fn save_deadline(&mut self, _: i64, _: i64) -> Result<()> {
            bail!("deadline persistence failed")
        }
        fn clear_if_due(&mut self, _: i64) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn retry_deadline_persistence_failure_is_not_hidden_by_budget_error() {
        use reqwest::ResponseBuilderExt;
        let now = Rc::new(Cell::new(Duration::ZERO));
        let wall = FakeWallClock {
            monotonic: now,
            unix: Rc::new(Cell::new(1_700_000_000)),
        };
        let mut requests = WaybackRequests::with_clock(wall)
            .with_retry_deadline_persistence(Box::new(FailingDeadlinePersistence));
        let mut calls = 0;
        let error = requests
            .get_with(
                "https://web.archive.org/test",
                |url| {
                    calls += 1;
                    Ok(http::Response::builder()
                        .status(429)
                        .header("retry-after", "121")
                        .url(url::Url::parse(url).unwrap())
                        .body(String::new())
                        .unwrap()
                        .into())
                },
                &mut |_| Ok(()),
            )
            .unwrap_err();
        assert!(error.to_string().contains("deadline persistence failed"));
        assert_eq!(calls, 1, "persistence failure must stop before a retry");
    }
}

//! Control secuencial de solicitudes Wayback, con esperas cancelables.
use std::time::{Duration, Instant, SystemTime};

use anyhow::{bail, Result};
use reqwest::blocking::{Client, Response};

pub const SHARED_MIN_PAUSE: Duration = Duration::from_millis(1200);
const MIN_PAUSE: Duration = SHARED_MIN_PAUSE;
const WAIT_BUDGET: Duration = Duration::from_secs(120);
const MAX_ATTEMPTS: usize = 3;

#[derive(Debug, PartialEq, Eq)]
pub enum RequestGate {
    Granted,
    WaitUntil(i64),
}

/// Shared request pacing and Retry-After boundary, independent of refresh ownership.
pub trait SharedRequestCoordination {
    fn gate(&mut self, now_millis: i64) -> Result<RequestGate>;
    fn record_cooldown(&mut self, deadline_millis: i64, now_millis: i64) -> Result<()>;
}

pub trait Clock {
    fn now(&self) -> Duration;
    fn wait(&mut self, duration: Duration);

    fn unix_now(&self) -> i64 {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|duration| i64::try_from(duration.as_secs()).unwrap_or(i64::MAX))
            .unwrap_or_default()
    }

    fn unix_now_millis(&self) -> i64 {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|duration| i64::try_from(duration.as_millis()).unwrap_or(i64::MAX))
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
    coordination: Option<Box<dyn SharedRequestCoordination>>,
}

impl WaybackRequests {
    pub fn new() -> Self {
        Self {
            clock: RealClock(Instant::now()),
            last_request: None,
            retry_deadline: None,
            coordination: None,
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
            coordination: None,
        }
    }

    pub fn with_retry_deadline_persistence(
        mut self,
        persistence: Box<dyn RetryDeadlinePersistence>,
    ) -> Self {
        self.retry_deadline = Some(persistence);
        self
    }

    pub fn with_shared_coordination(
        mut self,
        coordination: Box<dyn SharedRequestCoordination>,
    ) -> Self {
        self.coordination = Some(coordination);
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
            |url| {
                let endpoint = wayback_endpoint_class(url);
                match send(url) {
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
                                reason: format!("HTTP {status} for {endpoint}"),
                            })
                        } else if status.is_redirection() {
                            let location = response
                                .headers()
                                .get(reqwest::header::LOCATION)
                                .ok_or_else(|| {
                                    anyhow::anyhow!("Wayback redirect missing Location")
                                })?
                                .to_str()?
                                .to_owned();
                            Ok(Attempt::Ready(Redirect::Location(location)))
                        } else {
                            if status.is_client_error() {
                                let error = response
                                    .error_for_status()
                                    .expect_err("client error status must produce a reqwest error")
                                    .without_url();
                                return Err(anyhow::Error::new(error)
                                    .context(format!("Wayback {endpoint} HTTP {status}")));
                            }
                            Ok(Attempt::Ready(Redirect::Ready(response)))
                        }
                    }
                    Err(error) if error.is_timeout() || error.is_connect() => Ok(Attempt::Retry {
                        after: None,
                        reason: if error.is_timeout() {
                            format!("{endpoint} connection timeout")
                        } else {
                            format!("{endpoint} connection failure")
                        },
                    }),
                    Err(_) => bail!("Wayback {endpoint} transport failure"),
                }
            },
            activity,
        )
    }
}

fn wayback_endpoint_class(raw_url: &str) -> &'static str {
    let Ok(url) = url::Url::parse(raw_url) else {
        return "Wayback endpoint";
    };
    if url.path().starts_with("/cdx/search/cdx") {
        "CDX index"
    } else if url.path().starts_with("/web/") {
        "archived capture"
    } else if url.path().contains("available") {
        "capture availability lookup"
    } else {
        "Wayback endpoint"
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
            let pacing = if self.coordination.is_some() {
                Duration::ZERO
            } else {
                self.last_request.map_or(Duration::ZERO, |last| {
                    (last + MIN_PAUSE).saturating_sub(self.clock.now())
                })
            };
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
            self.wait_for_shared_gate(&mut waited, activity)?;
            let response = send();
            self.last_request = Some(self.clock.now());
            match response? {
                Attempt::Ready(value) => return Ok(value),
                Attempt::Retry { after, reason } => {
                    if let Some(after) = after {
                        let now = self.clock.unix_now();
                        let seconds = i64::try_from(after.as_secs()).unwrap_or(i64::MAX);
                        let deadline = now.saturating_add(seconds);
                        if let Some(coordination) = self.coordination.as_mut() {
                            let millis = i64::try_from(after.as_millis()).unwrap_or(i64::MAX);
                            let now_millis = self.clock.unix_now_millis();
                            coordination
                                .record_cooldown(now_millis.saturating_add(millis), now_millis)?;
                        }
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

    fn wait_for_shared_gate(
        &mut self,
        waited: &mut Duration,
        activity: &mut (impl FnMut(&str) -> Result<()> + ?Sized),
    ) -> Result<()> {
        let Some(coordination) = self.coordination.as_mut() else {
            return Ok(());
        };
        loop {
            let now = self.clock.unix_now_millis();
            match coordination.gate(now)? {
                RequestGate::Granted => return Ok(()),
                RequestGate::WaitUntil(deadline) => {
                    let millis = deadline.saturating_sub(now).max(0) as u64;
                    let remaining = Duration::from_millis(millis);
                    let step = remaining.min(Duration::from_secs(1));
                    if step > WAIT_BUDGET.saturating_sub(*waited) {
                        bail!("Wayback wait budget exceeded; letter interrupted before shared request gate");
                    }
                    activity(&format!("waiting/shared {}s", remaining.as_secs().max(1)))?;
                    self.clock.wait(step);
                    *waited += step;
                }
            }
        }
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

        fn unix_now_millis(&self) -> i64 {
            self.unix
                .get()
                .saturating_mul(1000)
                .saturating_add(self.monotonic.get().subsec_millis() as i64)
        }
    }

    #[test]
    fn redirects_share_pacing_and_have_a_finite_limit() {
        let now = Rc::new(Cell::new(Duration::ZERO));
        let mut requests = WaybackRequests {
            clock: FakeClock(now.clone()),
            last_request: None,
            retry_deadline: None,
            coordination: None,
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
            coordination: None,
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
            coordination: None,
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
                coordination: None,
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
            coordination: None,
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
            coordination: None,
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
            coordination: None,
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
            coordination: None,
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

    #[test]
    fn shared_gate_rechecks_cooldown_after_another_connection_records_429() {
        let path = std::env::temp_dir().join(format!(
            "refranero-wayback-gate-{}.sqlite3",
            std::process::id()
        ));
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
        let first_store = crate::store::Store::open(&path).unwrap();
        let second_store = crate::store::Store::open(&path).unwrap();
        let mut first = first_store.wayback_request_coordination().unwrap().unwrap();
        let mut second = second_store
            .wayback_request_coordination()
            .unwrap()
            .unwrap();

        assert_eq!(first.gate(1_000_000).unwrap(), RequestGate::Granted);
        assert_eq!(
            second.gate(1_000_000).unwrap(),
            RequestGate::WaitUntil(1_001_200)
        );
        first.record_cooldown(1_010_000, 1_000_000).unwrap();
        assert_eq!(
            second.gate(1_001_200).unwrap(),
            RequestGate::WaitUntil(1_010_000),
            "the gate must re-read the latest cooldown instead of honoring a stale turn"
        );
        second.record_cooldown(1_005_000, 1_001_200).unwrap();
        assert_eq!(
            first.gate(1_001_200).unwrap(),
            RequestGate::WaitUntil(1_010_000),
            "a later, shorter Retry-After must not shorten the global cooldown"
        );

        drop(first);
        drop(second);
        drop(first_store);
        drop(second_store);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }

    #[test]
    fn concurrent_gate_calls_on_separate_connections_grant_once() {
        use std::sync::{Arc, Barrier};

        let path = std::env::temp_dir().join(format!(
            "refranero-wayback-concurrent-gate-{}.sqlite3",
            std::process::id()
        ));
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
        drop(crate::store::Store::open(&path).unwrap());

        let barrier = Arc::new(Barrier::new(2));
        let first_path = path.clone();
        let first_barrier = Arc::clone(&barrier);
        let first = std::thread::spawn(move || {
            let store = crate::store::Store::open(&first_path).unwrap();
            let mut coordination = store.wayback_request_coordination().unwrap().unwrap();
            first_barrier.wait();
            coordination.gate(1_000_000).unwrap()
        });
        let second_path = path.clone();
        let second = std::thread::spawn(move || {
            let store = crate::store::Store::open(&second_path).unwrap();
            let mut coordination = store.wayback_request_coordination().unwrap().unwrap();
            barrier.wait();
            coordination.gate(1_000_000).unwrap()
        });

        let results = [first.join().unwrap(), second.join().unwrap()];
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, RequestGate::Granted))
                .count(),
            1,
            "concurrent callers must serialize their immediate request slots"
        );
        assert_eq!(
            results
                .iter()
                .filter_map(|result| match result {
                    RequestGate::WaitUntil(deadline) => Some(*deadline),
                    RequestGate::Granted => None,
                })
                .collect::<Vec<_>>(),
            vec![1_001_200]
        );

        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }

    #[test]
    fn simulated_429_cooldown_blocks_second_connection_until_deadline() {
        use reqwest::ResponseBuilderExt;

        let path = std::env::temp_dir().join(format!(
            "refranero-wayback-cycle-{}.sqlite3",
            std::process::id()
        ));
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
        let first_store = crate::store::Store::open(&path).unwrap();
        let second_store = crate::store::Store::open(&path).unwrap();
        let start = 1_700_000_000;
        let wall = FakeWallClock {
            monotonic: Rc::new(Cell::new(Duration::ZERO)),
            unix: Rc::new(Cell::new(start)),
        };
        let mut first = WaybackRequests::with_clock(wall.clone())
            .with_shared_coordination(first_store.wayback_request_coordination().unwrap().unwrap());
        let mut second = WaybackRequests::with_clock(wall.clone()).with_shared_coordination(
            second_store
                .wayback_request_coordination()
                .unwrap()
                .unwrap(),
        );
        let mut second_sent_at = Vec::new();
        let mut second_result = None;
        let mut second_called = false;
        let mut first_sends = 0;

        let result = first.get_with(
            "https://web.archive.org/cdx/search/cdx?url=example",
            |url| {
                first_sends += 1;
                let status = if first_sends == 1 { 429 } else { 200 };
                let mut builder = http::Response::builder()
                    .status(status)
                    .url(url::Url::parse(url).unwrap());
                if status == 429 {
                    builder = builder.header("retry-after", "5");
                }
                Ok(builder.body(String::new()).unwrap().into())
            },
            &mut |activity| {
                if activity.starts_with("waiting/retry") && !second_called {
                    second_called = true;
                    second_result = Some(second.get_with(
                        "https://web.archive.org/cdx/search/cdx?url=other",
                        |url| {
                            second_sent_at.push(wall.unix_now_millis());
                            Ok(http::Response::builder()
                                .status(200)
                                .url(url::Url::parse(url).unwrap())
                                .body(String::new())
                                .unwrap()
                                .into())
                        },
                        &mut |_| Ok(()),
                    ));
                }
                Ok(())
            },
        );

        assert!(result.is_ok());
        assert!(second_result.unwrap().is_ok());
        assert_eq!(second_sent_at, vec![(start + 5) * 1000]);
        assert_eq!(first_sends, 2);

        drop(first);
        drop(second);
        drop(first_store);
        drop(second_store);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }

    #[test]
    fn http_499_reports_safe_status_and_endpoint_class_without_retrying() {
        use reqwest::ResponseBuilderExt;

        let mut requests =
            WaybackRequests::with_clock(FakeClock(Rc::new(Cell::new(Duration::ZERO))));
        let mut calls = 0;
        let error = requests
            .get_with(
                "https://web.archive.org/cdx/search/cdx?url=private.example&token=secret",
                |url| {
                    calls += 1;
                    Ok(http::Response::builder()
                        .status(499)
                        .url(url::Url::parse(url).unwrap())
                        .body("private response body")
                        .unwrap()
                        .into())
                },
                &mut |_| Ok(()),
            )
            .unwrap_err()
            .to_string();

        assert_eq!(calls, 1, "HTTP 499 must not add a retry");
        assert!(error.contains("HTTP 499"));
        assert!(error.contains("CDX index"));
        assert!(!error.contains("private.example"));
        assert!(!error.contains("secret"));
        assert!(!error.contains("private response body"));
    }

    #[test]
    fn in_memory_catalog_keeps_local_pacing_without_a_shared_database_adapter() {
        let store = crate::store::Store::open(std::path::Path::new(":memory:")).unwrap();
        assert!(store.wayback_request_coordination().unwrap().is_none());
    }
}

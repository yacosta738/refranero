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
}

impl WaybackRequests {
    pub fn new() -> Self {
        Self {
            clock: RealClock(Instant::now()),
            last_request: None,
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
        }
    }

    pub fn get_with(
        &mut self,
        url: &str,
        mut send: impl FnMut(&str) -> reqwest::Result<Response>,
        activity: &mut (impl FnMut(&str) -> Result<()> + ?Sized),
    ) -> Result<Response> {
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
                            .and_then(|value| retry_after(value, SystemTime::now()));
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

fn retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    value
        .trim()
        .parse::<u64>()
        .map(Duration::from_secs)
        .ok()
        .or_else(|| {
            httpdate::parse_http_date(value)
                .ok()
                .map(|date| date.duration_since(now).unwrap_or_default())
        })
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
                    if attempt == MAX_ATTEMPTS {
                        bail!("Wayback retries exhausted ({reason}); letter interrupted");
                    }
                    delay = after.unwrap_or_else(|| Duration::from_secs(2_u64.pow(attempt as u32)));
                }
            }
        }
        unreachable!("every final attempt returns")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    #[test]
    fn redirects_share_pacing_and_have_a_finite_limit() {
        let now = Rc::new(Cell::new(Duration::ZERO));
        let mut requests = WaybackRequests {
            clock: FakeClock(now.clone()),
            last_request: None,
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
    fn retry_after_blocks_requests_and_keeps_activity() {
        let now = Rc::new(Cell::new(Duration::ZERO));
        let mut requests = WaybackRequests {
            clock: FakeClock(now.clone()),
            last_request: None,
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
}

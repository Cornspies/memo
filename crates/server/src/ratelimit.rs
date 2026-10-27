//! In-memory rate limits (`docs/api.md#rate-limits`) and the client IP behind trusted proxies.

use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr},
    sync::{Mutex, PoisonError},
    time::{Duration, Instant},
};

use axum::http::HeaderMap;
use uuid::Uuid;

use crate::{
    config::Cidr,
    error::{ApiError, ApiResult},
};

pub const MINUTE: Duration = Duration::from_secs(60);
pub const HOUR: Duration = Duration::from_secs(3600);
const FREE_LOGIN_FAILURES: u32 = 5;
const MAX_LOGIN_DELAY: u64 = 15 * 60;

#[derive(Default)]
pub struct RateLimiter {
    windows: Mutex<HashMap<(&'static str, String), Window>>,
    login_failures: Mutex<HashMap<Uuid, Failures>>,
}

struct Window {
    start: Instant,
    period: Duration,
    count: u32,
}

struct Failures {
    count: u32,
    last: Instant,
}

impl RateLimiter {
    /// Fixed window: at most `limit` calls per `period` for `scope` + `key`.
    pub fn check(&self, scope: &'static str, key: impl Into<String>, limit: u32, period: Duration) -> ApiResult<()> {
        let now = Instant::now();
        let mut windows = self.windows.lock().unwrap_or_else(PoisonError::into_inner);
        let window = windows.entry((scope, key.into())).or_insert(Window { start: now, period, count: 0 });
        if now.duration_since(window.start) >= period {
            *window = Window { start: now, period, count: 0 };
        }
        window.count += 1;
        if window.count > limit {
            let wait = period.saturating_sub(now.duration_since(window.start));
            return Err(ApiError::rate_limited(wait.as_secs()));
        }
        Ok(())
    }

    /// Exponential backoff after repeated failures, but never a hard lockout: that would let
    /// anyone lock someone else out.
    pub fn check_login(&self, user_id: Uuid) -> ApiResult<()> {
        let failures = self.login_failures.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(f) = failures.get(&user_id).filter(|f| f.count >= FREE_LOGIN_FAILURES) {
            let delay = Duration::from_secs((1u64 << (f.count - FREE_LOGIN_FAILURES).min(10)).min(MAX_LOGIN_DELAY));
            let elapsed = f.last.elapsed();
            if elapsed < delay {
                return Err(ApiError::rate_limited((delay - elapsed).as_secs()));
            }
        }
        Ok(())
    }

    pub fn login_failed(&self, user_id: Uuid) {
        let mut failures = self.login_failures.lock().unwrap_or_else(PoisonError::into_inner);
        let entry = failures.entry(user_id).or_insert(Failures { count: 0, last: Instant::now() });
        entry.count += 1;
        entry.last = Instant::now();
    }

    pub fn login_succeeded(&self, user_id: Uuid) {
        self.login_failures.lock().unwrap_or_else(PoisonError::into_inner).remove(&user_id);
    }

    /// Drops expired windows and old failure records.
    pub fn prune(&self) {
        let now = Instant::now();
        self.windows
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|_, w| now.duration_since(w.start) < w.period);
        self.login_failures
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|_, f| now.duration_since(f.last) < Duration::from_secs(MAX_LOGIN_DELAY * 2));
    }
}

/// The peer address, or the nearest untrusted address in `X-Forwarded-For` when the peer is a
/// trusted proxy.
pub fn client_ip(headers: &HeaderMap, peer: Option<IpAddr>, trusted: &[Cidr]) -> IpAddr {
    let peer = peer.unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));
    let is_trusted = |ip: IpAddr| trusted.iter().any(|net| net.contains(ip));
    if !is_trusted(peer) {
        return peer;
    }
    let forwarded: Vec<IpAddr> = headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .filter_map(|ip| ip.trim().parse().ok())
        .collect();
    forwarded.iter().rev().copied().find(|ip| !is_trusted(*ip)).or(forwarded.first().copied()).unwrap_or(peer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_limit_and_reset() {
        let limiter = RateLimiter::default();
        for _ in 0..3 {
            limiter.check("t", "k", 3, MINUTE).unwrap();
        }
        assert!(limiter.check("t", "k", 3, MINUTE).unwrap_err().retry_after.unwrap() >= 1);
        limiter.check("t", "other", 3, MINUTE).unwrap();
        limiter.check("z", "k", 1, Duration::ZERO).unwrap();
        limiter.check("z", "k", 1, Duration::ZERO).unwrap();
    }

    #[test]
    fn login_backoff_starts_after_five_failures() {
        let limiter = RateLimiter::default();
        let user = Uuid::now_v7();
        for _ in 0..5 {
            limiter.check_login(user).unwrap();
            limiter.login_failed(user);
        }
        assert!(limiter.check_login(user).is_err());
        limiter.login_succeeded(user);
        limiter.check_login(user).unwrap();
    }

    #[test]
    fn forwarded_for_only_from_trusted_proxies() {
        let trusted = [Cidr::parse("127.0.0.1/32").unwrap(), Cidr::parse("10.0.0.0/8").unwrap()];
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "203.0.113.9, 198.51.100.7, 10.0.0.2".parse().unwrap());
        let (local, outside): (IpAddr, IpAddr) = ("127.0.0.1".parse().unwrap(), "192.0.2.1".parse().unwrap());
        assert_eq!(client_ip(&headers, Some(local), &trusted), "198.51.100.7".parse::<IpAddr>().unwrap());
        assert_eq!(client_ip(&headers, Some(outside), &trusted), outside);
        assert_eq!(client_ip(&HeaderMap::new(), Some(local), &trusted), local);
    }
}

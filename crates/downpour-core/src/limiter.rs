use std::num::NonZeroU32;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, Notify};
use tokio::time::Instant;

/// Global bandwidth cap shared by every connection of every download.
///
/// A token bucket that may go into debt: a chunk larger than the remaining
/// budget is admitted immediately and the caller sleeps off the deficit, so
/// large network reads never stall forever waiting for a full bucket.
/// One large read at a low limit can mean a long sleep, so a change of the
/// limit wakes the sleepers: the new limit applies from their next bytes.
#[derive(Debug)]
pub struct RateLimiter {
    bytes_per_second: AtomicU64,
    bucket: Mutex<Bucket>,
    changed: Notify,
}

#[derive(Debug)]
struct Bucket {
    tokens: f64,
    refilled_at: Instant,
}

/// Bandwidth setting where `Unlimited` is explicit rather than a magic zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpeedLimit {
    Unlimited,
    BytesPerSecond(u64),
}

/// Limits are set in KiB per second; the limiter counts bytes.
pub fn kib_to_bytes_per_second(kib_per_second: NonZeroU32) -> u64 {
    u64::from(kib_per_second.get()) * 1024
}

impl SpeedLimit {
    pub fn kib_per_second(kib_per_second: NonZeroU32) -> Self {
        SpeedLimit::BytesPerSecond(kib_to_bytes_per_second(kib_per_second))
    }

    fn encode(self) -> u64 {
        match self {
            SpeedLimit::Unlimited => 0,
            SpeedLimit::BytesPerSecond(rate) => rate.max(1),
        }
    }
}

impl RateLimiter {
    pub fn new(limit: SpeedLimit) -> Self {
        Self {
            bytes_per_second: AtomicU64::new(limit.encode()),
            bucket: Mutex::new(Bucket {
                tokens: 0.0,
                refilled_at: Instant::now(),
            }),
            changed: Notify::new(),
        }
    }

    pub fn set_limit(&self, limit: SpeedLimit) {
        self.bytes_per_second
            .store(limit.encode(), Ordering::Relaxed);
        self.changed.notify_waiters();
    }

    pub async fn acquire(&self, bytes: u64) {
        // Registered before the rate is read, so a change in between still wakes us.
        let changed = self.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        let rate = self.bytes_per_second.load(Ordering::Relaxed);
        if rate == 0 || bytes == 0 {
            return;
        }
        let deficit = {
            let mut bucket = self.bucket.lock().await;
            let now = Instant::now();
            let refill = now.duration_since(bucket.refilled_at).as_secs_f64() * rate as f64;
            // At most one second of burst so an idle period cannot bank unlimited credit.
            bucket.tokens = (bucket.tokens + refill).min(rate as f64);
            bucket.refilled_at = now;
            bucket.tokens -= bytes as f64;
            -bucket.tokens
        };
        if deficit > 0.0 {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs_f64(deficit / rate as f64)) => {}
                _ = changed => {}
            }
        }
    }
}

/// How fast one download may go.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DownloadLimit {
    /// Shares the speed limit from Settings (if any) with the other downloads.
    #[default]
    FollowSettings,
    /// As fast as the server allows; it does not use up the shared limit.
    Unlimited,
    /// Its own limit, instead of the shared one.
    Limited { kib_per_second: NonZeroU32 },
}

impl DownloadLimit {
    fn own_rate(self) -> SpeedLimit {
        match self {
            DownloadLimit::Limited { kib_per_second } => SpeedLimit::kib_per_second(kib_per_second),
            DownloadLimit::FollowSettings | DownloadLimit::Unlimited => SpeedLimit::Unlimited,
        }
    }
}

/// One download's limiter: the shared one, its own, or none, as its
/// `DownloadLimit` says. The limit can change while the download runs.
#[derive(Debug)]
pub struct Throttle {
    shared: Arc<RateLimiter>,
    own: RateLimiter,
    limit: std::sync::Mutex<DownloadLimit>,
    /// Wakes a wait on the shared limiter when this download leaves it.
    changed: Notify,
}

impl Throttle {
    pub fn new(shared: Arc<RateLimiter>, limit: DownloadLimit) -> Self {
        Self {
            shared,
            own: RateLimiter::new(limit.own_rate()),
            limit: std::sync::Mutex::new(limit),
            changed: Notify::new(),
        }
    }

    pub fn set_limit(&self, limit: DownloadLimit) {
        *self.limit.lock().unwrap_or_else(|p| p.into_inner()) = limit;
        // Wakes a wait on its own limiter too.
        self.own.set_limit(limit.own_rate());
        self.changed.notify_waiters();
    }

    pub async fn acquire(&self, bytes: u64) {
        let changed = self.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        let limit = *self.limit.lock().unwrap_or_else(|p| p.into_inner());
        let wait = async {
            match limit {
                DownloadLimit::FollowSettings => self.shared.acquire(bytes).await,
                DownloadLimit::Unlimited => {}
                DownloadLimit::Limited { .. } => self.own.acquire(bytes).await,
            }
        };
        tokio::select! {
            () = wait => {}
            _ = changed => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SLOW_SHARED: SpeedLimit = SpeedLimit::BytesPerSecond(1000);

    fn kib(value: u32) -> DownloadLimit {
        DownloadLimit::Limited {
            kib_per_second: NonZeroU32::new(value).unwrap(),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_download_following_settings_shares_the_limit() {
        let throttle = Throttle::new(
            Arc::new(RateLimiter::new(SLOW_SHARED)),
            DownloadLimit::FollowSettings,
        );
        let start = Instant::now();
        for _ in 0..4 {
            throttle.acquire(500).await;
        }
        assert!(start.elapsed() >= Duration::from_millis(1900));
    }

    #[tokio::test(start_paused = true)]
    async fn an_unlimited_download_ignores_the_shared_limit() {
        let throttle = Throttle::new(
            Arc::new(RateLimiter::new(SLOW_SHARED)),
            DownloadLimit::Unlimited,
        );
        let start = Instant::now();
        throttle.acquire(1_000_000).await;
        assert!(start.elapsed() < Duration::from_millis(1));
    }

    #[tokio::test(start_paused = true)]
    async fn a_download_with_its_own_limit_uses_that_instead() {
        let throttle = Throttle::new(Arc::new(RateLimiter::new(SLOW_SHARED)), kib(4));
        let start = Instant::now();
        // 8 KiB at 4 KiB/s takes 2 s; the shared 1000 B/s would take 8 s.
        for _ in 0..8 {
            throttle.acquire(1024).await;
        }
        let elapsed = start.elapsed();
        assert!(elapsed >= Duration::from_millis(1900), "{elapsed:?}");
        assert!(elapsed < Duration::from_millis(2500), "{elapsed:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn the_limit_can_be_lifted_while_downloading() {
        let throttle = Throttle::new(Arc::new(RateLimiter::new(SLOW_SHARED)), kib(1));
        throttle.acquire(1024).await;
        throttle.set_limit(DownloadLimit::Unlimited);
        let start = Instant::now();
        throttle.acquire(1_000_000).await;
        assert!(start.elapsed() < Duration::from_millis(1));
    }

    #[tokio::test(start_paused = true)]
    async fn throttles_to_configured_rate() {
        let limiter = RateLimiter::new(SpeedLimit::BytesPerSecond(1000));
        let start = tokio::time::Instant::now();
        for _ in 0..4 {
            limiter.acquire(500).await;
        }
        assert!(start.elapsed() >= Duration::from_millis(1900));
    }

    #[tokio::test(start_paused = true)]
    async fn raising_the_limit_wakes_a_long_wait() {
        let limiter = Arc::new(RateLimiter::new(SpeedLimit::BytesPerSecond(1000)));
        let start = Instant::now();
        // A 1 MB read at 1000 B/s would sleep for over 16 minutes.
        let waiting = tokio::spawn({
            let limiter = Arc::clone(&limiter);
            async move { limiter.acquire(1_000_000).await }
        });
        tokio::time::sleep(Duration::from_secs(1)).await;
        limiter.set_limit(SpeedLimit::Unlimited);
        waiting.await.unwrap();
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test(start_paused = true)]
    async fn leaving_the_shared_limit_wakes_a_long_wait() {
        let throttle = Arc::new(Throttle::new(
            Arc::new(RateLimiter::new(SLOW_SHARED)),
            DownloadLimit::FollowSettings,
        ));
        let start = Instant::now();
        let waiting = tokio::spawn({
            let throttle = Arc::clone(&throttle);
            async move { throttle.acquire(1_000_000).await }
        });
        tokio::time::sleep(Duration::from_secs(1)).await;
        throttle.set_limit(DownloadLimit::Unlimited);
        waiting.await.unwrap();
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test]
    async fn unlimited_never_waits() {
        let limiter = RateLimiter::new(SpeedLimit::Unlimited);
        let start = Instant::now();
        limiter.acquire(u64::MAX).await;
        assert!(start.elapsed() < Duration::from_millis(50));
    }
}

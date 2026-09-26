//! [`TenantCache`]: the one protocol every TTL'd whole-fleet/lookup slot uses.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use chrono::{DateTime, Utc};

use super::TenantKey;

/// One tenant-stamped, TTL'd, epoch-gated, single-flight cache slot.
///
/// The same five-step protocol was written out four times in `state.rs` — once for
/// lookups, once for the device inventory, and twice for the current-patch families
/// via `current_family`. Each copy had to get the same sequence right:
///
/// 1. probe the slot (tenant match **and** within the TTL) and return a hit;
/// 2. take the single-flight gate, so the loser of a race waits instead of paging a
///    six-figure feed a second time;
/// 3. re-probe under the gate — this is what turns that wait into a cache hit;
/// 4. sample the epoch *before* the fetch;
/// 5. store only if the epoch is unchanged when the slot lock is retaken.
///
/// Step 5 is the subtle one and it is not optional: without it, a fetch already in
/// flight when an invalidation lands writes its pre-invalidation rows straight back
/// and restarts the TTL on exactly the data the caller wanted gone. The tenant stamp
/// cannot cover that case, because a sign-out or a post-action invalidation is the
/// *same* tenant.
///
/// Prose did not keep the four copies in step. `lookups_epoch`'s field comment used
/// to read "Mirrors `devices_epoch`/`current_epoch`, which the lookups cache was
/// missing" — step 5 was simply absent there until someone noticed, and the lookups
/// slot also never grew step 2 at all. Both are now structural: a fifth cache cannot
/// be added without the protocol, because the protocol is the type.
pub(crate) struct TenantCache<T> {
    slot: Mutex<Option<CacheEntry<T>>>,
    /// Bumped by [`TenantCache::invalidate`] before it clears the slot, and re-read
    /// under the slot lock at the store. Both interleavings of a racing write lose:
    /// one that stores before the clear is wiped by it, one that stores after sees
    /// the new epoch and declines.
    epoch: AtomicU64,
    /// Held across `.await`, so a `tokio` mutex rather than a `std` one.
    fetch_lock: tokio::sync::Mutex<()>,
}

struct CacheEntry<T> {
    /// Monotonic instant the entry was stored, for the TTL comparison. Separate from
    /// `fetched_at` because a wall clock can move backwards.
    at: Instant,
    tenant: TenantKey,
    /// Behind an `Arc` so a hit is a refcount bump rather than a deep copy of a
    /// whole-fleet `Vec`.
    value: Arc<T>,
    /// Wall-clock fetch time, for the UI's "patch data as of …" label.
    fetched_at: DateTime<Utc>,
}

impl<T> Default for TenantCache<T> {
    fn default() -> Self {
        Self {
            slot: Mutex::new(None),
            epoch: AtomicU64::new(0),
            fetch_lock: tokio::sync::Mutex::new(()),
        }
    }
}

impl<T> TenantCache<T> {
    /// A live entry for `tenant`, or `None` on a miss, a tenant change, or past
    /// `ttl`. One definition, used by both the pre-lock probe and the post-gate
    /// re-check so the two cannot disagree about what "fresh" means.
    pub(super) fn peek(
        &self,
        tenant: &TenantKey,
        ttl: Duration,
    ) -> Option<(Arc<T>, DateTime<Utc>)> {
        let guard = self.slot.lock().ok()?;
        let entry = guard.as_ref()?;
        (entry.tenant == *tenant && entry.at.elapsed() < ttl)
            .then(|| (entry.value.clone(), entry.fetched_at))
    }

    /// Serves `tenant`'s entry, fetching it if there isn't a fresh one.
    ///
    /// `ttl` is a parameter rather than a field because the current-patch families
    /// use two: a normal re-filter accepts `CURRENT_PATCHES_TTL`, while a forced
    /// refresh drops to `FORCE_MIN_INTERVAL` so a fast auto-refresh cadence cannot
    /// become a refetch loop.
    ///
    /// The slot lock is never held across the fetch.
    pub(super) async fn get_or_fetch<F, Fut>(
        &self,
        tenant: &TenantKey,
        ttl: Duration,
        label: &'static str,
        fetch: F,
    ) -> Result<(Arc<T>, DateTime<Utc>)>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        if let Some(hit) = self.peek(tenant, ttl) {
            return Ok(hit);
        }
        let _fetching = self.fetch_lock.lock().await;
        // Re-probe under the gate: the winner of the race has usually stored by now,
        // which is what makes waiting cheaper than fetching.
        if let Some(hit) = self.peek(tenant, ttl) {
            return Ok(hit);
        }
        let epoch = self.epoch.load(Ordering::SeqCst);
        let value = Arc::new(fetch().await?);
        let fetched_at = Utc::now();
        if let Ok(mut guard) = self.slot.lock() {
            if self.epoch.load(Ordering::SeqCst) == epoch {
                *guard = Some(CacheEntry {
                    at: Instant::now(),
                    tenant: tenant.clone(),
                    value: value.clone(),
                    fetched_at,
                });
            } else {
                tracing::debug!("{label} invalidated mid-fetch; not caching");
            }
        }
        Ok((value, fetched_at))
    }

    /// The current epoch, for tests asserting an invalidation happened.
    #[cfg(test)]
    pub(crate) fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::SeqCst)
    }

    /// Drops the entry and makes any fetch already in flight decline to store.
    pub(crate) fn invalidate(&self) {
        // Bump *before* clearing, so both interleavings of a racing write lose.
        self.epoch.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut guard) = self.slot.lock() {
            *guard = None;
        }
    }
}

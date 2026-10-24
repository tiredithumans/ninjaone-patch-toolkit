//! [`TenantCache`]: the one protocol every TTL'd whole-fleet/lookup slot uses.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use chrono::{DateTime, Utc};

use super::{FORCE_MIN_INTERVAL, TenantKey};

/// How long a degraded [`Fetched`] entry may be served, whatever TTL the caller
/// asks for.
///
/// Not zero ("don't store"): a token that permanently cannot list `/groups` would
/// then refetch the organizations and roles on every query, and a health feed it
/// cannot read would re-page the whole device inventory with it. The floor a forced
/// refresh gets is the right bound — a minute of a blank picker, not five, and not
/// fifteen minutes of reboot reasons silently missing.
const DEGRADED_TTL: Duration = FORCE_MIN_INTERVAL;

/// What a fetch closure hands [`TenantCache::get_or_fetch`]: the value, and whether
/// it is only partial because an optional sub-feed failed and was replaced by an
/// empty fallback.
pub(super) struct Fetched<T> {
    pub(super) value: T,
    pub(super) degraded: bool,
}

impl<T> Fetched<T> {
    /// A fetch with nothing optional in it, or whose optional parts all arrived.
    pub(super) fn complete(value: T) -> Self {
        Self {
            value,
            degraded: false,
        }
    }
}

/// One tenant-stamped, TTL'd, epoch-gated, single-flight cache slot.
///
/// The same protocol was written out four times in `state.rs` — once for lookups,
/// once for the device inventory, and twice for the current-patch families via
/// `current_family`. Each copy had to get the same sequence right:
///
/// 1. sample the epoch on *entry*, before anything else;
/// 2. probe the slot (tenant match **and** within the TTL) and return a hit;
/// 3. take the single-flight gate, so the loser of a race waits instead of paging a
///    six-figure feed a second time;
/// 4. re-probe under the gate — this is what turns that wait into a cache hit;
/// 5. store only if the epoch is unchanged when the slot lock is retaken, and only
///    under a capped TTL ([`DEGRADED_TTL`]) if the fetch was degraded.
///
/// The epoch check is the subtle part and it is not optional: without it, a fetch
/// already in flight when an invalidation lands writes its pre-invalidation rows
/// straight back and restarts the TTL on exactly the data the caller wanted gone.
/// The tenant stamp cannot cover that case, because a sign-out or a post-action
/// invalidation is the *same* tenant.
///
/// The epoch is sampled at entry rather than after the gate because the caller
/// read its tenant key before calling. Sampled after the gate, a tenant switch that
/// landed while this caller was parked behind another fetch went unseen, and the
/// store proceeded under the departed tenant's key. The price is that a caller
/// which waited across a same-tenant invalidation does not cache its (fresh)
/// fetch; the next caller does.
///
/// The degraded cap exists because "an optional feed failed, fall back to empty"
/// used to be cached exactly like a clean fetch: a `/groups` blip left the Group
/// filter empty for the full five-minute lookup TTL, a health-feed blip hid every
/// reboot reason for the fifteen-minute inventory TTL, and a forced refresh cleared
/// neither.
///
/// Prose did not keep the four copies in step. `lookups_epoch`'s field comment used
/// to read "Mirrors `devices_epoch`/`current_epoch`, which the lookups cache was
/// missing" — the epoch check was simply absent there until someone noticed, and
/// the lookups slot also never grew the gate at all. Both are now structural: a
/// fifth cache cannot be added without the protocol, because the protocol is the
/// type.
///
/// A value displaced from the slot (by an invalidation or a store) is dropped only
/// after the slot lock is released. The slot holds whole-fleet `Vec`s, and freeing
/// six figures of rows under a `std` mutex on an async worker stalls every other
/// reader of the slot for the length of that free.
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
    /// The longest this entry may be served regardless of the caller's TTL:
    /// [`DEGRADED_TTL`] for a degraded fetch, unbounded otherwise.
    cap: Duration,
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
    /// `ttl` (or the entry's own cap, if shorter). One definition, used by both the
    /// pre-lock probe and the post-gate re-check so the two cannot disagree about
    /// what "fresh" means.
    pub(super) fn peek(
        &self,
        tenant: &TenantKey,
        ttl: Duration,
    ) -> Option<(Arc<T>, DateTime<Utc>)> {
        let guard = self.slot.lock().ok()?;
        let entry = guard.as_ref()?;
        (entry.tenant == *tenant && entry.at.elapsed() < ttl.min(entry.cap))
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
        Fut: std::future::Future<Output = Result<Fetched<T>>>,
    {
        // Before the gate, not after it: `tenant` was read before this call, so an
        // invalidation during the wait must decline this caller's store too.
        let epoch = self.epoch.load(Ordering::SeqCst);
        if let Some(hit) = self.peek(tenant, ttl) {
            return Ok(hit);
        }
        let _fetching = self.fetch_lock.lock().await;
        // Re-probe under the gate: the winner of the race has usually stored by now,
        // which is what makes waiting cheaper than fetching.
        if let Some(hit) = self.peek(tenant, ttl) {
            return Ok(hit);
        }
        let Fetched { value, degraded } = fetch().await?;
        let value = Arc::new(value);
        let fetched_at = Utc::now();
        if degraded {
            tracing::debug!("{label} fetched degraded; caching for {DEGRADED_TTL:?} only");
        }
        let mut displaced = None;
        if let Ok(mut guard) = self.slot.lock() {
            if self.epoch.load(Ordering::SeqCst) == epoch {
                displaced = guard.replace(CacheEntry {
                    at: Instant::now(),
                    cap: if degraded {
                        DEGRADED_TTL
                    } else {
                        Duration::MAX
                    },
                    tenant: tenant.clone(),
                    value: value.clone(),
                    fetched_at,
                });
            } else {
                tracing::debug!("{label} invalidated mid-fetch; not caching");
            }
        }
        // The slot lock is released by now, so the displaced rows are freed outside it.
        drop(displaced);
        Ok((value, fetched_at))
    }

    /// The current epoch, for tests asserting an invalidation happened.
    #[cfg(test)]
    pub(crate) fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::SeqCst)
    }

    /// Back-dates the stored entry by `by`, so a test can step past a TTL or the
    /// degraded cap without sleeping through it.
    #[cfg(test)]
    pub(super) fn age(&self, by: Duration) {
        if let Ok(mut guard) = self.slot.lock()
            && let Some(entry) = guard.as_mut()
        {
            entry.at = entry.at.checked_sub(by).expect("instant underflow");
        }
    }

    /// Drops the entry and makes any fetch already in flight decline to store.
    pub(crate) fn invalidate(&self) {
        // Bump *before* clearing, so both interleavings of a racing write lose.
        self.epoch.fetch_add(1, Ordering::SeqCst);
        let old = match self.slot.lock() {
            Ok(mut guard) => guard.take(),
            Err(_) => None,
        };
        // Outside the lock: the entry can be a whole-fleet `Vec`.
        drop(old);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tenant() -> TenantKey {
        TenantKey {
            instance_base_url: "https://app.ninjarmm.com".into(),
            client_id: None,
        }
    }

    /// A partial fetch is served only until the cap, a clean one for the full TTL.
    /// Without the cap a blip on an optional feed was cached as if it were the
    /// answer: a blank Group filter for five minutes, missing reboot reasons for
    /// fifteen.
    #[tokio::test]
    async fn a_degraded_entry_expires_at_the_cap_and_a_clean_one_does_not() {
        let ttl = Duration::from_secs(300);
        let clean: TenantCache<u32> = TenantCache::default();
        let degraded: TenantCache<u32> = TenantCache::default();
        clean
            .get_or_fetch(&tenant(), ttl, "clean", || async {
                Ok(Fetched::complete(1))
            })
            .await
            .expect("clean fetch");
        degraded
            .get_or_fetch(&tenant(), ttl, "degraded", || async {
                Ok(Fetched {
                    value: 1,
                    degraded: true,
                })
            })
            .await
            .expect("degraded fetch");
        assert!(clean.peek(&tenant(), ttl).is_some());
        assert!(
            degraded.peek(&tenant(), ttl).is_some(),
            "fresh within the cap"
        );

        let past_cap = DEGRADED_TTL + Duration::from_secs(1);
        clean.age(past_cap);
        degraded.age(past_cap);
        assert!(
            clean.peek(&tenant(), ttl).is_some(),
            "a clean entry keeps its full TTL"
        );
        assert!(
            degraded.peek(&tenant(), ttl).is_none(),
            "a degraded entry must expire at the cap, not the TTL"
        );
        // The apply preview peeks with an unbounded TTL; the cap still applies.
        assert!(degraded.peek(&tenant(), Duration::MAX).is_none());
    }

    /// The epoch used to be sampled after the gate, while the caller's tenant key
    /// was read before it. An invalidation (a tenant switch) that landed while this
    /// caller was parked behind another fetch was invisible to it, and it stored
    /// under the departed tenant's key.
    #[tokio::test]
    async fn an_invalidation_while_parked_at_the_gate_declines_the_store() {
        let cache: Arc<TenantCache<u32>> = Arc::new(TenantCache::default());
        let (release, hold) = tokio::sync::oneshot::channel::<()>();

        // The gate holder: its fetch does not finish until released.
        let holder = {
            let cache = cache.clone();
            tokio::spawn(async move {
                cache
                    .get_or_fetch(&tenant(), Duration::MAX, "holder", || async {
                        hold.await.ok();
                        Ok(Fetched::complete(1))
                    })
                    .await
                    .expect("holder")
            })
        };
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        // The waiter: misses, then parks on the gate the holder has.
        let waiter = {
            let cache = cache.clone();
            tokio::spawn(async move {
                cache
                    .get_or_fetch(&tenant(), Duration::MAX, "waiter", || async {
                        Ok(Fetched::complete(2))
                    })
                    .await
                    .expect("waiter")
            })
        };
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }

        // A tenant switch lands while the waiter is parked.
        cache.invalidate();
        release.send(()).expect("release the holder");
        holder.await.expect("join holder");
        let (value, _) = waiter.await.expect("join waiter");
        assert_eq!(*value, 2, "the waiter still fetched for its own caller");
        assert!(
            cache.peek(&tenant(), Duration::MAX).is_none(),
            "neither fetch may store across the invalidation"
        );
    }
}

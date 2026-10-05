//! The dispatched-job store, the single-claim poller slot and the confirm-token
//! slot — the mutable action state `AppState` carries between IPC calls.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tracing::warn;

use super::{AppState, TenantKey};
use crate::actions::{JobReport, MAX_JOBS};

/// How long a confirmation token issued by `plan_action` stays usable. Short
/// enough that a dialog left open over lunch fails closed rather than dispatching
/// against a fleet whose state has moved on.
const CONFIRM_TTL: Duration = Duration::from_secs(300);

/// A plan the operator has been shown and may confirm.
///
/// The hash binds the token to the exact action + device set + parameters that
/// were planned, so an altered request fails the check even with a valid token.
pub struct PendingConfirm {
    pub token: String,
    pub request_hash: String,
    pub issued_at: Instant,
    /// The session (tenant + jobs epoch) the plan was built and approved under.
    ///
    /// `request_hash` destructures `ActionRequest` exhaustively — a new field there
    /// is a compile error — but `ActionRequest` carries no instance or client id, so
    /// no amount of hashing could bind the approval to a tenant. `run_action` then
    /// re-reads `instance_base_url` from settings *after* consuming the token, which
    /// means a plan approved against instance A could dispatch against instance B
    /// inside the 5-minute window. Every other cache in `AppState` is tenant-stamped
    /// for exactly this reason; the one slot that authorizes writes to real devices
    /// was not. It has to live on the slot rather than in the hash for that reason.
    ///
    /// The epoch half covers what the tenant cannot: a sign-out and sign-in as a
    /// different operator on the same instance is the same tenant.
    session: JobSession,
}

/// The session a write-path call started under: the tenant plus the jobs epoch,
/// sampled by [`AppState::job_session`] before the call's first `.await`.
///
/// `plan_action`, `run_action` and each poll tick await the network for seconds to
/// minutes, and a sign-out, sign-in, re-authorization or tenant switch can land in
/// that gap. Every store at the end of such a call used to read `tenant_key()` at
/// *store* time, so whatever the departed session had computed — a confirmation
/// token, a dispatched batch, a poll tick's updates — was written into the new
/// session as if it belonged there. Carrying the session from the start, and
/// re-checking it under the slot lock, is the same protocol as [`QueryToken`].
///
/// [`QueryToken`]: super::QueryToken
#[derive(Clone)]
pub struct JobSession {
    tenant: TenantKey,
    epoch: u64,
}

/// RAII claim on the single job-poller slot, issued by
/// [`AppState::try_claim_job_poller`].
///
/// Dropping it releases the slot, so **every** exit from the poller task frees the
/// claim — a panic in `advance_job`/`audit::record`/`emit_progress`, a runtime
/// shutdown dropping the task, or an early `return`. The flag used to be a bare
/// `AtomicBool` cleared only inside `release_job_poller_if_idle`, reached from the
/// single "no pending jobs" arm of the loop; any other exit leaked the claim
/// permanently, after which every later `spawn_job_poller` returned immediately and
/// **no dispatched job was polled again for the life of the process** — jobs simply
/// sat at Queued while the operator watched.
///
/// [`AppState::release_job_poller_if_idle`] takes the claim by value so the release
/// can happen under the jobs lock; it hands the claim back when work is still
/// pending.
pub struct JobPollerClaim {
    flag: Arc<AtomicBool>,
}

impl Drop for JobPollerClaim {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::Release);
    }
}

impl AppState {
    /// Reserves `count` consecutive job ids, returning `(batch_id, first_job_id)`.
    pub fn next_job_ids(&self, count: usize) -> (u64, u64) {
        let batch = self.job_seq.fetch_add(1, Ordering::Relaxed);
        let base = self.job_seq.fetch_add(count as u64, Ordering::Relaxed);
        (batch, base)
    }

    /// The session a write-path call is starting under. Take it before the first
    /// `.await` and hand it to every store the call makes.
    pub fn job_session(&self) -> JobSession {
        // The epoch is read first. A tenant switch replaces the settings and *then*
        // bumps, so this order can pair an old epoch with a new tenant (stale either
        // way) but never a new epoch with the old tenant.
        let epoch = self.job_epoch.load(Ordering::SeqCst);
        JobSession {
            tenant: self.tenant_key(),
            epoch,
        }
    }

    /// Whether `session` is still the live one: no clear since it was taken, and the
    /// same tenant. Stores call this under their slot lock; the dispatch loop calls
    /// it before each send.
    pub fn job_session_is_current(&self, session: &JobSession) -> bool {
        self.job_epoch.load(Ordering::SeqCst) == session.epoch
            && self.tenant_key() == session.tenant
    }

    /// Appends newly dispatched rows for `session`, trimming history to
    /// [`MAX_JOBS`] by dropping the oldest **terminal** rows first — an in-flight
    /// job must never be evicted out from under the poller.
    ///
    /// Returns `false`, storing nothing, when the session ended while the batch was
    /// dispatching. The rows belong to the operator or tenant that left: stored, the
    /// poller would resolve their device ids against the new session's API, the
    /// invalidation would hit the new session's caches, and the Jobs tab would show
    /// them to whoever signed in next.
    #[must_use]
    pub fn append_jobs(&self, session: &JobSession, new_jobs: Vec<JobReport>) -> bool {
        let Ok(mut guard) = self.jobs.lock() else {
            warn!("job store poisoned; dispatched jobs will not appear in the Jobs tab");
            // Not a session change: the caller still reports the batch it sent.
            return true;
        };
        // Under the jobs lock: `clear_jobs` bumps before it takes this lock, so an
        // append either lands before the clear (and is wiped by it) or sees the bump.
        if !self.job_session_is_current(session) {
            warn!(
                count = new_jobs.len(),
                "session changed while the batch was dispatching; its jobs were not recorded"
            );
            return false;
        }
        let key = session.tenant.clone();
        // `insert` hands back the `&mut` directly, so the re-lookup that needed an
        // `expect` is gone. That expect was the only one in production code, and it
        // sat inside a held guard — a panic there would have poisoned the job store
        // for the rest of the process, which the arm above warns is unrecoverable.
        let jobs = match guard.as_mut() {
            Some((t, jobs)) if *t == key => jobs,
            _ => &mut guard.insert((key, Vec::new())).1,
        };
        jobs.extend(new_jobs);

        if jobs.len() > MAX_JOBS {
            let excess = jobs.len() - MAX_JOBS;
            let mut dropped = 0;
            jobs.retain(|j| {
                if dropped < excess && j.state.is_terminal() {
                    dropped += 1;
                    return false;
                }
                true
            });
        }
        true
    }

    /// Applies polled updates, matching on `JobReport.id`. Rows the caller no
    /// longer knows about are left untouched.
    pub fn apply_job_updates(&self, updates: Vec<JobReport>) {
        let key = self.tenant_key();
        let Ok(mut guard) = self.jobs.lock() else {
            return;
        };
        let Some((t, jobs)) = guard.as_mut() else {
            return;
        };
        if *t != key {
            return;
        }
        for update in updates {
            if let Some(slot) = jobs.iter_mut().find(|j| j.id == update.id) {
                *slot = update;
            }
        }
    }

    /// Clone-out of the jobs still awaiting a terminal state. Returns owned rows so
    /// the lock is released before the poller's `.await`s.
    pub fn pending_jobs(&self) -> Vec<JobReport> {
        self.jobs_snapshot()
            .into_iter()
            .filter(|j| !j.state.is_terminal())
            .collect()
    }

    /// All jobs for the current tenant, newest last. Empty after a tenant switch.
    pub fn jobs_snapshot(&self) -> Vec<JobReport> {
        let key = self.tenant_key();
        self.jobs
            .lock()
            .ok()
            .and_then(|g| match g.as_ref() {
                Some((t, jobs)) if *t == key => Some(jobs.clone()),
                _ => None,
            })
            .unwrap_or_default()
    }

    /// Ends the job session on sign-out, sign-in, re-authorization or an instance
    /// change: dispatch history and any pending confirmation are dropped, and every
    /// plan, dispatch or poll tick still in flight is refused at its store.
    pub fn clear_jobs(&self) {
        // Bumped *before* the slots are cleared, as `clear_last_result` does: each
        // store re-reads the epoch under its slot lock, so whichever order the two
        // interleave, the departed session's write loses.
        self.job_epoch.fetch_add(1, Ordering::SeqCst);
        self.clear_job_history();
    }

    /// The Jobs tab's Clear: drops the history and any pending confirmation without
    /// ending the session. A batch still dispatching keeps sending and records its
    /// rows when it finishes, as it always has — the operator asked to tidy a list,
    /// not to cancel the devices queued behind the semaphore.
    pub fn clear_job_history(&self) {
        if let Ok(mut guard) = self.jobs.lock() {
            *guard = None;
        }
        if let Ok(mut guard) = self.pending_confirm.lock() {
            *guard = None;
        }
    }

    /// Claims the single poller slot. `None` means one is already running and the
    /// caller should just let it pick up the new batch.
    ///
    /// The claim is an RAII guard: dropping it releases the slot. That is what makes
    /// the flag safe to hold across a long-running task — see [`JobPollerClaim`].
    pub fn try_claim_job_poller(&self) -> Option<JobPollerClaim> {
        self.job_poller_running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
            .then(|| JobPollerClaim {
                flag: Arc::clone(&self.job_poller_running),
            })
    }

    /// Releases the poller claim **only if** no unresolved job remains. Returns
    /// `None` when the claim was released and the caller should stop polling, and
    /// `Some(claim)` — the claim handed back — when work appeared and it must keep
    /// going.
    ///
    /// Closes a lost-wakeup race. The poller used to break out of its loop on an
    /// empty pending set and release the claim afterwards; a batch dispatched in
    /// that gap recorded its jobs, then found `try_claim_job_poller` still taken
    /// because the flag had not been cleared yet — so nothing polled it, and those
    /// jobs sat unresolved until some later dispatch happened to start a new poller.
    ///
    /// The check and the release happen under the jobs lock, and dispatch records
    /// its jobs before calling `try_claim_job_poller`. A concurrent dispatch is
    /// therefore either visible here (we keep polling) or strictly after the release
    /// (its own claim succeeds). There is no order in which it is neither.
    pub fn release_job_poller_if_idle(&self, claim: JobPollerClaim) -> Option<JobPollerClaim> {
        let Ok(guard) = self.jobs.lock() else {
            // A poisoned job store cannot be polled meaningfully; release so a
            // later dispatch can at least try.
            drop(claim);
            return None;
        };
        let key = self.tenant_key();
        let has_pending = guard.as_ref().is_some_and(|(tenant, jobs)| {
            *tenant == key && jobs.iter().any(|j| !j.state.is_terminal())
        });
        if has_pending {
            return Some(claim);
        }
        // Dropped before `guard`, so the release still happens under the jobs lock —
        // that ordering is the whole point of the check above.
        drop(claim);
        drop(guard);
        None
    }

    /// Records the plan the operator is being asked to confirm, replacing any
    /// earlier one — only one dialog is open at a time.
    ///
    /// `session` is the one taken before the plan was built. Returns `false`, storing
    /// nothing, when it has ended since: building a plan awaits the device inventory
    /// and the org names, and a sign-out or tenant switch in that gap used to leave a
    /// token stamped for the *new* session — which `run_action`'s re-plan, now under
    /// the new tenant, would then match.
    #[must_use]
    pub fn store_pending_confirm(
        &self,
        session: &JobSession,
        token: String,
        request_hash: String,
    ) -> bool {
        let Ok(mut guard) = self.pending_confirm.lock() else {
            return false;
        };
        // Under the slot lock, for the same reason as `append_jobs`.
        if !self.job_session_is_current(session) {
            return false;
        }
        *guard = Some(PendingConfirm {
            token,
            request_hash,
            issued_at: Instant::now(),
            session: session.clone(),
        });
        true
    }

    /// Consumes a confirmation token, returning whether it authorizes this exact
    /// request. Single-use: the slot is cleared on any match attempt, so a
    /// double-click can't dispatch twice.
    ///
    /// Both secrets are compared in constant time. The realistic threat here is low —
    /// the slot holds one token at a time, it is single-use, it expires in five
    /// minutes, and the only party that can present one is the frontend that was
    /// handed it — but `==` on a `String` returns on the first differing byte, and
    /// this is the gate standing between a stale or modified frontend and a fleet-wide
    /// reboot. A comparison that leaks nothing costs nothing here.
    pub fn consume_confirm_token(&self, token: &str, request_hash: &str) -> bool {
        let Ok(mut guard) = self.pending_confirm.lock() else {
            return false;
        };
        let Some(pending) = guard.take() else {
            return false;
        };
        // Not short-circuiting: both comparisons run regardless of the first result.
        let token_ok = constant_time_eq(pending.token.as_bytes(), token.as_bytes());
        let hash_ok = constant_time_eq(pending.request_hash.as_bytes(), request_hash.as_bytes());
        // The tenant is compared here rather than folded into `request_hash` because
        // `ActionRequest` has no field to hash it from — see `PendingConfirm::tenant`.
        // An approval is for one instance; the operator can change instance in
        // Settings while the dialog is open.
        token_ok & hash_ok
            && self.job_session_is_current(&pending.session)
            && pending.issued_at.elapsed() < CONFIRM_TTL
    }
}

/// Byte-equality that does not return early on the first difference.
///
/// The length check is deliberately *not* constant time — the length of a token is
/// not the secret, and both sides here are fixed-width by construction.
pub(super) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

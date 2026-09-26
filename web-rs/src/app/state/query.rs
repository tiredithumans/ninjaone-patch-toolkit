//! Running a query and owning its result: the Run/refresh/auto-refresh paths, the
//! queued run, the post-refresh selection prune, the demo query, drill-downs from
//! the rollups, and clearing the result on a session change.

use super::*;

impl AppState {
    /// Snapshots the active filters for the applied-filter chips, resolving org/loc/role
    /// ids to display names and severity raw values to labels. All reads are untracked
    /// (this runs imperatively at Run time, not inside a reactive scope).
    pub(in crate::app) fn snapshot_filters(self) -> AppliedFilters {
        let statuses = self.filters.statuses.get_untracked();
        // An operator reading "12 failures" must be able to see it meant "12 in 30
        // days" — see `util::needs_install_window`.
        let install_days = util::needs_install_window(&statuses)
            .then(|| self.filters.install_days.get_untracked());

        let organizations = util::names_for(
            &self.filters.org_ids.get_untracked(),
            self.lookups.orgs.get_untracked().into_iter(),
        );
        let locations = util::names_for(
            &self.filters.loc_ids.get_untracked(),
            self.lookups.locations.get_untracked().into_iter(),
        );
        let roles = util::names_for(
            &self.filters.role_ids.get_untracked(),
            self.lookups.roles.get_untracked().into_iter(),
        );
        let selected = self.filters.selected_classes.get_untracked();
        let os_types = self
            .lookups
            .node_classes
            .get_untracked()
            .into_iter()
            .filter(|nc| selected.contains(&nc.value))
            .map(|nc| nc.label)
            .collect();
        let sev_raw = self.filters.selected_severities.get_untracked();
        let severities = SEVERITY_OPTIONS
            .iter()
            .filter(|(v, _)| sev_raw.iter().any(|s| s == v))
            .map(|(_, label)| label.to_string())
            .collect();

        AppliedFilters {
            organizations,
            locations,
            roles,
            os_types,
            os_name: non_empty(self.filters.os_name.get_untracked()),
            patch_type: self.filters.patch_type.get_untracked(),
            statuses,
            severities,
            search: non_empty(self.filters.search.get_untracked()),
            detected_window: self.filters.detected_window.get_untracked(),
            detected_after: self.filters.detected_after_date.get_untracked(),
            detected_before: self.filters.detected_before_date.get_untracked(),
            install_days,
        }
    }

    /// Manual **Run query** / filter change: re-scopes the cached whole-fleet data
    /// client-side (no refetch unless the cache is cold or past its staleness bound).
    pub(in crate::app) fn run_query(self) {
        self.run_query_inner(false, false);
    }

    /// Auto-refresh variant: flags a subtle `refreshing` state instead of the main
    /// `busy` one (so the Run-query button doesn't flicker each tick) and stays
    /// quiet about precondition failures. Forces a refetch of the live patch data —
    /// the point of the cadence is fresh patch state during a patching operation.
    pub(in crate::app) fn run_query_auto(self) {
        self.run_query_inner(true, true);
    }

    /// Manual ↻ **Refresh**: user-initiated refetch of the live patch data for the
    /// current filter (shows the main busy/progress, unlike the silent auto tick).
    pub(in crate::app) fn refresh_now(self) {
        self.run_query_inner(false, true);
    }

    pub(in crate::app) fn run_query_inner(self, silent: bool, force: bool) {
        let statuses = self.filters.statuses.get_untracked();
        // The guard chain lives in `util::run_decision` so its ordering is testable;
        // this method only carries out the decision.
        match util::run_decision(
            self.run.busy.get_untracked(),
            self.run.refreshing.get_untracked(),
            silent,
            self.session.demo.get_untracked(),
            self.is_authed(),
            statuses.is_empty(),
        ) {
            util::RunDecision::AlreadyRunning => return,
            util::RunDecision::Queue => {
                self.run.queued.update(|q| *q = util::queue_run(*q, force));
                return;
            }
            util::RunDecision::Demo => {
                self.run_demo_query(silent);
                return;
            }
            util::RunDecision::NotSignedIn => {
                if !silent {
                    self.notify(Toast::err("Sign in first"));
                }
                return;
            }
            util::RunDecision::NoStatusSelected => {
                if !silent {
                    self.notify(Toast::err("Select at least one status"));
                }
                return;
            }
            util::RunDecision::Run => {}
        }
        let args = PatchQueryArgs {
            filter: self.current_filter(),
            patch_type: self.filters.patch_type.get_untracked(),
            statuses,
            install_after_days: Some(self.filters.install_days.get_untracked()),
        };
        // Snapshot the filters driving this run; applied only if the query succeeds, so
        // a failed run leaves the chips matching the still-displayed prior result.
        let snapshot = self.snapshot_filters();
        // Stamp this run so progress events from a superseded run are ignored, and
        // clear the previous run's counts.
        let seq = util::next_query_seq(self.run.query_seq.get_untracked());
        self.run.query_seq.set(seq);
        self.run.progress.set(Progress::default());
        let flag = if silent {
            self.run.refreshing
        } else {
            self.run.busy
        };
        let started = js_sys::Date::now();
        self.run.query_started_ms.set(started);
        flag.set(true);
        spawn_local(async move {
            let outcome = api::query_patches(args, seq, force).await;
            // Apply only if this is still the newest run. Queries overlap routinely
            // (an auto-refresh tick fires while a manual Run is still paging the
            // fleet) and they do not resolve in start order, so without this a
            // superseded response could overwrite a newer one on screen — while the
            // backend, which now drops the superseded *cache* write, kept the newer
            // rows. The table would then disagree with paging and export.
            //
            // Not an early return: this run still owns the busy/refreshing flag it
            // set, and a manual Run superseded by a refresh tick would otherwise
            // leave `busy` stuck on forever.
            let superseded = util::is_superseded(self.run.query_seq.get_untracked(), seq);
            if superseded {
                flag.set(false);
                self.run_queued();
                return;
            }
            match outcome {
                Ok(r) => {
                    // Jump back to page 1 on a manual run; an auto-refresh keeps the
                    // current page, clamped in case the new result is shorter. The
                    // bound comes from whichever collection this view pages — see
                    // `util::paged_total`, which is also what sizes the pager itself,
                    // so the two cannot disagree about how many pages exist.
                    let grouped = self.query.group_by.get_untracked().is_some();
                    let total = util::paged_total(
                        grouped,
                        r.rows_total,
                        self.query.groups_total.get_untracked(),
                    );
                    let page = if silent {
                        util::clamp_page(
                            self.query.patches_page.get_untracked(),
                            util::page_count(total, PATCHES_PAGE_SIZE),
                        )
                    } else {
                        // A manual run returns to page 1 in the canonical order.
                        self.query.patches_sort.set(None);
                        0
                    };
                    self.query.patches_page.set(page);
                    // Fetch only the collection the active view renders, mirroring
                    // `set_group_by`. A grouped view is built from group headers and
                    // per-group member pages, none of which ride along with the
                    // summary — so without this the previous query's headers and
                    // cached members stayed on screen against the new result's counts,
                    // and re-ticking a checkbox could select a device/patch pair that
                    // isn't in the current result at all. `reset_members` also
                    // invalidates a member fetch still in flight, which would
                    // otherwise land old-result rows in the new view (and, from a
                    // group checkbox, in the selection). The flat rows behind a
                    // grouped view are never drawn, so fetching them alongside was a
                    // wasted round trip on every auto-refresh tick; switching back to
                    // flat re-fetches page 0 through `set_group_by`.
                    self.query.reset_members();
                    if grouped {
                        self.fetch_groups(page);
                    } else if page == 0 && self.query.patches_sort.get_untracked().is_none() {
                        // Page 0 ships inline with the summary (canonical order), so
                        // seed it directly; a later page — or a silent refresh with an
                        // active sort — is fetched instead. Stamped like a fetch, so
                        // a page request still in flight cannot overwrite it.
                        self.query.next_view_seq();
                        self.query.page_rows.set(r.rows.clone());
                    } else {
                        self.fetch_page(page);
                    }
                    // Reclaim the fold. The filter panel is ~487px tall and always
                    // opened expanded, so on the app's own default window not one
                    // patch row was visible on first paint — the operator scrolled
                    // past the controls to reach the thing they ran the query for.
                    // Only when they have expressed no preference: an explicit
                    // toggle is remembered and always wins.
                    if !silent && api::ui_pref(api::PREF_FILTERS_COLLAPSED).is_none() {
                        self.ui.filters_collapsed.set(true);
                    }
                    self.query.result.set(Some(r));
                    self.query.applied_filters.set(Some(snapshot));
                    self.query.query_error.set(None);
                    if silent {
                        // Same scope, fresher data: keep what the operator ticked,
                        // minus anything this refresh no longer lists.
                        self.prune_selection_after_refresh(seq);
                    } else {
                        // A new scope: a selection made against the previous result
                        // no longer describes what is on screen.
                        self.clear_selection();
                    }
                    self.actions.results_stale.set(false);
                }
                // The toast announces the failure (aria-live); the banner keeps it
                // visible after the toast auto-dismisses.
                Err(e) => {
                    self.query.query_error.set(Some(e.clone()));
                    self.notify(Toast::err(e));
                }
            }
            // Record the round-trip so the next run can show "Last run took Ns"
            // and drive the estimated progress bar.
            self.run
                .last_duration_ms
                .set(Some(js_sys::Date::now() - started));
            flag.set(false);
            self.run_queued();
        });
    }

    /// Starts the manual run queued behind the one that just settled, if any.
    fn run_queued(self) {
        let mut queued = self.run.queued.get_untracked();
        if let Some(force) = util::take_queued_run(
            &mut queued,
            self.run.busy.get_untracked(),
            self.run.refreshing.get_untracked(),
        ) {
            self.run.queued.set(queued);
            self.run_query_inner(false, force);
        }
    }

    /// After a silent refresh, re-checks every selected device against its rows in
    /// the new result and drops the ticked patches that are gone (installed,
    /// rejected, or the device left the scope) — see `util::prune_device_selection`.
    ///
    /// Reads the backend's by-device grouping, which serves from the result just
    /// cached — no NinjaOne traffic. Abandoned if another run lands meanwhile: a
    /// manual run clears the selection itself, and a newer refresh prunes again.
    fn prune_selection_after_refresh(self, seq: u64) {
        let devices: Vec<i64> = self
            .actions
            .selected
            .with_untracked(|s| s.keys().copied().collect());
        if devices.is_empty() {
            return;
        }
        spawn_local(async move {
            let mut fresh = Vec::with_capacity(devices.len());
            for id in devices {
                // A failed (or full, so possibly partial) read keeps that device
                // as it was rather than drop a selection the operator can't
                // rebuild from memory; the backend re-plans against live state
                // before any dispatch anyway.
                if let Ok(rows) = api::get_patch_group_members(
                    GroupBy::Device,
                    id.to_string(),
                    0,
                    SELECTION_PRUNE_LIMIT,
                )
                .await
                    && rows.len() < SELECTION_PRUNE_LIMIT
                {
                    fresh.push((id, rows));
                }
            }
            if util::is_superseded(self.run.query_seq.get_untracked(), seq) {
                return;
            }
            let mut removed = 0;
            self.actions.selected.update(|sel| {
                for (id, rows) in &fresh {
                    removed += util::prune_device_selection(sel, *id, rows);
                }
            });
            if removed > 0 {
                self.notify(Toast::ok(format!(
                    "Auto-refresh: {removed} selected patch row(s) are no longer listed and were deselected"
                )));
            }
        });
    }

    /// Narrows the filters to one organization and shows the matching patch rows.
    ///
    /// The rollup tabs used to be terminal: reading "Contoso · 63% · 41 pending
    /// Critical/Important" gave the operator no way to reach those 41 rows except to
    /// scroll back to Filters, re-pick the org by hand, tick Severity, press Run and
    /// switch tabs. That is what made five tabs read as five disconnected reports
    /// rather than one console — and it was self-inflicted, because the whole fleet is
    /// already cached backend-side, so an org/severity narrowing is a client-side
    /// re-filter with zero HTTP calls.
    ///
    /// Matching on the display name is deliberate: the compliance rollup is keyed by
    /// org *name* (`ComplianceBucket.organization`) because that is what labels a row,
    /// and the backend's synthetic `(unknown)` bucket has no id at all. An unmatched
    /// name leaves the org scope alone rather than silently clearing it.
    pub(in crate::app) fn drill_to_org(self, organization: String) {
        let id = self
            .lookups
            .orgs
            .with_untracked(|orgs| orgs.iter().find(|o| o.name == organization).map(|o| o.id));
        match id {
            // Replaces the org scope rather than adding to it: a drill-down means
            // "show me this org's rows", not "add this org to whatever was picked".
            Some(id) => {
                self.filters.org_ids.set(vec![id]);
                self.reload_locations();
            }
            None => self.notify(Toast::err(format!(
                "No organization named \"{organization}\" in the current scope — showing every org."
            ))),
        }
        self.drill_run();
    }

    /// Narrows to a single severity band and shows the matching patch rows. Replaces
    /// the selection rather than adding to it, so clicking a chart segment shows that
    /// segment and not an accumulation of everything clicked before it.
    pub(in crate::app) fn drill_to_severity(self, severity: String) {
        self.filters.selected_severities.set(vec![severity]);
        self.drill_run();
    }

    /// Narrows to one patch by KB (or by name when the patch carries no KB — third
    /// party patches have no `kbNumber`) and shows the affected rows. `search_allowed`
    /// matches the needle against both fields, so one control covers both cases.
    pub(in crate::app) fn drill_to_patch(self, needle: String) {
        self.filters.search.set(needle);
        self.drill_run();
    }

    /// Shared tail of every drill-down: re-run and land the operator on the rows.
    ///
    /// Flat view on purpose — a drill-down is a request to see *the rows behind this
    /// number*, and a grouped view would re-collapse them behind headers. Filters are
    /// left expanded/collapsed as the operator had them; the chip row already reports
    /// what the drill-down applied.
    fn drill_run(self) {
        self.query.patches_page.set(0);
        self.set_group_by(None);
        self.ui.active_tab.set(Tab::Patches);
        self.run_query();
    }

    /// Demo-mode counterpart to `run_query`: filters the in-memory sample with the
    /// current facets (no backend, no auth) and recomputes the row count.
    pub(in crate::app) fn run_demo_query(self, silent: bool) {
        let statuses = self.filters.statuses.get_untracked();
        if statuses.is_empty() {
            if !silent {
                self.notify(Toast::err("Select at least one status"));
            }
            return;
        }
        let r = demo::filtered_result(
            &self.current_filter(),
            &self.filters.patch_type.get_untracked(),
            &statuses,
            Some(self.filters.install_days.get_untracked()),
        );
        self.query.patches_page.set(0);
        // A run returns to the canonical order, as on the live path; leaving the
        // sort in place drew a ▲ on a header over rows that were not sorted by it.
        self.query.patches_sort.set(None);
        self.query.result.set(Some(r));
        // Same reason as the live path: a grouped view's headers and members don't
        // ride along with the result, so they'd otherwise describe the last query.
        // Both fetches re-derive from `demo_rows()`, which reads the result just set.
        self.query.reset_members();
        if self.query.group_by.get_untracked().is_some() {
            self.fetch_groups(0);
        } else {
            self.fetch_page(0);
        }
        self.clear_selection();
        self.query
            .applied_filters
            .set(Some(self.snapshot_filters()));
        self.query.query_error.set(None);
    }

    /// Everything the backend drops on sign-out, sign-in, re-authorization and a
    /// tenant switch (`commands::auth::clear_session_state`): the cached result
    /// *and* the job store *and* any pending confirmation. The frontend used to
    /// refresh only the auth badge, so the table, the selection and the Jobs list
    /// stayed rendered over a cache that was already gone — the next page came back
    /// blank under "Rows 101–200 of N", and Export said "Run a query before
    /// exporting" beside a visible table.
    pub(in crate::app) fn clear_session(self) {
        self.clear_results();
        self.actions.jobs.set(Vec::new());
        self.actions.pending.set(None);
        self.actions.confirm_input.set(String::new());
        self.actions.dispatch_error.set(None);
        // A run queued behind one from the old session would fire into the new
        // one (or, signed out, just to say "Sign in first").
        self.run.queued.set(None);
    }

    /// Drops everything derived from the last query: the summary, the current page,
    /// grouping state, the applied-filter chips and any device selection made
    /// against those rows.
    pub(in crate::app) fn clear_results(self) {
        self.query.result.set(None);
        self.query.page_rows.set(Vec::new());
        self.query.patches_page.set(0);
        self.query.patches_sort.set(None);
        self.query.groups.set(Vec::new());
        self.query.reset_members();
        // Invalidates a page/header request still in flight for the dropped result.
        self.query.next_view_seq();
        self.query.applied_filters.set(None);
        self.query.query_error.set(None);
        self.clear_selection();
        self.actions.results_stale.set(false);
    }
}

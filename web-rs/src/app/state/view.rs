//! The Patches-table view over the cached result: paging, sorting, flat vs
//! grouped mode, and loading/ticking a group's members.

use super::*;

impl AppState {
    /// Loads the detail rows for `page` from the backend's cached result into
    /// `page_rows`. Paging fetches just the visible window rather than holding the
    /// whole row set in the frontend. Demo mode pages (and sorts) its in-memory
    /// sample instead — there is no backend to ask, and switching the demo back to
    /// the Flat view used to fail with "only available in the desktop app".
    pub(in crate::app) fn fetch_page(self, page: usize) {
        let sort = self.query.patches_sort.get_untracked();
        let seq = self.query.next_view_seq();
        if self.session.demo.get_untracked() {
            let mut rows = self.demo_rows();
            if let Some(sort) = sort {
                sort_patch_rows(&mut rows, sort);
            }
            let (start, end) = util::page_bounds(page, PATCHES_PAGE_SIZE, rows.len());
            self.query
                .page_rows
                .set(rows.drain(start..end).map(Arc::new).collect());
            self.query.query_error.set(None);
            return;
        }
        spawn_local(async move {
            let outcome =
                api::get_patch_rows(page * PATCHES_PAGE_SIZE, PATCHES_PAGE_SIZE, sort).await;
            if util::is_superseded(self.query.view_seq.get_untracked(), seq) {
                return;
            }
            match outcome {
                Ok(rows) => {
                    self.query
                        .page_rows
                        .set(rows.into_iter().map(Arc::new).collect());
                    self.query.query_error.set(None);
                }
                Err(e) => {
                    self.query.query_error.set(Some(e.clone()));
                    self.notify(Toast::err(e));
                }
            }
        });
    }
    /// Switches the Patches view between flat rows and a grouped view.
    ///
    /// Resets paging, every expand and the previous mode's group headers, because
    /// group keys and page offsets mean different things in each mode — carrying
    /// them over would open arbitrary groups, and By device ↔ By patch briefly
    /// rendered the other mode's headers until the new page landed. Demo mode groups
    /// its in-memory sample instead of round-tripping.
    pub(in crate::app) fn set_group_by(self, group_by: Option<GroupBy>) {
        if self.query.group_by.get_untracked() == group_by {
            return;
        }
        self.query.group_by.set(group_by);
        self.query.patches_page.set(0);
        self.query.groups.set(Vec::new());
        self.query.reset_members();
        match group_by {
            None => self.fetch_page(0),
            Some(_) => self.fetch_groups(0),
        }
    }

    /// Loads one page of group headers for the active grouping.
    pub(in crate::app) fn fetch_groups(self, page: usize) {
        let Some(group_by) = self.query.group_by.get_untracked() else {
            return;
        };
        let seq = self.query.next_view_seq();
        if self.session.demo.get_untracked() {
            let all = demo::group_rows(&self.demo_rows(), group_by);
            self.query.groups_total.set(all.len());
            self.query.groups.set(
                all.into_iter()
                    .skip(page * PATCHES_PAGE_SIZE)
                    .take(PATCHES_PAGE_SIZE)
                    .collect(),
            );
            return;
        }
        spawn_local(async move {
            let outcome =
                api::get_patch_groups(group_by, page * PATCHES_PAGE_SIZE, PATCHES_PAGE_SIZE).await;
            // A newer header request (another page, another mode, a new result)
            // owns the view now.
            if util::is_superseded(self.query.view_seq.get_untracked(), seq) {
                return;
            }
            match outcome {
                Ok(response) => {
                    self.query.groups_total.set(response.total);
                    self.query.query_error.set(None);
                    // The caller could only bound `page` by the *previous* result's
                    // group total; this response carries the real one. If the new
                    // result is shorter the request was past the end and came back
                    // empty, so land on the last real page instead of showing an
                    // empty table. Re-entrant exactly once: the retry is issued
                    // against the total we just stored, so its own clamp is a no-op.
                    let clamped =
                        util::clamp_page(page, util::page_count(response.total, PATCHES_PAGE_SIZE));
                    if clamped != page {
                        self.query.patches_page.set(clamped);
                        self.fetch_groups(clamped);
                        return;
                    }
                    self.query.groups.set(response.groups);
                }
                Err(e) => {
                    self.query.query_error.set(Some(e.clone()));
                    self.notify(Toast::err(e));
                }
            }
        });
    }

    /// Fetches one group's members (capped at `GROUP_MEMBER_LIMIT`), from the demo
    /// sample or the backend. `None` when the view moved on while it was loading —
    /// a new result or a new grouping — in which case the rows belong to a result
    /// no longer on screen and must be neither cached nor selected.
    async fn load_group_members(
        self,
        group_by: GroupBy,
        key: String,
    ) -> Option<Result<Vec<PatchRow>, String>> {
        if self.session.demo.get_untracked() {
            return Some(Ok(demo::group_members(&self.demo_rows(), group_by, &key)));
        }
        let generation = self.query.members_gen.get_untracked();
        let outcome = api::get_patch_group_members(group_by, key, 0, GROUP_MEMBER_LIMIT).await;
        let stale = util::is_superseded(self.query.members_gen.get_untracked(), generation)
            || self.query.group_by.get_untracked() != Some(group_by);
        (!stale).then_some(outcome)
    }

    /// Opens or closes a group, fetching its members the first time it opens.
    /// Members are cached per key, so re-opening is free and a collapse doesn't
    /// discard what was already loaded.
    pub(in crate::app) fn toggle_group(self, key: String) {
        let open = self.query.expanded.with_untracked(|e| e.contains(&key));
        if open {
            self.query.expanded.update(|e| {
                e.remove(&key);
            });
            return;
        }
        self.query.expanded.update(|e| {
            e.insert(key.clone());
        });
        if self.query.members.with_untracked(|m| m.contains_key(&key)) {
            return;
        }
        let Some(group_by) = self.query.group_by.get_untracked() else {
            return;
        };
        spawn_local(async move {
            let Some(outcome) = self.load_group_members(group_by, key.clone()).await else {
                return;
            };
            match outcome {
                Ok(rows) => self.query.members.update(|m| {
                    m.insert(key, Arc::new(rows));
                }),
                Err(e) => {
                    // Leave the group open but empty and say why, rather than
                    // silently collapsing it back under the operator.
                    self.query.members.update(|m| {
                        m.insert(key, Arc::default());
                    });
                    self.notify(Toast::err(e));
                }
            }
        });
    }

    /// The sample rows behind demo-mode paging and grouping — the displayed
    /// result's rows, in the sample's canonical order.
    fn demo_rows(self) -> Vec<PatchRow> {
        self.query
            .result
            .with_untracked(|r| r.as_ref().map(|r| r.rows.clone()).unwrap_or_default())
    }

    /// Ticks or clears every member of a group, loading them first if the group has
    /// never been expanded — otherwise the checkbox on a collapsed group would
    /// silently do nothing.
    ///
    /// Members are capped at `GROUP_MEMBER_LIMIT`, so one click can never select
    /// more rows than the expanded group would show. Ticking a group whose members
    /// the operator has not seen also opens it and says how many rows it took: a
    /// by-patch group can hold hundreds of devices, and selecting them behind a
    /// collapsed header left the only trace in the action bar's running total.
    pub(in crate::app) fn toggle_group_selection(self, key: &str, label: String, checked: bool) {
        if let Some(rows) = self.query.members.with_untracked(|m| m.get(key).cloned()) {
            self.toggle_rows_selection(&rows, checked);
            return;
        }
        let Some(group_by) = self.query.group_by.get_untracked() else {
            return;
        };
        let key = key.to_string();
        spawn_local(async move {
            let Some(outcome) = self.load_group_members(group_by, key.clone()).await else {
                return;
            };
            match outcome {
                Ok(rows) => {
                    self.toggle_rows_selection(&rows, checked);
                    if checked {
                        self.notify(Toast::ok(util::group_selection_note(
                            &label,
                            rows.len(),
                            rows.len() >= GROUP_MEMBER_LIMIT,
                        )));
                        self.query.expanded.update(|e| {
                            e.insert(key.clone());
                        });
                    }
                    self.query.members.update(|m| {
                        m.insert(key, Arc::new(rows));
                    });
                }
                Err(e) => self.notify(Toast::err(e)),
            }
        });
    }

    /// `(all, some)` ticked state for a group's loaded members, for its checkbox.
    /// Reads both maps in place — no copy of up to `GROUP_MEMBER_LIMIT` rows or of
    /// the selection; the view wraps it in one `Memo` per group header.
    pub(in crate::app) fn group_selection_state(self, key: &str) -> (bool, bool) {
        self.query.members.with(|m| {
            self.actions
                .selected
                .with(|sel| util::rows_selection_state(m.get(key).map(|r| r.as_slice()), sel))
        })
    }

    /// Pages in the Patches view as it is currently shown — rows when flat, group
    /// headers when grouped (see `util::paged_total`).
    pub(in crate::app) fn patches_page_count(self) -> usize {
        let grouped = self.query.group_by.get().is_some();
        let rows_total = self
            .query
            .result
            .with(|r| r.as_ref().map_or(0, |r| r.rows_total));
        let total = util::paged_total(grouped, rows_total, self.query.groups_total.get());
        util::page_count(total, PATCHES_PAGE_SIZE)
    }

    /// Moves the Patches view to `target` and fetches that page — of headers or of
    /// rows, matching what the view is actually showing.
    pub(in crate::app) fn go_to_patches_page(self, target: usize) {
        self.query.patches_page.set(target);
        if self.query.group_by.get_untracked().is_some() {
            self.fetch_groups(target);
        } else {
            self.fetch_page(target);
        }
    }

    /// Opens the device drill-down for `device_id` and loads it. `name` is what the
    /// operator clicked, shown until the detail lands. A row with no device (the
    /// orphan sentinel) has nothing to drill into.
    pub(in crate::app) fn open_device(self, device_id: i64, name: String) {
        if device_id == util::ORPHAN_DEVICE_ID {
            return;
        }
        self.query.drill.set(Some(DeviceDrill {
            device_id,
            name,
            load: DrillLoad::Loading,
        }));
        self.load_device_detail(device_id);
    }

    pub(in crate::app) fn close_device(self) {
        self.query.drill.set(None);
        // A load still in flight must not reopen it.
        self.query
            .drill_seq
            .update(|s| *s = util::next_query_seq(*s));
    }

    /// Reloads an open drill-down against the result just put on screen — an
    /// auto-refresh or re-run keeps the dialog but not its old numbers.
    pub(in crate::app) fn reload_device_detail(self) {
        if let Some(id) = self
            .query
            .drill
            .with_untracked(|d| d.as_ref().map(|d| d.device_id))
        {
            self.load_device_detail(id);
        }
    }

    /// Fetches the drill-down from the backend's cached result, or builds it from
    /// the sample in demo mode, and applies it only if it is still the newest load
    /// for the device on screen.
    fn load_device_detail(self, device_id: i64) {
        let seq = util::next_query_seq(self.query.drill_seq.get_untracked());
        self.query.drill_seq.set(seq);
        let apply = move |outcome: Result<Option<DeviceDetail>, String>| {
            if util::is_superseded(self.query.drill_seq.get_untracked(), seq) {
                return;
            }
            self.query.drill.update(|drill| {
                if let Some(drill) = drill.as_mut().filter(|d| d.device_id == device_id) {
                    drill.load = match outcome {
                        Ok(Some(detail)) => DrillLoad::Ready(Box::new(detail)),
                        Ok(None) => DrillLoad::Gone,
                        Err(e) => DrillLoad::Failed(e),
                    };
                }
            });
        };
        if self.session.demo.get_untracked() {
            // Read off the Run-time snapshot, not the live controls: it is the
            // query on screen that did or did not ask about failures.
            let failed_queried = self.query.applied_filters.with_untracked(|a| {
                a.as_ref()
                    .is_some_and(|a| a.statuses.iter().any(|s| s == "FAILED"))
            });
            let detail = self.query.result.with_untracked(|r| {
                r.as_ref()
                    .and_then(|r| demo::device_detail(r, device_id, failed_queried))
            });
            apply(Ok(detail));
            return;
        }
        spawn_local(async move { apply(api::device_detail(device_id).await) });
    }

    /// Cycles a Patches-table column through none → ascending → descending and
    /// re-fetches page 1 in the new order (demo mode re-sorts its in-memory sample
    /// inside `fetch_page`).
    pub(in crate::app) fn cycle_sort(self, key: RowSortKey) {
        let next = next_sort(self.query.patches_sort.get_untracked(), key);
        self.query.patches_sort.set(next);
        self.query.patches_page.set(0);
        self.fetch_page(0);
    }
}

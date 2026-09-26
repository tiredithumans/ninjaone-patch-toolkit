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
            self.query.page_rows.set(rows[start..end].to_vec());
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
                    self.query.page_rows.set(rows);
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
                    m.insert(key, rows);
                }),
                Err(e) => {
                    // Leave the group open but empty and say why, rather than
                    // silently collapsing it back under the operator.
                    self.query.members.update(|m| {
                        m.insert(key, Vec::new());
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
            for row in &rows {
                self.toggle_row_selection(row, checked);
            }
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
                    for row in &rows {
                        self.toggle_row_selection(row, checked);
                    }
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
                        m.insert(key, rows);
                    });
                }
                Err(e) => self.notify(Toast::err(e)),
            }
        });
    }

    /// `(all, some)` ticked state for a group's loaded members, for its checkbox.
    pub(in crate::app) fn group_selection_state(self, key: &str) -> (bool, bool) {
        let rows = self
            .query
            .members
            .with(|m| m.get(key).cloned().unwrap_or_default());
        if rows.is_empty() {
            return (false, false);
        }
        let n = rows.iter().filter(|r| self.is_row_selected(r)).count();
        (n == rows.len(), n > 0 && n < rows.len())
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

//! The per-patch-row selection the action bar dispatches against.

use super::*;

impl AppState {
    /// Whether this exact patch row is ticked.
    pub(in crate::app) fn is_row_selected(self, row: &PatchRow) -> bool {
        let key = patch_key(row);
        self.actions.selected.with(|sel| {
            sel.get(&row.device_id)
                .is_some_and(|d| d.patches.contains_key(&key))
        })
    }

    /// Ticks or unticks exactly the patch row clicked — nothing else.
    ///
    /// The device is implied by its ticked rows: it enters the selection with the
    /// first one and leaves when the last is cleared, so a device with nothing
    /// ticked is never dispatched against. What `Apply` then does on that device
    /// is still all-or-nothing (there is no per-KB apply endpoint); the per-row
    /// detail is what lets a `kbAllowList` script receive the actual subset.
    pub(in crate::app) fn toggle_row_selection(self, row: &PatchRow, checked: bool) {
        self.actions
            .selected
            .update(|sel| util::apply_row_selection(sel, row, checked));
    }

    /// Whether every row on the current page is selected. Used for the header
    /// checkbox's checked/indeterminate state.
    pub(in crate::app) fn page_selection_state(self) -> (bool, bool) {
        let rows = self.query.page_rows.get();
        if rows.is_empty() {
            return (false, false);
        }
        let sel = self.actions.selected.get();
        // Counts ticked *rows*, not devices: with per-row selection a device can
        // be partly ticked, and the header box must read indeterminate for that.
        let selected = rows
            .iter()
            .filter(|r| {
                sel.get(&r.device_id)
                    .is_some_and(|d| d.patches.contains_key(&patch_key(r)))
            })
            .count();
        (
            selected == rows.len(),
            selected > 0 && selected < rows.len(),
        )
    }

    /// Ticks or clears every patch row on the current page. Idempotent per row, so
    /// re-running it never double-counts.
    pub(in crate::app) fn toggle_page_selection(self, checked: bool) {
        let rows = self.query.page_rows.get_untracked();
        for row in &rows {
            self.toggle_row_selection(row, checked);
        }
    }

    pub(in crate::app) fn clear_selection(self) {
        self.actions.selected.update(|s| s.clear());
    }

    /// `(devices, patch rows, offline devices)` for the action bar's running total.
    /// Cross-page selection is invisible unless it is surfaced somewhere.
    pub(in crate::app) fn selection_counts(self) -> (usize, usize, usize) {
        self.actions.selected.with(|sel| {
            (
                sel.len(),
                sel.values().map(|d| d.patches.len()).sum(),
                sel.values().filter(|d| d.offline).count(),
            )
        })
    }

    /// Per-device targets for a remediation kind, and the devices that have any.
    pub(in crate::app) fn remediation_targets(
        self,
        kind: ActionKind,
    ) -> BTreeMap<i64, Vec<String>> {
        self.actions
            .selected
            .with(|sel| util::remediation_targets(sel, kind))
    }

    /// Per-device KBs a hand-picked `kbAllowList` script would receive. Same shape as
    /// the OS remediation targets — the checkbox says "the selected KBs", and there
    /// is only one honest reading of that.
    pub(in crate::app) fn script_kb_targets(self) -> BTreeMap<i64, Vec<String>> {
        self.actions
            .selected
            .with(|sel| util::targets_by_device(sel, true))
    }
}

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

    /// Drops both selections — the patch rows and the Needs Reboot tab's devices.
    /// Every caller is a new scope (a manual query, a dropped result, a session
    /// change), and neither selection describes what is on screen after one.
    pub(in crate::app) fn clear_selection(self) {
        self.actions.selected.update(|s| s.clear());
        self.actions.device_selected.update(|s| s.clear());
    }

    /// The "Clear" link in the action bar: only the selection that bar is showing.
    pub(in crate::app) fn clear_selection_for(self, source: SelectionSource) {
        self.selection_signal(source).update(|s| s.clear());
    }

    fn selection_signal(self, source: SelectionSource) -> RwSignal<BTreeMap<i64, DeviceSelection>> {
        match source {
            SelectionSource::PatchRows => self.actions.selected,
            SelectionSource::Devices => self.actions.device_selected,
        }
    }

    /// `(devices, patch rows, offline devices)` for the action bar's running total.
    /// Cross-page selection is invisible unless it is surfaced somewhere. A
    /// device-level selection always reports zero patch rows.
    pub(in crate::app) fn selection_counts_for(
        self,
        source: SelectionSource,
    ) -> (usize, usize, usize) {
        self.selection_signal(source).with(|sel| {
            (
                sel.len(),
                sel.values().map(|d| d.patches.len()).sum(),
                sel.values().filter(|d| d.offline).count(),
            )
        })
    }

    /// Whether this device is ticked on the Needs Reboot tab.
    pub(in crate::app) fn is_device_selected(self, device_id: i64) -> bool {
        self.actions
            .device_selected
            .with(|sel| sel.contains_key(&device_id))
    }

    /// Ticks or unticks one device on the Needs Reboot tab — never any patch row.
    pub(in crate::app) fn toggle_device_selection(self, device: &DeviceSummary, checked: bool) {
        self.actions
            .device_selected
            .update(|sel| util::apply_device_selection(sel, device, checked));
    }

    /// `(all, some)` of `page` ticked, for the header checkbox.
    pub(in crate::app) fn device_page_selection_state(
        self,
        page: &[DeviceSummary],
    ) -> (bool, bool) {
        self.actions
            .device_selected
            .with(|sel| util::device_page_selection_state(sel, page))
    }

    /// Ticks or clears every device on the current page of the Needs Reboot tab.
    pub(in crate::app) fn toggle_device_page(self, page: &[DeviceSummary], checked: bool) {
        self.actions.device_selected.update(|sel| {
            for device in page {
                util::apply_device_selection(sel, device, checked);
            }
        });
    }

    /// After a silent refresh, drops the ticked devices that no longer need a
    /// reboot. The fresh list arrives whole in the summary, so this needs no fetch.
    pub(in crate::app) fn prune_device_selection_after_refresh(self) {
        if self
            .actions
            .device_selected
            .with_untracked(|s| s.is_empty())
        {
            return;
        }
        let mut removed = 0;
        self.query.result.with_untracked(|r| {
            let fresh = r.as_ref().map_or(&[][..], |r| r.reboot_devices.as_slice());
            self.actions
                .device_selected
                .update(|sel| removed = util::prune_device_level_selection(sel, fresh));
        });
        if removed > 0 {
            self.notify(Toast::ok(format!(
                "Auto-refresh: {removed} selected device(s) no longer need a reboot and were deselected"
            )));
        }
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

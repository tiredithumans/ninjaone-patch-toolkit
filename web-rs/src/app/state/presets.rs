//! Wholesale overwrites of the form and filters: applying the backend's settings
//! view (including the tenant-switch reset) and restoring a saved preset.

use super::*;

impl AppState {
    pub(in crate::app) fn apply_settings_view(self, v: SettingsView) {
        self.settings.f_instance.set(v.instance_base_url);
        self.settings
            .f_client_id
            .set(v.client_id.unwrap_or_default());
        self.settings.f_port.set(v.callback_port);
        self.settings.f_install_days.set(v.install_window_days);
        self.settings.f_sla.set(v.sla_days);
        self.settings.has_secret.set(v.has_client_secret);
        self.settings.f_auto_update.set(v.auto_check_updates);
        self.settings.f_actions.set(v.actions);
        self.filters.install_days.set(v.install_window_days);
        self.settings.presets.set(v.presets);
        // The backend dropped its cached result when the tenant changed, so whatever
        // is on screen can no longer be paged, sorted, grouped or exported — every
        // one of those re-reads that cache and would now find the miss. Clearing is
        // the honest end of the same rule `query_patches` enforces for a query that
        // spans the switch.
        if v.tenant_changed {
            self.clear_session();
            // The scope ids belong to the previous tenant's lookups. Left in place
            // they narrowed the next query to organizations that do not exist here,
            // and the chips could only name them as "(not found)".
            self.filters.org_ids.set(Vec::new());
            self.filters.loc_ids.set(Vec::new());
            self.filters.role_ids.set(Vec::new());
            // So are the lists themselves: the switch drops the grant, and until the
            // next sign-in reloads them the pickers offered the old tenant's names.
            self.lookups.orgs.set(Vec::new());
            self.lookups.locations.set(Vec::new());
            self.lookups.roles.set(Vec::new());
        }
    }

    pub(in crate::app) fn apply_preset(self, p: Preset) {
        let f = p.filter;
        // Restore the patch-query selectors only when the preset captured them, so a
        // legacy preset leaves the current Type/Status/install-window untouched.
        if let Some(pt) = p.patch_type {
            self.filters.patch_type.set(pt);
        }
        if let Some(st) = p.statuses {
            self.filters.statuses.set(st);
        }
        if let Some(d) = p.install_days {
            self.filters.install_days.set(d);
        }
        self.filters.role_ids.set(f.role_ids);
        self.filters.selected_classes.set(f.node_classes);
        self.filters.selected_severities.set(f.severities);
        self.filters
            .os_name
            .set(f.os_name_contains.unwrap_or_default());
        self.filters.search.set(f.search.unwrap_or_default());
        // Restore the release-date filter UI from the stored bounds.
        let (window, after, before) = util::detected_window_fields(
            f.detected_within_days,
            f.detected_after,
            f.detected_before,
        );
        self.filters.detected_window.set(window);
        self.filters.detected_after_date.set(after);
        self.filters.detected_before_date.set(before);
        // Load the locations for the restored org scope, then restore the saved
        // location selection — the list has to exist before the ids can be pruned
        // against it.
        self.filters.org_ids.set(f.organization_ids);
        let want_locs = f.location_ids;
        self.filters.loc_ids.set(Vec::new());
        self.lookups.locations.set(Vec::new());
        let orgs = self.filters.org_ids.get_untracked();
        if self.session.demo.get_untracked() {
            self.lookups.locations.set(demo::sample_locations(&orgs));
            self.filters.loc_ids.set(want_locs);
            self.prune_selected_locations();
            return;
        }
        spawn_local(async move {
            match api::list_locations(orgs).await {
                Ok(locs) => {
                    self.lookups.locations.set(locs);
                    self.filters.loc_ids.set(want_locs);
                    self.prune_selected_locations();
                }
                Err(e) => self.notify(Toast::err(format!("Couldn't load locations: {e}"))),
            }
        });
    }
}

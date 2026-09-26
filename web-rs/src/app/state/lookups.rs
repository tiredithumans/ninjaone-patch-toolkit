//! Scope lookups: the org/role/location/OS-type lists behind the filter pickers,
//! the organization toggle that reloads locations, and demo-mode seeding.

use super::*;

impl AppState {
    pub(in crate::app) fn load_lookups(self) {
        self.lookups.lookups_pending.set(3);
        spawn_local(async move {
            match api::list_orgs().await {
                Ok(o) => self.lookups.orgs.set(o),
                Err(e) => self.notify(Toast::err(format!("Couldn't load organizations: {e}"))),
            }
            self.lookups.lookup_done();
        });
        spawn_local(async move {
            match api::list_roles().await {
                Ok(r) => self.lookups.roles.set(r),
                Err(e) => self.notify(Toast::err(format!("Couldn't load roles: {e}"))),
            }
            self.lookups.lookup_done();
        });
        // Locations load up front too, rather than only once an organization is
        // picked. With the facets multi-select, "every organization" is a real and
        // common scope, and under it the location picker would otherwise sit
        // permanently empty and disabled — the operator could not narrow to a site
        // without first selecting its org.
        spawn_local(async move {
            match api::list_locations(Vec::new()).await {
                Ok(locs) => self.lookups.locations.set(locs),
                Err(e) => self.notify(Toast::err(format!("Couldn't load locations: {e}"))),
            }
            self.lookups.lookup_done();
        });
    }

    /// Loads the static OS-type list. It needs no auth or API call, so it runs at
    /// startup rather than waiting for sign-in like the org/role/location lookups.
    pub(in crate::app) fn load_node_classes(self) {
        spawn_local(async move {
            match api::list_node_classes().await {
                Ok(n) => self.lookups.node_classes.set(n),
                Err(e) => self.notify(Toast::err(format!("Couldn't load OS types: {e}"))),
            }
        });
    }

    /// Toggles one organization in the scope and reloads the locations available
    /// under the new selection.
    ///
    /// Locations that no longer belong to any selected organization are dropped from
    /// the selection rather than left behind: an invisible location id would go on
    /// narrowing every query with nothing on screen to explain the empty result.
    pub(in crate::app) fn toggle_org(self, org_id: i64) {
        self.filters.toggle_id(self.filters.org_ids, org_id);
        self.reload_locations();
    }

    /// Clears the organization scope — through the same reload as a toggle, so the
    /// location list goes back to every location and no longer-offered location
    /// stays selected.
    pub(in crate::app) fn clear_orgs(self) {
        self.filters.org_ids.set(Vec::new());
        self.reload_locations();
    }

    /// Reloads the location list for the current organization selection, then prunes
    /// any selected location that is no longer offered.
    pub(in crate::app) fn reload_locations(self) {
        self.load_locations(None);
    }

    /// Loads the location list for the current organization selection. Once the
    /// list exists it selects `restore`, if given, and then prunes any selected
    /// location the list does not offer — the ids can only be pruned against a list
    /// that has arrived, which is why a preset's saved ids ride in here rather than
    /// being set up front.
    pub(super) fn load_locations(self, restore: Option<Vec<i64>>) {
        let orgs = self.filters.org_ids.get_untracked();
        let apply = move |locs: Vec<Location>| {
            self.lookups.locations.set(locs);
            if let Some(ids) = restore {
                self.filters.loc_ids.set(ids);
            }
            self.prune_selected_locations();
        };
        // Demo mode resolves locations from the sample, not the backend.
        if self.session.demo.get_untracked() {
            apply(demo::sample_locations(&orgs));
            return;
        }
        spawn_local(async move {
            match api::list_locations(orgs).await {
                Ok(locs) => apply(locs),
                Err(e) => self.notify(Toast::err(format!("Couldn't load locations: {e}"))),
            }
        });
    }

    /// Drops selected location ids that the current list no longer offers.
    fn prune_selected_locations(self) {
        let available: Vec<i64> = self
            .lookups
            .locations
            .get_untracked()
            .iter()
            .map(|l| l.id)
            .collect();
        self.filters
            .loc_ids
            .update(|sel| sel.retain(|id| available.contains(id)));
    }

    /// Enters demo mode (browser/Pages) without populating results: seeds the facet
    /// dropdowns from the sample and flags `demo` so **Run query** filters the sample
    /// locally. The results stay empty ("Run a query to list patches") until the user
    /// runs a query — exactly like the real app, which lists nothing until queried.
    pub(in crate::app) fn enter_demo(self) {
        self.lookups.orgs.set(demo::sample_orgs());
        self.lookups.roles.set(demo::sample_roles());
        // Every location up front, matching the signed-in path: with no organization
        // selected the demo's location picker would otherwise be empty and disabled.
        self.lookups.locations.set(demo::sample_locations(&[]));
        self.lookups.node_classes.set(demo::sample_node_classes());
        self.session.demo.set(true);
    }
}

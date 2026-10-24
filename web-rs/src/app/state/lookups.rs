//! Scope lookups: the org/role/group/location/OS-type lists behind the filter pickers,
//! the organization toggle that reloads locations, and demo-mode seeding.

use super::*;

impl AppState {
    /// Loads the org/role/group/location lists. Each reply is dropped if the
    /// session ended while it was in flight (`clear_session` moved the epoch): it
    /// describes the previous session — often the previous tenant — and painting it
    /// would offer, and let a query be scoped to, organizations that are not there.
    /// A dropped reply does not count itself done either; `clear_session` already
    /// reset the counter, and a newer load owns it now.
    pub(in crate::app) fn load_lookups(self) {
        let epoch = self.session.epoch();
        self.lookups.lookups_pending.set(4);
        spawn_local(async move {
            let reply = api::list_orgs().await;
            if self.session.outlived(epoch) {
                return;
            }
            match reply {
                Ok(o) => self.lookups.orgs.set(o),
                Err(e) => self.notify(Toast::err(format!("Couldn't load organizations: {e}"))),
            }
            self.lookups.lookup_done();
        });
        spawn_local(async move {
            let reply = api::list_roles().await;
            if self.session.outlived(epoch) {
                return;
            }
            match reply {
                Ok(r) => self.lookups.roles.set(r),
                Err(e) => self.notify(Toast::err(format!("Couldn't load roles: {e}"))),
            }
            self.lookups.lookup_done();
        });
        spawn_local(async move {
            let reply = api::list_device_groups().await;
            if self.session.outlived(epoch) {
                return;
            }
            match reply {
                Ok(g) => self.lookups.device_groups.set(g),
                Err(e) => self.notify(Toast::err(format!("Couldn't load device groups: {e}"))),
            }
            self.lookups.lookup_done();
        });
        // The *full* location list, loaded up front rather than once an
        // organization is picked: with the facets multi-select, "every
        // organization" is a real and common scope, and under it the picker would
        // otherwise sit permanently empty. Kept whole; each org scope's list is
        // derived from it client-side (`load_locations`).
        spawn_local(async move {
            let reply = api::list_locations(Vec::new()).await;
            if self.session.outlived(epoch) {
                return;
            }
            match reply {
                Ok(locs) => {
                    self.lookups.all_locations.set(Some(locs));
                    self.load_locations(None);
                }
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

    /// Toggles one organization in the scope and narrows the location list to it.
    pub(in crate::app) fn toggle_org(self, org_id: i64) {
        self.filters.toggle_id(self.filters.org_ids, org_id);
        self.reload_locations();
    }

    /// Clears the organization scope — through the same reload as a toggle, so the
    /// location list goes back to every location.
    pub(in crate::app) fn clear_orgs(self) {
        self.filters.org_ids.set(Vec::new());
        self.reload_locations();
    }

    /// Re-derives the location list for the current organization selection and
    /// prunes any selected location it no longer offers.
    pub(in crate::app) fn reload_locations(self) {
        self.load_locations(None);
    }

    /// Derives the location list for the current organization selection from the
    /// full list, selects `restore` if given, and prunes the selection to what the
    /// list offers (`util::scope_locations`). Synchronous on purpose: a drill-down
    /// runs its query right after narrowing the org scope, and must read a location
    /// selection that has already been pruned to that org.
    pub(super) fn load_locations(self, restore: Option<Vec<i64>>) {
        let orgs = self.filters.org_ids.get_untracked();
        let selected = restore.unwrap_or_else(|| self.filters.loc_ids.get_untracked());
        let (offered, selected) = self
            .lookups
            .all_locations
            .with_untracked(|all| util::scope_locations(all.as_deref(), &orgs, selected));
        self.lookups.locations.set(offered);
        self.filters.loc_ids.set(selected);
    }

    /// Enters demo mode (browser/Pages) without populating results: seeds the facet
    /// dropdowns from the sample and flags `demo` so **Run query** filters the sample
    /// locally. The results stay empty ("Run a query to list patches") until the user
    /// runs a query — exactly like the real app, which lists nothing until queried.
    pub(in crate::app) fn enter_demo(self) {
        self.lookups.orgs.set(demo::sample_orgs());
        self.lookups.roles.set(demo::sample_roles());
        self.lookups.device_groups.set(demo::sample_device_groups());
        // Every location up front, matching the signed-in path: with no organization
        // selected the demo's location picker would otherwise be empty and disabled.
        self.lookups
            .all_locations
            .set(Some(demo::sample_locations(&[])));
        self.load_locations(None);
        self.lookups.node_classes.set(demo::sample_node_classes());
        self.session.demo.set(true);
    }
}

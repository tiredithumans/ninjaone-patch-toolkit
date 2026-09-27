//! Shareable view links: capturing the current view as a code and applying one.
//! The encoding, validation and host rule are `util::view_link`'s; this file only
//! reads and writes the signals.

use super::*;

/// The host a web-demo view is stamped with — there is no instance to name.
const DEMO_HOST: &str = "demo";

impl AppState {
    /// The instance this app is pointed at, as a view link names it.
    pub(in crate::app) fn current_view_host(self) -> String {
        if self.session.demo.get_untracked() {
            return DEMO_HOST.to_string();
        }
        let url = self
            .session
            .auth
            .with_untracked(|a| a.as_ref().map(|a| a.instance_base_url.clone()))
            .filter(|u| !u.trim().is_empty())
            .unwrap_or_else(|| self.settings.f_instance.get_untracked());
        util::instance_host(&url)
    }

    /// The live controls and view as a shareable view. The filters are the ones
    /// on screen (what a preset would save), not the last-run snapshot.
    pub(in crate::app) fn current_view(self) -> util::SharedView {
        util::SharedView {
            host: self.current_view_host(),
            filter: self.current_filter(),
            patch_type: self.filters.patch_type.get_untracked(),
            statuses: self.filters.statuses.get_untracked(),
            install_days: self.filters.install_days.get_untracked(),
            tab: self.ui.active_tab.get_untracked(),
            group_by: self.query.group_by.get_untracked(),
            sort: self.query.patches_sort.get_untracked(),
        }
    }

    /// Subscribes the caller to every signal [`current_view`](Self::current_view)
    /// reads, which are all untracked there because the Run path must not be.
    pub(in crate::app) fn track_view(self) {
        let f = self.filters;
        f.org_ids.track();
        f.loc_ids.track();
        f.role_ids.track();
        f.selected_classes.track();
        f.selected_severities.track();
        f.os_name.track();
        f.search.track();
        f.detected_window.track();
        f.detected_after_date.track();
        f.detected_before_date.track();
        f.patch_type.track();
        f.statuses.track();
        f.install_days.track();
        self.ui.active_tab.track();
        self.query.group_by.track();
        self.query.patches_sort.track();
    }

    /// Decodes pasted text (or the page fragment) and applies it — unless it was
    /// shared from another instance, in which case it is held for confirmation.
    pub(in crate::app) fn open_view_link(self, input: &str) {
        let view = match util::decode_view(input) {
            Ok(view) => view,
            Err(e) => {
                self.notify(Toast::err(e.message()));
                return;
            }
        };
        let here = self.current_view_host();
        if view.host != here {
            let from = if view.host.is_empty() {
                "an unknown instance".to_string()
            } else {
                view.host.clone()
            };
            let here = if here.is_empty() {
                "no instance yet".to_string()
            } else {
                here
            };
            self.ui.pending_view.set(Some(PendingView {
                warning: format!(
                    "This view was shared from {from}, but this app is on {here}. Its \
                     organization, location and role picks belong to that instance — any \
                     this one doesn't list are dropped."
                ),
                view,
            }));
            return;
        }
        self.apply_view(view);
    }

    /// Applies a validated view: the filters (through the preset path, which also
    /// reloads and prunes locations), the tab, the grouping and the sort, then runs.
    pub(in crate::app) fn apply_view(self, view: util::SharedView) {
        self.ui.pending_view.set(None);
        let mut filter = view.filter;
        let known_orgs: Vec<i64> = self
            .lookups
            .orgs
            .with_untracked(|o| o.iter().map(|o| o.id).collect());
        let known_roles: Vec<i64> = self
            .lookups
            .roles
            .with_untracked(|r| r.iter().map(|r| r.id).collect());
        let (orgs, dropped_orgs) = util::prune_unknown_ids(filter.organization_ids, &known_orgs);
        let (roles, dropped_roles) = util::prune_unknown_ids(filter.role_ids, &known_roles);
        filter.organization_ids = orgs;
        filter.role_ids = roles;
        let classes = self.lookups.node_classes.get_untracked();
        if !classes.is_empty() {
            filter
                .node_classes
                .retain(|c| classes.iter().any(|nc| &nc.value == c));
        }
        self.apply_preset(Preset {
            name: String::new(),
            filter,
            patch_type: Some(view.patch_type),
            statuses: Some(view.statuses),
            install_days: Some(view.install_days),
        });
        self.ui.active_tab.set(view.tab);
        if self.query.group_by.get_untracked() != view.group_by {
            // Not `set_group_by`: that fetches for the result on screen, and the
            // run below fetches for the new one.
            self.query.group_by.set(view.group_by);
            self.query.groups.set(Vec::new());
            self.query.reset_members();
        }
        self.query.sort_on_next_run.set(view.sort);
        let dropped = dropped_orgs + dropped_roles;
        self.notify(Toast::ok(if dropped > 0 {
            format!("View applied — {dropped} organization/role pick(s) not in this instance were ignored")
        } else {
            "View applied".to_string()
        }));
        self.run_query();
    }

    /// Copies the current view: the whole page URL in the web demo (its fragment
    /// is kept current), a bare code on the desktop, which has no URL to share.
    /// The code stays on screen either way, for when the clipboard refuses.
    pub(in crate::app) fn copy_view_link(self) {
        let code = util::encode_view(&self.current_view());
        let text = if self.session.web_mode.get_untracked() {
            api::replace_fragment(&format!("{}{code}", util::FRAGMENT_KEY));
            api::location_href()
        } else {
            code
        };
        self.ui.view_code.set(Some(text.clone()));
        spawn_local(async move {
            match api::copy_to_clipboard(&text).await {
                Ok(()) => self.notify(Toast::ok("View link copied")),
                Err(_) => self.notify(Toast::ok(
                    "Couldn't reach the clipboard — select the link below and copy it",
                )),
            }
        });
    }
}

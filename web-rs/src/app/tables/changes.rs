//! "Changes since the previous run": what became pending, what stopped being
//! pending and what newly failed, against the previous run of the same scope.

use super::*;

/// Which change list is open under the strip.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ChangeKind {
    NewPending,
    Resolved,
    NewlyFailed,
}

/// The strip above the Patches table: the baseline and the three counts on one
/// line, each opening its list below it, then the caveats that decide how they may
/// be read (every surface prints those; `changes_notes`). It used to be a boxed
/// panel of three stacked disclosures and two note lines above every result.
///
/// Rendered even when the query matched no rows — "everything was resolved since
/// yesterday" is exactly when the table is empty.
#[component]
pub(super) fn ChangesPanel() -> impl IntoView {
    let state = expect_context::<AppState>();
    let changes = move || {
        state
            .query
            .result
            .with(|r| r.as_ref().map(|r| r.changes.clone()))
    };
    let open = RwSignal::new(None::<ChangeKind>);

    move || {
        changes().map(|c| {
            let title = util::changes_title(&c);
            let notes = util::changes_notes(&c, util::CHANGE_LIST_LIMIT).join(" ");
            let has_baseline = c.previous_at.is_some();
            let lists = [
                (ChangeKind::NewPending, "newly pending", "changes-new", c.new_pending, c.tracks_pending, c.new_pending_items.clone()),
                (ChangeKind::Resolved, "resolved", "changes-resolved", c.resolved, c.tracks_pending, c.resolved_items.clone()),
                (ChangeKind::NewlyFailed, "newly failed", "changes-failed", c.newly_failed, c.tracks_failed, c.newly_failed_items.clone()),
            ];
            let toggles = has_baseline.then(|| {
                lists
                    .iter()
                    .map(|(kind, label, class, total, measured, _)| {
                        let (kind, label, class) = (*kind, *label, *class);
                        // An unmeasured count shows a dash, not a zero: zero would
                        // claim nothing changed when the status selection never looked.
                        let count = if *measured {
                            group_thousands(*total)
                        } else {
                            "\u{2014}".to_string()
                        };
                        let on = move || open.get() == Some(kind);
                        view! {
                            <button
                                type="button"
                                class=format!("changes-toggle {class}")
                                aria-expanded=move || on().to_string()
                                on:click=move |_| {
                                    open.update(|o| *o = if *o == Some(kind) { None } else { Some(kind) })
                                }
                            >
                                <span aria-hidden="true">{move || if on() { "▾ " } else { "▸ " }}</span>
                                <span class="changes-count">{count}</span>
                                " "
                                {label}
                            </button>
                        }
                    })
                    .collect_view()
            });
            let bodies = lists
                .into_iter()
                .map(|(kind, _, _, total, _, items)| {
                    view! {
                        <Show when=move || open.get() == Some(kind)>
                            <ChangeList total=total items=items.clone()/>
                        </Show>
                    }
                })
                .collect_view();
            view! {
                <section class="changes" aria-label="Changes since the previous run">
                    <div class="changes-strip">
                        <span class="changes-title">{title}</span>
                        {toggles}
                    </div>
                    {(!notes.is_empty()).then(|| view! { <p class="changes-note">{notes}</p> })}
                    {bodies}
                </section>
            }
        })
    }
}

/// One change list, opened from its count in the strip.
#[component]
fn ChangeList(total: usize, items: Vec<ChangeItem>) -> impl IntoView {
    let caption = util::change_list_caption(items.len(), total);
    let body = if items.is_empty() {
        view! { <p class="changes-note">"None."</p> }.into_any()
    } else {
        view! {
            <div class="table-wrap">
                <table class="changes-table">
                    <thead>
                        <tr>
                            // Spelled as the workbook's Changes sheet spells them
                            // (`changes::ChangeRow::COLUMNS`, less the Change column
                            // this list's heading already states).
                            <th scope="col">"Device"</th>
                            <th scope="col">"Patch Type"</th>
                            <th scope="col">"KB"</th>
                            <th scope="col">"Patch"</th>
                            <th scope="col">"Severity"</th>
                        </tr>
                    </thead>
                    <tbody>
                        {items
                            .into_iter()
                            .map(|i| {
                                let sev = sev_class(&i.severity);
                                view! {
                                    <tr>
                                        <td>{i.device_name}</td>
                                        <td>{i.patch_type}</td>
                                        <td>{i.kb.unwrap_or_default()}</td>
                                        <td class="patch-name"><span class="clamp-2" title=i.name.clone()>{i.name.clone()}</span></td>
                                        <td>
                                            <span class=sev>{i.severity}</span>
                                        </td>
                                    </tr>
                                }
                            })
                            .collect_view()}
                    </tbody>
                </table>
            </div>
        }
        .into_any()
    };
    view! {
        <div class="changes-list">
            {body}
            {caption.map(|c| view! { <p class="changes-note">{c}</p> })}
        </div>
    }
}

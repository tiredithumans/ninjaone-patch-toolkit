//! "Changes since the previous run": what became pending, what stopped being
//! pending and what newly failed, against the previous run of the same scope.

use super::*;

/// The panel above the Patches table. Rendered even when the query matched no rows
/// — "everything was resolved since yesterday" is exactly when the table is empty.
#[component]
pub(super) fn ChangesPanel() -> impl IntoView {
    let state = expect_context::<AppState>();
    let changes = move || {
        state
            .query
            .result
            .with(|r| r.as_ref().map(|r| r.changes.clone()))
    };

    move || {
        changes().map(|c| {
            let title = util::changes_title(&c);
            let notes = util::changes_notes(&c, util::CHANGE_LIST_LIMIT);
            let lists = c.previous_at.is_some().then(|| {
                view! {
                    <div class="changes-lists">
                        <ChangeList
                            label="Newly pending"
                            class="changes-new"
                            total=c.new_pending
                            measured=c.tracks_pending
                            items=c.new_pending_items.clone()
                        />
                        <ChangeList
                            label="Resolved"
                            class="changes-resolved"
                            total=c.resolved
                            measured=c.tracks_pending
                            items=c.resolved_items.clone()
                        />
                        <ChangeList
                            label="Newly failed"
                            class="changes-failed"
                            total=c.newly_failed
                            measured=c.tracks_failed
                            items=c.newly_failed_items.clone()
                        />
                    </div>
                }
            });
            view! {
                <section class="changes" aria-label="Changes since the previous run">
                    <h3 class="changes-title">{title}</h3>
                    {lists}
                    {notes
                        .into_iter()
                        .map(|n| view! { <p class="changes-note">{n}</p> })
                        .collect_view()}
                </section>
            }
        })
    }
}

/// One expandable count. An unmeasured count shows a dash, not a zero: zero would
/// claim nothing changed when the status selection never looked.
#[component]
fn ChangeList(
    label: &'static str,
    class: &'static str,
    total: usize,
    measured: bool,
    items: Vec<ChangeItem>,
) -> impl IntoView {
    let caption = util::change_list_caption(items.len(), total);
    let count = if measured {
        group_thousands(total)
    } else {
        "\u{2014}".to_string()
    };
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
                                        <td class="patch-name">{i.name}</td>
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
        <details class=format!("changes-group {class}")>
            <summary>
                <span class="changes-count">{count}</span>
                " "
                {label}
            </summary>
            {body}
            {caption.map(|c| view! { <p class="changes-note">{c}</p> })}
        </details>
    }
}

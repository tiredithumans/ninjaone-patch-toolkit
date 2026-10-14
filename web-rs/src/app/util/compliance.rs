//! The Compliance tab's own sentences: what its figures mean and what they leave
//! out. Each mirrors a backend `rows::` function, so the in-app wording and the
//! exports' read the same.

use crate::types::FirstSeenTracking;

/// How many organizations [`uncounted_orgs_note`] names before "and N more".
pub(crate) const UNCOUNTED_ORGS_LIMIT: usize = 10;

/// The line under the organization table naming the organizations that have no row
/// in it — every device offline or not patchable — or `None` when there are none.
/// Without it such an organization silently vanishes from the table.
pub(crate) fn uncounted_orgs_note(orgs: &[String]) -> Option<String> {
    match orgs {
        [] => None,
        [one] => Some(format!(
            "{one} has no online, patchable device in scope, so it is not in the table."
        )),
        _ => {
            let shown = orgs[..orgs.len().min(UNCOUNTED_ORGS_LIMIT)].join(", ");
            let more = orgs.len().saturating_sub(UNCOUNTED_ORGS_LIMIT);
            let tail = if more > 0 {
                format!(", and {more} more")
            } else {
                String::new()
            };
            Some(format!(
                "{} organizations have no online, patchable device in scope, so they are not \
                 in the table: {shown}{tail}.",
                orgs.len()
            ))
        }
    }
}

/// What "first seen" means in this result, or `None` when no patch family is in it:
/// the earlier of NinjaOne's time and this app's own record, since when that record
/// has been kept, and how many pending records it dated. Mirrors
/// `rows::FirstSeenTracking::sentence`.
pub(crate) fn first_seen_note(t: &FirstSeenTracking) -> Option<String> {
    let since = t.since.as_deref()?;
    let dated = match t.from_app_record {
        0 => "every pending record here is dated by NinjaOne's time".to_string(),
        n => format!(
            "{n} of {} pending records are dated by this app's record, because NinjaOne's \
             time on them is later",
            t.pending_records
        ),
    };
    Some(format!(
        "First seen is the earlier of NinjaOne's record time and when this app first saw the \
         patch pending (this app's record began {since}); {dated}."
    ))
}

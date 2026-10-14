//! The Compliance tab's own sentences: what its figures mean and what they leave
//! out. Each mirrors a backend `rows::` function, so the in-app wording and the
//! exports' read the same.

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

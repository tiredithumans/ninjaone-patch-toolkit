//! Shareable view links: the current filters, results tab, grouping and sort as a
//! compact, versioned, URL-safe code.
//!
//! What goes in is exactly what a preset stores (the `FilterParams` shape, patch
//! type, statuses, install window) plus the view — never the selection, never a
//! credential, never a client id. The instance *host* rides along so a view opened
//! against another tenant is flagged instead of quietly narrowing to organization
//! ids that mean something else there.
//!
//! A code is `v1.` + base64url (no padding) of a small JSON object. The version sits
//! outside the payload so an unknown one is refused before anything is parsed.
//! Decoding never trusts the payload: unknown statuses, severities, tabs, groupings
//! and sort keys are dropped, numbers are clamped to what the controls accept, and
//! text is trimmed and capped.

use serde::{Deserialize, Serialize};

use super::super::{SEVERITY_OPTIONS, STATUS_OPTIONS, Tab};
use super::TAB_ORDER;
use crate::types::{FilterParams, GroupBy, RowSort, RowSortKey};

/// The version prefix this build writes and the only one it reads.
const PREFIX: &str = "v1.";

/// Where the code sits in a web-demo URL: `…/#view=<code>`.
pub(crate) const FRAGMENT_KEY: &str = "view=";

/// Longer than any code this build writes by a wide margin; anything past it is
/// not a view link and is refused before decoding.
const MAX_CODE_LEN: usize = 8_192;
/// Per text facet (search, OS name).
const MAX_TEXT: usize = 200;
/// Per id facet — far above any real scope, low enough to bound the work.
const MAX_IDS: usize = 1_000;
/// The install window and First-seen bounds the controls accept.
const MAX_DAYS: i64 = 3_650;
/// The First-seen presets the filter offers; any other relative window would be
/// ignored by `filter_params`, so it is dropped here rather than carried silently.
const DETECTED_WINDOWS: [i64; 4] = [1, 7, 30, 90];
/// 9999-12-31T23:59:59Z — past it an epoch bound is not a date the picker shows.
const MAX_EPOCH: i64 = 253_402_300_799;
const PATCH_TYPES: [&str; 3] = ["ALL", "OS", "SOFTWARE"];

/// A decoded, validated view.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SharedView {
    /// Instance host the view was taken on (`us2.ninjarmm.com`, or `demo`).
    pub host: String,
    pub filter: FilterParams,
    pub patch_type: String,
    pub statuses: Vec<String>,
    pub install_days: i64,
    pub tab: Tab,
    pub group_by: Option<GroupBy>,
    pub sort: Option<RowSort>,
}

/// The wire form. Short keys keep the code short; every field is defaulted so a
/// missing one is "not set" rather than a failed decode.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Wire {
    h: String,
    f: FilterParams,
    t: String,
    s: Vec<String>,
    d: i64,
    tab: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    g: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    o: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    desc: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ViewLinkError {
    Empty,
    TooLong,
    UnknownVersion,
    Malformed,
}

impl ViewLinkError {
    pub(crate) fn message(&self) -> &'static str {
        match self {
            Self::Empty => "Paste a view link or code first",
            Self::TooLong => "That is too long to be a view link",
            Self::UnknownVersion => {
                "That view link is from a different version of the app and can't be read here"
            }
            Self::Malformed => "That isn't a readable view link",
        }
    }
}

/// The host part of an instance URL, lowercased: `https://US2.ninjarmm.com/` →
/// `us2.ninjarmm.com`.
pub(crate) fn instance_host(url: &str) -> String {
    let url = url.trim();
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

/// Stable id of a results tab in a view code.
pub(crate) fn tab_id(tab: Tab) -> &'static str {
    match tab {
        Tab::Patches => "patches",
        Tab::Compliance => "compliance",
        Tab::Reboot => "reboot",
        Tab::Failures => "failures",
        Tab::Trend => "trend",
        Tab::Jobs => "jobs",
    }
}

fn tab_from_id(id: &str) -> Option<Tab> {
    TAB_ORDER.iter().copied().find(|t| tab_id(*t) == id)
}

/// A serde unit-variant name (`"DEVICE"`, `"severity"`), as the backend spells it.
fn variant_name<T: Serialize>(v: &T) -> Option<String> {
    match serde_json::to_value(v).ok()? {
        serde_json::Value::String(s) => Some(s),
        _ => None,
    }
}

fn from_variant_name<T: for<'de> Deserialize<'de>>(name: &str) -> Option<T> {
    serde_json::from_value(serde_json::Value::String(name.to_string())).ok()
}

pub(crate) fn encode_view(view: &SharedView) -> String {
    let wire = Wire {
        h: view.host.clone(),
        f: view.filter.clone(),
        t: view.patch_type.clone(),
        s: view.statuses.clone(),
        d: view.install_days,
        tab: tab_id(view.tab).to_string(),
        g: view.group_by.as_ref().and_then(variant_name),
        o: view.sort.and_then(|s| variant_name(&s.key)),
        desc: view.sort.is_some_and(|s| s.desc),
    };
    let json = serde_json::to_vec(&wire).unwrap_or_default();
    format!("{PREFIX}{}", base64url_encode(&json))
}

/// The code inside whatever was pasted: a bare code, `view=<code>`, or a whole
/// demo URL carrying `#view=<code>`.
pub(crate) fn view_code_from(input: &str) -> &str {
    let input = input.trim();
    let code = match input.find(FRAGMENT_KEY) {
        Some(at) => &input[at + FRAGMENT_KEY.len()..],
        None => input,
    };
    code.split('&').next().unwrap_or_default().trim()
}

pub(crate) fn decode_view(input: &str) -> Result<SharedView, ViewLinkError> {
    let code = view_code_from(input);
    if code.is_empty() {
        return Err(ViewLinkError::Empty);
    }
    if code.len() > MAX_CODE_LEN {
        return Err(ViewLinkError::TooLong);
    }
    let Some(payload) = code.strip_prefix(PREFIX) else {
        // `v7.…` is a view link from some other build; anything else is noise.
        let versioned = code.split_once('.').is_some_and(|(v, _)| {
            v.len() > 1 && v.starts_with('v') && v[1..].bytes().all(|b| b.is_ascii_digit())
        });
        return Err(if versioned {
            ViewLinkError::UnknownVersion
        } else {
            ViewLinkError::Malformed
        });
    };
    let bytes = base64url_decode(payload).ok_or(ViewLinkError::Malformed)?;
    let wire: Wire = serde_json::from_slice(&bytes).map_err(|_| ViewLinkError::Malformed)?;
    Ok(validate(wire))
}

fn validate(w: Wire) -> SharedView {
    let f = w.f;
    let filter = FilterParams {
        organization_ids: clean_ids(f.organization_ids),
        location_ids: clean_ids(f.location_ids),
        role_ids: clean_ids(f.role_ids),
        node_classes: clean_list(f.node_classes, |_| true),
        os_name_contains: f.os_name_contains.and_then(clean_text),
        search: f.search.and_then(clean_text),
        severities: clean_list(f.severities, |s| {
            SEVERITY_OPTIONS.iter().any(|(raw, _)| *raw == s)
        }),
        detected_within_days: f
            .detected_within_days
            .filter(|d| DETECTED_WINDOWS.contains(d)),
        detected_after: f.detected_after.filter(|t| (0..=MAX_EPOCH).contains(t)),
        detected_before: f.detected_before.filter(|t| (0..=MAX_EPOCH).contains(t)),
        installed_after: f.installed_after.filter(|t| (0..=MAX_EPOCH).contains(t)),
        installed_before: f.installed_before.filter(|t| (0..=MAX_EPOCH).contains(t)),
    };
    let mut statuses = clean_list(w.s, |s| STATUS_OPTIONS.contains(&s));
    if statuses.is_empty() {
        // A query with no status is refused outright; fall back to the app's own
        // default rather than hand the operator a Run that cannot run.
        statuses.push("PENDING".to_string());
    }
    let patch_type = if PATCH_TYPES.contains(&w.t.as_str()) {
        w.t
    } else {
        "ALL".to_string()
    };
    let install_days = if w.d == 0 { 30 } else { w.d.clamp(1, MAX_DAYS) };
    let sort =
        w.o.as_deref()
            .and_then(from_variant_name::<RowSortKey>)
            .map(|key| RowSort { key, desc: w.desc });
    SharedView {
        host: w.h.trim().to_ascii_lowercase(),
        filter,
        patch_type,
        statuses,
        install_days,
        tab: tab_from_id(&w.tab).unwrap_or(Tab::Patches),
        group_by: w.g.as_deref().and_then(from_variant_name::<GroupBy>),
        sort,
    }
}

fn clean_ids(mut ids: Vec<i64>) -> Vec<i64> {
    ids.retain(|id| *id > 0);
    ids.sort_unstable();
    ids.dedup();
    ids.truncate(MAX_IDS);
    ids
}

fn clean_list(values: Vec<String>, keep: impl Fn(&str) -> bool) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for v in values {
        let v = v.trim();
        if !v.is_empty() && v.len() <= MAX_TEXT && keep(v) && !out.iter().any(|o| o == v) {
            out.push(v.to_string());
        }
        if out.len() >= MAX_IDS {
            break;
        }
    }
    out
}

fn clean_text(s: String) -> Option<String> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    Some(s.chars().take(MAX_TEXT).collect())
}

/// Drops the ids `known` does not list, returning what is left and how many went.
/// An empty `known` means the list has not loaded — nothing is dropped, because
/// "not loaded yet" is not "does not exist".
pub(crate) fn prune_unknown_ids(ids: Vec<i64>, known: &[i64]) -> (Vec<i64>, usize) {
    if known.is_empty() {
        return (ids, 0);
    }
    let before = ids.len();
    let kept: Vec<i64> = ids.into_iter().filter(|id| known.contains(id)).collect();
    let dropped = before - kept.len();
    (kept, dropped)
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// RFC 4648 §5 base64url without padding. Hand-rolled: ~30 lines is cheaper than a
/// new dependency in the wasm bundle.
fn base64url_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        let chars = chunk.len() + 1;
        for i in 0..chars {
            out.push(B64[((n >> (18 - 6 * i)) & 0x3f) as usize] as char);
        }
    }
    out
}

fn base64url_decode(s: &str) -> Option<Vec<u8>> {
    let s = s.trim_end_matches('=');
    if s.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut acc: u32 = 0;
    let mut bits = 0;
    for c in s.bytes() {
        let v = B64.iter().position(|&b| b == c)? as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> SharedView {
        SharedView {
            host: "us2.ninjarmm.com".to_string(),
            filter: FilterParams {
                organization_ids: vec![3, 7],
                location_ids: vec![11],
                role_ids: vec![],
                node_classes: vec!["WINDOWS_SERVER".to_string()],
                os_name_contains: Some("2019".to_string()),
                search: Some("KB5040434".to_string()),
                severities: vec!["CRITICAL".to_string(), "IMPORTANT".to_string()],
                detected_within_days: Some(30),
                detected_after: None,
                detected_before: None,
                // A custom install-history range travels with the view.
                installed_after: Some(1_772_323_200),
                installed_before: Some(1_774_915_200),
            },
            patch_type: "OS".to_string(),
            statuses: vec!["PENDING".to_string(), "FAILED".to_string()],
            install_days: 14,
            tab: Tab::Failures,
            group_by: Some(GroupBy::Device),
            sort: Some(RowSort {
                key: RowSortKey::Severity,
                desc: true,
            }),
        }
    }

    fn wire_code(json: &str) -> String {
        format!("{PREFIX}{}", base64url_encode(json.as_bytes()))
    }

    #[test]
    fn base64url_round_trips_every_length() {
        for len in 0..40 {
            let bytes: Vec<u8> = (0..len).map(|i| (i * 37 + 250) as u8).collect();
            let enc = base64url_encode(&bytes);
            assert!(enc.bytes().all(|b| B64.contains(&b)), "{enc}");
            assert_eq!(base64url_decode(&enc).as_deref(), Some(&bytes[..]));
        }
        assert_eq!(base64url_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64url_encode(b"\xfb\xff"), "-_8");
        assert_eq!(base64url_decode("Zm9vYg=="), Some(b"foob".to_vec()));
        assert_eq!(base64url_decode("a"), None);
        assert_eq!(base64url_decode("ab+c"), None);
    }

    #[test]
    fn a_view_round_trips() {
        let view = sample();
        let code = encode_view(&view);
        assert!(code.starts_with("v1."));
        assert!(
            code.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)),
            "not URL-safe: {code}"
        );
        assert_eq!(decode_view(&code), Ok(view));
    }

    #[test]
    fn a_flat_unsorted_view_round_trips() {
        let view = SharedView {
            group_by: None,
            sort: None,
            tab: Tab::Patches,
            ..sample()
        };
        assert_eq!(decode_view(&encode_view(&view)), Ok(view));
    }

    #[test]
    fn accepts_a_whole_url_or_a_fragment() {
        let code = encode_view(&sample());
        for pasted in [
            format!("https://example.github.io/app/#view={code}"),
            format!("#view={code}"),
            format!("view={code}"),
            format!("  {code}\n"),
        ] {
            assert_eq!(decode_view(&pasted), Ok(sample()), "{pasted}");
        }
    }

    #[test]
    fn refuses_an_unknown_version_and_noise() {
        assert_eq!(decode_view(""), Err(ViewLinkError::Empty));
        assert_eq!(decode_view("   "), Err(ViewLinkError::Empty));
        assert_eq!(decode_view("v2.eyJ9"), Err(ViewLinkError::UnknownVersion));
        assert_eq!(decode_view("v10.abc"), Err(ViewLinkError::UnknownVersion));
        assert_eq!(decode_view("hello world"), Err(ViewLinkError::Malformed));
        assert_eq!(decode_view("v1.!!!"), Err(ViewLinkError::Malformed));
        assert_eq!(
            decode_view(&wire_code("[1,2,3]")),
            Err(ViewLinkError::Malformed)
        );
        let long = format!("v1.{}", "A".repeat(MAX_CODE_LEN));
        assert_eq!(decode_view(&long), Err(ViewLinkError::TooLong));
    }

    #[test]
    fn unknown_ids_are_ignored_and_values_clamped() {
        let json = r#"{
            "h": " US2.NinjaRMM.com ",
            "f": {
                "organizationIds": [5, -1, 0, 5, 2],
                "locationIds": [],
                "roleIds": [],
                "nodeClasses": ["", "WINDOWS_SERVER", "WINDOWS_SERVER"],
                "osNameContains": "   ",
                "search": "  kb1  ",
                "severities": ["CRITICAL", "APOCALYPTIC"],
                "detectedWithinDays": 45,
                "detectedAfter": -5,
                "detectedBefore": 1700000000
            },
            "t": "FIRMWARE",
            "s": ["PENDING", "SNOOZED"],
            "d": 99999,
            "tab": "sla-heatmap",
            "g": "PRODUCT_FAMILY",
            "o": "notAColumn",
            "desc": true,
            "futureField": {"anything": 1}
        }"#;
        let v = decode_view(&wire_code(json)).expect("decodes");
        assert_eq!(v.host, "us2.ninjarmm.com");
        assert_eq!(v.filter.organization_ids, vec![2, 5]);
        assert_eq!(v.filter.node_classes, vec!["WINDOWS_SERVER"]);
        assert_eq!(v.filter.os_name_contains, None);
        assert_eq!(v.filter.search.as_deref(), Some("kb1"));
        assert_eq!(v.filter.severities, vec!["CRITICAL"]);
        assert_eq!(v.filter.detected_within_days, None);
        assert_eq!(v.filter.detected_after, None);
        assert_eq!(v.filter.detected_before, Some(1_700_000_000));
        assert_eq!(v.patch_type, "ALL");
        assert_eq!(v.statuses, vec!["PENDING"]);
        assert_eq!(v.install_days, MAX_DAYS);
        assert_eq!(v.tab, Tab::Patches);
        assert_eq!(v.group_by, None);
        assert_eq!(v.sort, None);
    }

    #[test]
    fn a_minimal_payload_gets_the_app_defaults() {
        let v = decode_view(&wire_code("{}")).expect("decodes");
        assert_eq!(v.statuses, vec!["PENDING"]);
        assert_eq!(v.patch_type, "ALL");
        assert_eq!(v.install_days, 30);
        assert_eq!(v.filter, FilterParams::default());
    }

    #[test]
    fn long_text_is_capped() {
        let json = format!(
            r#"{{"f":{{"organizationIds":[],"locationIds":[],"roleIds":[],"nodeClasses":[],"search":"{}"}}}}"#,
            "x".repeat(5_000)
        );
        let v = decode_view(&wire_code(&json)).expect("decodes");
        assert_eq!(v.filter.search.map(|s| s.len()), Some(MAX_TEXT));
    }

    #[test]
    fn the_code_carries_no_selection_or_credentials() {
        let code = encode_view(&sample());
        let json = String::from_utf8(base64url_decode(&code[PREFIX.len()..]).unwrap()).unwrap();
        for forbidden in ["secret", "client", "token", "selected", "deviceTargets"] {
            assert!(!json.contains(forbidden), "{forbidden} in {json}");
        }
    }

    #[test]
    fn hosts_are_normalised() {
        assert_eq!(
            instance_host("https://US2.ninjarmm.com/"),
            "us2.ninjarmm.com"
        );
        assert_eq!(instance_host("eu.ninjarmm.com"), "eu.ninjarmm.com");
        assert_eq!(
            instance_host(" http://127.0.0.1:8080/x?y "),
            "127.0.0.1:8080"
        );
        assert_eq!(instance_host(""), "");
    }

    #[test]
    fn every_tab_has_a_distinct_id_that_reads_back() {
        for tab in TAB_ORDER {
            assert_eq!(tab_from_id(tab_id(tab)), Some(tab));
        }
    }

    #[test]
    fn unknown_ids_are_pruned_only_against_a_loaded_list() {
        assert_eq!(
            prune_unknown_ids(vec![1, 2, 3], &[2, 3, 4]),
            (vec![2, 3], 1)
        );
        assert_eq!(prune_unknown_ids(vec![1, 2], &[]), (vec![1, 2], 0));
    }
}

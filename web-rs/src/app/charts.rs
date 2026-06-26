//! Inline-SVG dashboard charts for the Results panel: per-org compliance bars, a
//! pending-patch severity breakdown, and a pending-patch age histogram. The shapes
//! are hand-drawn `<svg>` (no JS charting lib — WASM-friendly) sized off the compact
//! aggregates the backend ships in `QueryResult` (it never sends every detail row).
//!
//! The numeric geometry lives in small pure helpers (`bar_width_px`,
//! `bar_height_px`, `sum_severity`, `severity_segments`) that host-test via
//! `just web-test`, the same split as `app::util`; the `view!`-bearing components
//! are compiled by `web-check`/`web-clippy` but not unit-tested.

use leptos::prelude::*;

use super::AppState;
use crate::types::{OrgSeverity, SeverityCounts};

/// Severity bands in most-to-least-urgent order, paired with the CSS class that
/// fills their chart segment / legend swatch.
const SEV_BANDS: [(&str, &str); 6] = [
    ("Critical", "seg-critical"),
    ("Important", "seg-important"),
    ("Moderate", "seg-moderate"),
    ("Low", "seg-low"),
    ("Optional", "seg-optional"),
    ("Unknown", "seg-unknown"),
];

// Fixed SVG coordinate spaces; CSS scales them down responsively (max-width:100%).
const COMPLIANCE_VW: f64 = 480.0;
const COMPLIANCE_ROW_H: i32 = 42;
const SEV_TRACK: f64 = 600.0;
const AGE_FULL_H: f64 = 120.0;
const AGE_BAR_W: i32 = 64;
const AGE_GAP: i32 = 22;

/// Linear bar length: `value/max` of `track` px, guarding a zero/empty max and
/// clamping so an out-of-range value never overshoots the track.
fn bar_width_px(value: f64, max: f64, track: f64) -> f64 {
    if max <= 0.0 {
        0.0
    } else {
        (value / max).clamp(0.0, 1.0) * track
    }
}

/// Vertical bar height: `count/max` of `full` px, guarding an empty histogram.
fn bar_height_px(count: usize, max: usize, full: f64) -> f64 {
    if max == 0 {
        0.0
    } else {
        count as f64 / max as f64 * full
    }
}

/// Compliance bar fill class: green at/above 95%, amber at/above 80%, else red.
fn compliance_fill_class(pct: f64) -> &'static str {
    if pct >= 95.0 {
        "bar-good"
    } else if pct >= 80.0 {
        "bar-warn"
    } else {
        "bar-bad"
    }
}

/// Sums the per-org pending-severity breakdowns into one fleet total.
fn sum_severity(by_org: &[OrgSeverity]) -> SeverityCounts {
    let mut t = SeverityCounts::default();
    for o in by_org {
        t.critical += o.counts.critical;
        t.important += o.counts.important;
        t.moderate += o.counts.moderate;
        t.low += o.counts.low;
        t.optional += o.counts.optional;
        t.unknown += o.counts.unknown;
    }
    t
}

fn sev_count(c: &SeverityCounts, label: &str) -> usize {
    match label {
        "Critical" => c.critical,
        "Important" => c.important,
        "Moderate" => c.moderate,
        "Low" => c.low,
        "Optional" => c.optional,
        _ => c.unknown,
    }
}

/// One positioned segment of the stacked severity bar (its `x`/`width` are pixels in
/// the `track`-wide coordinate space). Zero-count bands are skipped.
#[derive(Clone, Debug, PartialEq)]
struct Segment {
    class: &'static str,
    label: &'static str,
    count: usize,
    x: f64,
    width: f64,
}

/// Lays the non-empty severity bands out left-to-right across `track` pixels,
/// proportional to each band's share of the total.
fn severity_segments(c: &SeverityCounts, track: f64) -> Vec<Segment> {
    let total = c.critical + c.important + c.moderate + c.low + c.optional + c.unknown;
    if total == 0 {
        return Vec::new();
    }
    let mut x = 0.0;
    let mut out = Vec::new();
    for (label, class) in SEV_BANDS {
        let count = sev_count(c, label);
        if count == 0 {
            continue;
        }
        let width = count as f64 / total as f64 * track;
        out.push(Segment {
            class,
            label,
            count,
            x,
            width,
        });
        x += width;
    }
    out
}

/// The dashboard tab: compliance / severity / age charts stacked in cards. Empty
/// until a query has run (same gate as the other result tabs).
#[component]
pub(crate) fn Dashboard() -> impl IntoView {
    let state = expect_context::<AppState>();
    let has_result = move || state.result.with(|r| r.is_some());
    view! {
        <Show
            when=has_result
            fallback=|| view! { <p class="empty">"Run a query to see the dashboard."</p> }
        >
            <div class="charts">
                <div class="chart-card">
                    <h3 class="chart-title">"Compliance by organization"</h3>
                    <ComplianceBars/>
                </div>
                <div class="chart-card">
                    <h3 class="chart-title">"Pending patches by severity"</h3>
                    <SeverityBreakdown/>
                </div>
                <div class="chart-card">
                    <h3 class="chart-title">"Pending patch age"</h3>
                    <AgeHistogram/>
                </div>
            </div>
        </Show>
    }
}

#[component]
fn ComplianceBars() -> impl IntoView {
    let state = expect_context::<AppState>();
    let buckets = move || {
        state
            .result
            .with(|r| r.as_ref().map(|r| r.compliance.clone()).unwrap_or_default())
    };
    view! {
        {move || {
            let bks = buckets();
            if bks.is_empty() {
                return view! { <p class="empty">"No compliance data."</p> }.into_any();
            }
            let h = bks.len() as i32 * COMPLIANCE_ROW_H;
            view! {
                <svg
                    class="chart"
                    role="img"
                    width=COMPLIANCE_VW.to_string()
                    height=h.to_string()
                    viewBox=format!("0 0 {COMPLIANCE_VW:.0} {h}")
                >
                    {bks
                        .into_iter()
                        .enumerate()
                        .map(|(i, b)| {
                            let y0 = i as i32 * COMPLIANCE_ROW_H;
                            let label_y = (y0 + 14).to_string();
                            let bar_y = (y0 + 20).to_string();
                            let pct = b.compliance_pct;
                            let w = format!("{:.1}", bar_width_px(pct, 100.0, COMPLIANCE_VW));
                            let fill = compliance_fill_class(pct);
                            view! {
                                <g>
                                    <text x="0" y=label_y.clone() class="chart-lbl">
                                        {b.organization}
                                    </text>
                                    <text
                                        x=COMPLIANCE_VW.to_string()
                                        y=label_y
                                        text-anchor="end"
                                        class="chart-val"
                                    >
                                        {format!("{pct:.0}%")}
                                    </text>
                                    <rect
                                        class="chart-track"
                                        x="0"
                                        y=bar_y.clone()
                                        width=COMPLIANCE_VW.to_string()
                                        height="10"
                                        rx="5"
                                    ></rect>
                                    <rect class=fill x="0" y=bar_y width=w height="10" rx="5"></rect>
                                </g>
                            }
                        })
                        .collect_view()}
                </svg>
            }
                .into_any()
        }}
    }
}

#[component]
fn SeverityBreakdown() -> impl IntoView {
    let state = expect_context::<AppState>();
    let total = move || {
        state.result.with(|r| {
            r.as_ref()
                .map(|r| sum_severity(&r.severity_by_org))
                .unwrap_or_default()
        })
    };
    view! {
        {move || {
            let segs = severity_segments(&total(), SEV_TRACK);
            if segs.is_empty() {
                return view! { <p class="empty">"No pending patches."</p> }.into_any();
            }
            view! {
                <div>
                    <svg
                        class="chart"
                        role="img"
                        width=SEV_TRACK.to_string()
                        height="24"
                        viewBox=format!("0 0 {SEV_TRACK:.0} 24")
                        preserveAspectRatio="none"
                    >
                        {segs
                            .clone()
                            .into_iter()
                            .map(|s| {
                                view! {
                                    <rect
                                        class=s.class
                                        x=format!("{:.1}", s.x)
                                        y="0"
                                        width=format!("{:.1}", s.width)
                                        height="24"
                                    ></rect>
                                }
                            })
                            .collect_view()}
                    </svg>
                    <ul class="chart-legend">
                        {segs
                            .into_iter()
                            .map(|s| {
                                view! {
                                    <li>
                                        <span class=format!("chart-swatch {}", s.class)></span>
                                        {format!("{}: {}", s.label, s.count)}
                                    </li>
                                }
                            })
                            .collect_view()}
                    </ul>
                </div>
            }
                .into_any()
        }}
    }
}

#[component]
fn AgeHistogram() -> impl IntoView {
    let state = expect_context::<AppState>();
    let buckets = move || {
        state.result.with(|r| {
            r.as_ref()
                .map(|r| r.age_buckets.clone())
                .unwrap_or_default()
        })
    };
    view! {
        {move || {
            let bks = buckets();
            let max = bks.iter().map(|b| b.count).max().unwrap_or(0);
            if max == 0 {
                return view! { <p class="empty">"No pending patches."</p> }.into_any();
            }
            let top_pad = 16;
            let label_h = 22;
            let width = AGE_GAP + bks.len() as i32 * (AGE_BAR_W + AGE_GAP);
            let height = AGE_FULL_H as i32 + top_pad + label_h;
            let baseline = top_pad as f64 + AGE_FULL_H;
            view! {
                <svg
                    class="chart"
                    role="img"
                    width=width.to_string()
                    height=height.to_string()
                    viewBox=format!("0 0 {width} {height}")
                >
                    {bks
                        .into_iter()
                        .enumerate()
                        .map(|(i, b)| {
                            let h = bar_height_px(b.count, max, AGE_FULL_H);
                            let x = AGE_GAP + i as i32 * (AGE_BAR_W + AGE_GAP);
                            let y = baseline - h;
                            let cx = (x + AGE_BAR_W / 2).to_string();
                            view! {
                                <g>
                                    <rect
                                        class="bar-good"
                                        x=x.to_string()
                                        y=format!("{y:.1}")
                                        width=AGE_BAR_W.to_string()
                                        height=format!("{h:.1}")
                                        rx="3"
                                    ></rect>
                                    <text
                                        x=cx.clone()
                                        y=format!("{:.1}", y - 4.0)
                                        text-anchor="middle"
                                        class="chart-val"
                                    >
                                        {b.count}
                                    </text>
                                    <text
                                        x=cx
                                        y=(baseline as i32 + 16).to_string()
                                        text-anchor="middle"
                                        class="chart-lbl"
                                    >
                                        {b.label}
                                    </text>
                                </g>
                            }
                        })
                        .collect_view()}
                </svg>
            }
                .into_any()
        }}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bar_width_px_guards_zero_max_and_clamps() {
        assert_eq!(bar_width_px(50.0, 0.0, 100.0), 0.0);
        assert_eq!(bar_width_px(50.0, 100.0, 200.0), 100.0);
        assert_eq!(
            bar_width_px(150.0, 100.0, 200.0),
            200.0,
            "over-range clamps"
        );
    }

    #[test]
    fn bar_height_px_scales_against_max() {
        assert_eq!(bar_height_px(0, 0, 100.0), 0.0, "empty histogram → no bar");
        assert_eq!(bar_height_px(5, 10, 100.0), 50.0);
        assert_eq!(bar_height_px(10, 10, 80.0), 80.0);
    }

    #[test]
    fn compliance_fill_class_thresholds() {
        assert_eq!(compliance_fill_class(96.0), "bar-good");
        assert_eq!(compliance_fill_class(85.0), "bar-warn");
        assert_eq!(compliance_fill_class(50.0), "bar-bad");
    }

    #[test]
    fn sum_severity_adds_each_band_across_orgs() {
        let by_org = vec![
            OrgSeverity {
                organization: "A".into(),
                counts: SeverityCounts {
                    critical: 2,
                    important: 1,
                    ..Default::default()
                },
            },
            OrgSeverity {
                organization: "B".into(),
                counts: SeverityCounts {
                    critical: 3,
                    low: 4,
                    ..Default::default()
                },
            },
        ];
        let t = sum_severity(&by_org);
        assert_eq!(t.critical, 5);
        assert_eq!(t.important, 1);
        assert_eq!(t.low, 4);
    }

    #[test]
    fn severity_segments_skip_zero_bands_and_tile_the_track() {
        let counts = SeverityCounts {
            critical: 1,
            important: 3,
            ..Default::default()
        };
        let segs = severity_segments(&counts, 400.0);
        assert_eq!(segs.len(), 2, "only the two non-zero bands");
        assert_eq!(segs[0].label, "Critical");
        assert_eq!(segs[0].x, 0.0, "first segment starts at the origin");
        assert!((segs[0].width - 100.0).abs() < 1e-9, "1/4 of 400");
        assert!(
            (segs[1].x - 100.0).abs() < 1e-9,
            "second starts where first ends"
        );
        let spanned = segs.last().map(|s| s.x + s.width).unwrap();
        assert!(
            (spanned - 400.0).abs() < 1e-9,
            "segments tile the full track"
        );
    }

    #[test]
    fn severity_segments_empty_when_no_pending() {
        assert!(severity_segments(&SeverityCounts::default(), 400.0).is_empty());
    }
}

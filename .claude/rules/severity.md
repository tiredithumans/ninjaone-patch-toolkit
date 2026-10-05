---
paths:
  - "src-tauri/src/model.rs"
  - "src-tauri/src/rows/rollups.rs"
  - "web-rs/src/app/charts.rs"
  - "web-rs/src/app.rs"
---

# Severity

- **Two vocabularies on one field; `Security`/`Recommended` are their own variants ranked below
  `Important`; unmapped → `Unknown`.** Adding a value touches ten sites — follow the checklist
  in the design note. Enumerate bands via `SeverityCounts::BANDS` / `charts::SEV_BANDS`, never a
  label match — `total_severity_is_the_sum_of_its_bands`, `severity_css_defines_every_band`.
  → `docs/design/severity.md`
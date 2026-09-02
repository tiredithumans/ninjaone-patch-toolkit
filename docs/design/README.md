# Design notes

[AGENTS.md](../../AGENTS.md) is the **contract**: one short bullet per rule, naming the file to
read and the test that enforces it. These notes are the **rationale** behind those rules — why
each invariant exists, what went wrong before it did, and the trade-offs that were weighed. Read
the note for a domain before changing it; update the note when the reasoning changes and keep the
contract line in AGENTS.md short.

| Note | Covers |
|---|---|
| [query-cache.md](./query-cache.md) | The `query_patches` result cache, tenant/generation gating, paging/grouping/sorting memos, whole-fleet prefetch + client-side scoping, epoch-gated single-flight fetches, `force_refresh`. |
| [concurrency.md](./concurrency.md) | `spawn_blocking` for CPU-bound and blocking work, `AppState` lock discipline, the result-handle rule. |
| [auth.md](./auth.md) | Secrets discipline, PKCE + lazy refresh, conditional scope and the management grant, keyring ordering, the callback listener, single-flight refresh and `invalid_grant`. |
| [actions.md](./actions.md) | The write path: the two apply mechanisms, per-device targets, `ReplaySafety::ActOnce`, payload-bound confirm tokens, the single dispatch surface, `plan()` guardrails, invalidation, the job store and poller, resolving jobs from `/activities`. |
| [api-client.md](./api-client.md) | `NinjaApiClient` retry policy and pagination, single-parse page bodies, forward-progress rules, the reqwest feature list. |
| [filter.md](./filter.md) | Client-side device scope vs the server-side install `df` vs client-side row facets, `PreparedFilter`, multi-select ids, the `df` grammar. |
| [compliance.md](./compliance.md) | What a compliance number means: the rollup population, the scope note, both clocks, `QueryScope` provenance, status routing and pushdown, `is_pending`, percentage capping, interned row strings, the absent release date. |
| [severity.md](./severity.md) | NinjaOne's two severity vocabularies and the full checklist for adding a value. |
| [frontend.md](./frontend.md) | Tauri command shape, IPC arg shape, WASM gating, CSP, auto-update, Leptos reactivity and focus traps, demo mode, and the "no logic in a component body" testing rule. |
| [ci.md](./ci.md) | The CI-only gates: coverage, audit/deny, CodeQL, manifest versions, screenshot tooling, the release verify job. |

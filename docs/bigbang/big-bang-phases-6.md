# Epic 6: Dual-workspace HTMX web UI and control plane APIs

Epic 6 builds the embedded web interface (`askama`, `htmx`, `uPlot`, `rust-embed`) and JSON control-plane APIs. It covers the Personal Workspace (`/app/*`), the Admin Console (`/admin/*`), and the read-only Session Chat Preview UI. It corresponds to Milestone `M5` (`SRDD Sections 4, 6.10, 6.11, 8.2, 14`).

## 6.1 Sub-phase 6.1: Askama + HTMX shell, embedded assets, CSP headers, and role-aware navigation

This sub-phase sets up the Askama templates and embeds vendored `htmx 2.x` and `uPlot` assets via `rust-embed` (`UI-3`). It also enforces `Content-Security-Policy`, `x-csrf-token` checks (`SEC-5`), and role-aware navigation (`UI-1`, `UI-4`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 6.11`, `Section 8.2`, `Section 14.2`, `Section 14.3`; Requirements `UI-1`, `UI-3`, `UI-4`, `SEC-5`, `SEC-9` |
| Crates touched | `crates/admin::ui`, `crates/admin::assets`, `crates/admin::csrf` |
| TDD step (`Red -> Green`) | Write integration tests (`tests/security_csrf_and_headers.rs`, `#[doc = "Spec: UI-1, UI-3, UI-4, SEC-5, SEC-9"]`) asserting: (1) every HTML response includes `Content-Security-Policy: default-src 'self'...`, `X-Frame-Options: DENY`, and `X-Content-Type-Options: nosniff`, (2) zero HTML templates reference external `http://` or `https://` script/style/font URLs (`UI-3`), (3) any `POST`/`PUT`/`PATCH`/`DELETE` without a valid `x-csrf-token` returns HTTP 403 (`SEC-5`), and (4) enabling `content_traceability_mode` renders the global traceability status banner (`UI-4`). |
| Atomic commits | 1. `feat(admin/6.1): embed vendored HTMX and uPlot assets and enforce CSP and CSRF middleware [Spec: UI-3, SEC-5, SEC-9]` 2. `feat(admin/6.1): implement role-aware layout shell and content traceability status banner [Spec: UI-1, UI-4]` |
| Risks and resolutions | **Risk:** Inline `onclick` handlers or inline `<script>` blocks break `script-src 'self'`. **Resolution:** Use standard declarative `hx-get`, `hx-post`, `hx-target`, and `hx-swap` attributes plus a single external `/static/app.js` file served from `rust-embed`. |

## 6.2 Sub-phase 6.2: Standard user Personal Workspace (`/app/*` and `/api/user/*`)

This sub-phase implements the self-service `/api/user/*` endpoints and `/app/*` screens for standard `user` accounts (`SRDD Section 14.2.1`). It covers Personal Providers & Keys, Outgoing and Incoming Shares, the Personal Routing Matrix, and Client Keys.

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 4.2`, `Section 6.2`, `Section 6.3`, `Section 6.4`, `Section 6.5`, `Section 14.2.1`, `Section 14.3`; Requirements `USR-2`..`USR-4`, `PRV-1`..`PRV-6`, `SHR-1`..`SHR-6`, `CLK-1`..`CLK-5`, `UI-1`, `UI-2` |
| Crates touched | `crates/admin::user_api`, `crates/admin::templates::app` |
| TDD step (`Red -> Green`) | Write RBAC and UI integration tests (`tests/integration_rbac_isolation.rs`, `#[doc = "Spec: USR-2, USR-3, USR-4, UI-1, UI-2"]`) testing every `/api/user/*` endpoint as Standard User Alice and Standard User Bob: (1) Alice can create a personal provider, add a key, test `/models` and `/usage`, set `user_route_preferences` priority overrides, share the key with Bob, and generate an `llr_...` client key, (2) Bob sees the shared key in `/api/user/shares/incoming` with health and remaining cap but never sees `secret_last4` or Alice's raw `/usage` JSON (`USR-4`, `SEC-18`), and (3) cross-tenant IDOR requests (`PATCH /api/user/upstream-keys/{alice_key_id}` by Bob) return HTTP 404. |
| Atomic commits | 1. `feat(admin/6.2): implement /api/user/* endpoints and HTMX views for personal providers, keys, and /usage modal [Spec: USR-2, USR-3, USR-4, PRV-1, PRV-6, UI-2]` 2. `feat(admin/6.2): implement /api/user/shares, personal routing matrix, and client key screens [Spec: SHR-1, SHR-6, CLK-1, CLK-3, UI-1]` |
| Risks and resolutions | **Risk:** A user loses their newly generated `llr_...` client key if they navigate away accidentally. **Resolution:** Render the one-time token inside a modal that stays open until the user clicks "I have copied this key" (`SRDD Section 14.3`). |

## 6.3 Sub-phase 6.3: Admin Console (`/admin/*` and `/admin/api/*`)

This sub-phase implements the `/admin/api/*` endpoints and `/admin/*` screens for instance administration (`SRDD Section 14.2.2`). It covers Global Providers, the Model Catalog, the Price Book, Users, Content Traceability settings, and the Audit Log.

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 6.1`..`6.3`, `Section 6.6`, `Section 6.10`, `Section 6.11`, `Section 14.2.2`, `Section 14.3`; Requirements `AUTH-4`, `USR-5`, `MDL-3`, `MDL-4`, `TRC-1`, `UI-1`, `AUD-1` |
| Crates touched | `crates/admin::admin_api`, `crates/admin::templates::admin` |
| TDD step (`Red -> Green`) | Write integration tests (`tests/e2e_dashboard_htmx.rs`, `#[doc = "Spec: AUTH-4, USR-5, MDL-3, MDL-4, TRC-1, UI-1, AUD-1"]`) verifying that every `/admin/*` and `/admin/api/*` route returns HTTP 403 for `Role::User`, and that `Role::Admin` can manage global providers, force-expose models, update price-book tiers/schedules, set per-user global budgets, toggle content traceability mode, and filter the audit log. |
| Atomic commits | 1. `feat(admin/6.3): implement /admin/api/* endpoints and screens for global providers, model catalog, and price book [Spec: MDL-3, MDL-4, UI-1]` 2. `feat(admin/6.3): implement user management, traceability settings, and audit log screens [Spec: AUTH-4, USR-5, TRC-1, AUD-1]` |
| Risks and resolutions | **Risk:** Editing global model exposure or route priorities changes the projected WRR session shares in the Routing Matrix UI. **Resolution:** Reuse the exact `crates/core::wrr::projected_shares` function in the template handler so UI percentages always match router behavior. |

## 6.4 Sub-phase 6.4: Overview dashboard, dual `/usage` gauges, Active Sessions, and Usage Explorer

This sub-phase implements the Overview Dashboard with `uPlot` charts and side-by-side local and upstream `/usage` window gauges (`SRDD Section 14.3`). It also implements the Active Sessions pin table and the Usage Explorer with CSV and JSON export.

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 6.9`, `Section 6.11`, `Section 14.3`; Requirements `USG-1`..`USG-8`, `UI-1`, `UI-2` |
| Crates touched | `crates/admin::dashboard`, `crates/admin::sessions`, `crates/admin::usage_explorer` |
| TDD step (`Red -> Green`) | Write integration tests (`tests/e2e_dashboard_htmx.rs`, `#[doc = "Spec: USG-4, USG-6, UI-1, UI-2"]`) asserting that: (1) standard users see only their own client keys' usage and pins, while admins can group by `user`, `client_key`, `upstream_key`, `provider`, `provider_scope`, and `model`, (2) upstream key cards display both local estimated spend and reconciled source provider `/usage` percentages (`5h`, `7d`, `1mo`), and (3) moving an active pin in the Sessions UI routes the very next turn of that session to the newly selected upstream key. |
| Atomic commits | 1. `feat(admin/6.4): implement Overview Dashboard with uPlot charts and dual local/upstream /usage gauges [Spec: UI-1, UI-2]` 2. `feat(admin/6.4): implement Active Sessions pin manager and Usage Explorer with CSV/JSON export [Spec: UI-1]` |
| Risks and resolutions | **Risk:** Range queries over 30 days of raw `usage_events` could be slow in the dashboard. **Resolution:** Route all time-series charts and summary cards to `usage_rollups_hourly` (which is pre-aggregated in O(1) per hour), querying raw `usage_events` only for the paginated event table and CSV export. |

## 6.5 Sub-phase 6.5: Read-only Session Chat Preview UI (`TRC-5`, `TRC-6`, `UI-4`)

This sub-phase builds the client-side (dashboard) read-only Session Chat Preview drawer and full-page view (`/app/sessions/{session_hash}/chat` and `/admin/sessions/{session_hash}/chat`), rendering the reconstructed multi-turn conversation timeline (`SRDD Section 6.10, Section 12.6, Section 14.3`).

| Attribute | Details |
| --- | --- |
| SRDD references | `Section 6.10`, `Section 8.2`, `Section 12.6`, `Section 14.3`; Requirements `TRC-5`, `TRC-6`, `TRC-8`, `UI-4`, `SEC-19` |
| Crates touched | `crates/admin::chat_preview_ui`, `crates/admin::templates::chat_preview` |
| TDD step (`Red -> Green`) | Write end-to-end HTMX rendering and security tests (`tests/e2e_dashboard_htmx.rs`, `#[doc = "Spec: TRC-5, TRC-6, TRC-8, UI-4, SEC-19"]`) verifying that clicking **Preview Chat** on a traced session renders: (1) collapsible `<details>` blocks for system instructions, tool schemas, extended thinking/reasoning traces, and formatted tool-call JSON arguments/results, (2) sanitized Markdown and formatted code blocks for assistant responses (`ammonia`), (3) per-turn telemetry chips (`turn_index`, timestamp, model, upstream key, TTFB, latency, tokens, cache hit rate, cost), and (4) a raw request/response JSON drawer per turn. |
| Atomic commits | 1. `feat(admin/6.5): implement read-only Session Chat Preview UI and raw turn JSON inspector [Spec: TRC-5, TRC-6, TRC-8, UI-4, SEC-19]` |
| Risks and resolutions | **Risk:** Rendering large tool outputs (such as a 2,000-line file read by Claude Code or opencode) clutters the chat timeline. **Resolution:** Collapse tool results and reasoning blocks taller than 12 lines inside expandable `<details>` containers by default, showing a one-line summary (`Tool Result: Read 2,048 lines, 64.2 KiB`). |

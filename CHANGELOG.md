# Changelog

All notable changes to this project are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- **`app.js` split into twelve domain modules, and the MNT-3 ceiling now gates static assets
  (ISSUE-0013, ISSUE-0056).** `crates/admin/static/app.js` was 1,649 lines — 49 over the 1,600
  ceiling — and `all_rust_files_are_under_1600_lines` could not see it because its collector
  accepted only `.rs`. A gate that names a number and never checks it is worse than no gate:
  the green tick reads as verification and nobody re-measures. It is now twelve modules loaded
  by twelve `<script>` tags, largest **449 lines** (`app-core.js`, which holds the HTMX config,
  shared helpers, the tab router, and every delegated listener), with `APP_JS_MODULES` as the
  single ordered table and a test asserting the templates list the same modules in the same
  order. The files deliberately share one global scope rather than each being an IIFE: 53 of 54
  functions are invoked *by name* from the delegated listeners, so wrapping them would have
  meant rewriting all 53 call sites — a rewrite rather than the extraction. `/static/app.js`
  still serves the whole application as a compile-time `concat!` of the twelve bodies. The move
  is byte-exact: comparing the multiset of non-blank source lines before and after shows **zero
  lost or altered**, and the 24 `innerHTML` sites, 6 listeners and 54 functions each appear
  exactly once. New gate `all_static_assets_are_under_1600_lines` extends the same walker (and
  the same `MNT3_MAX_LINES` constant) to `.js`, `.css`, and `.html` under `static/` and
  `templates/`, excluding the three vendored minified bundles by a pinned list — they are 0–2
  lines but ~95 KB, so a line ceiling says nothing about them and a byte ceiling would be the
  right gate for that class. Since the repo has no JS harness, one executable Node check
  (`tests/js/app-modules-smoke.js`, wired into `cargo test`) loads the modules in template order
  in a shared `vm` context, asserts all 53 listener-reachable functions are globals, and fires
  synthetic clicks to confirm the dispatcher still routes. It passes identically against the
  pre-split file, which is the evidence that behaviour did not change.

### Fixed

- **`PATCH /admin/api/models/{id}` accepts its documented fields, validates them, and answers
  truthfully (ISSUE-0170).** The handler was eleven lines with four defects. It accepted only
  `exposed` — `distribution` and `routing_strategy` could not be sent at all, because
  `PatchModelRequest` had one field and serde drops unknown keys silently. It discarded the SQL
  result, so a PATCH naming a `model_pk` that does not exist returned `200 OK` having changed
  nothing. It validated nothing, so a bad enum value surfaced as a `500` from the schema's CHECK
  constraint instead of a `422`. And it wrote a bare `UPDATE` on the reader-facing pool. Now:
  both fields are optional members of the request; `distribution` and `routing_strategy` are
  validated first and rejected with `422` listing the accepted values (the
  `default_routing_scope`/`ui_locale` house pattern), with `routing_strategy` reusing
  `RoutingStrategy`'s own `FromStr` error so the six valid names live in one place; zero rows
  affected returns `404`; and the whole thing runs through
  `execute_atomic_mutation(vec![ConfigDomain::Models])`, so the row and the audit entry share one
  transaction and a refusal cannot half-commit. The partial update is `COALESCE` per column plus
  a `CASE` for `routing_strategy`, which has a third state: absent (keep), `""` (clear to `NULL`
  so the model inherits `default_routing_strategy`), or a value. `null` in JSON cannot express
  the clear because it deserialises the same as an absent field, so it is ignored and the empty
  string is the explicit sentinel — documented on the struct and pinned by a test. **Two of the
  issue's requested fields are declined rather than implemented:** `headroom_scaling` is the
  instance-wide runtime setting `headroom_scaling_enabled`, not a per-model property, and
  `max_context_tokens` must stay upstream-discovered — the real column is `context_window`
  (migration `0002`), whose own comment says NULL means the upstream never advertised a length
  and the proxy *"must omit the field rather than fabricate a capability value (audit
  F-031)"*. A test asserts a `context_window` key in the PATCH body is ignored, so the field
  cannot quietly become writable later. **Also documented, from running it:** the issue's claim
  that bypassing `execute_atomic_mutation` loses the audit row and the snapshot publish is
  **false** — `control_mutation_barrier` does both for every successful mutation, which a mutant
  replacing the handler with a bare `UPDATE` confirmed. Those two assertions are therefore
  regression guards; what the atomic path uniquely buys is rollback, now pinned separately.
- **`control_plane_logs` is swept in production, and the sweep is bounded (ISSUE-0230).** The table
  recording every control-plane HTTP request grew without bound: the live maintenance tick swept
  eleven tables and omitted this one, while `run_retention_pass` — the function written to trim it —
  had no production caller and was exercised only by its own tests. Two defects, fixed together
  because either alone is a regression: an unbounded sweep that runs, or a bound on a sweep that
  never runs. `sweep_expired_control_plane_logs` now takes a chunk limit like every other sweeper in
  the module (`REL-3`: *"a slow or locked SQLite writer must never block or fail an inference
  request"*), ordered `ts ASC` so a backlog drains oldest-first rather than starving the rows furthest
  past their window. The maintenance tick's retention block was extracted into
  `run_maintenance_retention_sweep` so it is reachable from a test — its previous home, a closure
  inside `tokio::spawn` inside `run_serve`, is why the omission survived. The window follows
  `usage_events_retention_days`, which `AUD-2` (*"retained for 30 days"*) and `14.l` name explicitly;
  the uncalled `run_retention_pass` used 90 days for this table, a third divergence. That function is
  now deleted, after a table-by-table comparison showed `control_plane_logs` was the only sweep it
  uniquely performed (its snapshot entry was an unbounded duplicate at 7 days of a table the tick
  already sweeps at 90). Its two tests, which asserted the behaviour of a function nothing called,
  were re-pointed at the function production actually runs.
- **The state-bound pass-through path uses the provider's billing plan (ISSUE-0130).**
  `forward_state_bound_response_op` attributed usage on two paths and they disagreed: the
  decode-failure path called `provider_billing(&snapshot, &key.provider_id)` (cycle 16's fix for
  #156) while the pass-through path wrote `plan_multiplier_bps: 10_000` and
  `billing_coverage_mode: "pay_as_you_go"` — the *defaults*. So one handler reported a subscription
  plan on a corrupt body and pay-as-you-go on a normal upstream error, which is worse than a plain
  hardcode because a reader of the fixed branch would conclude the handler honours provider billing.
  The lookup now happens once, immediately after the upstream key is in hand and before either
  terminal branch, so the two rows cannot drift apart again. `list_cost_micro_usd: 0` is kept and
  documented: a bodyless pass-through has no token cost, but the *plan* still matters because
  `apply_effective_upstream_cost` consults the mode and the ledger groups by it.
- **The usage-by-model breakdown emits a real provider badge (ISSUE-0131).** The overview emitted
  `"badge": "[LLR]"` for every model — the *custom/internal* mark from `14.g.2` — so the field said
  "house model" about OpenRouter-served models too. `14.g.4` types the projection as
  `provider_badge_kind: &'static str // "go" | "cc" | "od" | "or" | "llr"`, and those five are
  exactly `ProviderKind`'s five variants, so the badge is derived (`ProviderKind::badge_kind()`)
  rather than invented. **The aggregation rule the issue did not raise:** the breakdown groups by
  `canonical_id`, but one model can be served by several providers in one window — the header's
  "`<N>` models · `<M>` providers" counts exactly that — so a row needs a rule, not a lookup. It is
  "the provider that served the most of this model's requests", ties broken on `provider_id`, via a
  correlated subquery over the same filtered set.
- **The overview reports the configured retention figures (ISSUE-0132).** `max_usage_events_rows`
  was the literal `50_000_000`, and `retention_days` was derived from
  `traceability_config.retention_days` — the *traceability* window, default 14 — rather than
  `usage_events_retention_days`, default 30. The second was the consequential one: `A6` reads
  `daily_requests * retention_days * 750 B * 1.3` for raw `usage_events`, and the sweeper prunes
  those rows at `usage_events_retention_days`, so both the reported window and the
  `projected_disk_mb` figure derived from it described a policy nothing was enforcing — in the one
  card an operator sizes a volume from.
- **Both log streams report the retention window actually in force (ISSUE-0133).**
  `get_logs_internal` emitted `"retention_days": 30` at two sites while the policy lives in
  `RetentionPolicy.usage_events_retention_days`. Both now change together — fixing one leaves the
  two streams disagreeing about the same policy. The `inference` stream reads `usage_events`, which
  the live maintenance loop sweeps at `usage_events_retention_days`. The `api` stream reads
  `control_plane_logs`, which **nothing prunes in the live path** (the maintenance loop does not
  touch it, and `run_retention_pass` — which does, at a fixed 90 days — has no production caller),
  so it now reports `"retention_days": null` with `"retention_scope": "control_plane_logs"` rather
  than claiming a window nothing enforces. The sweeper's `.max(31)` clamps the *rollup* cutoff, not
  this table, so it is not the number this response should carry either.
- **No invented model id on an unresolvable response link (ISSUE-0129).** `state_bound.rs` resolved
  the ledger's model with `snapshot.models.get(&link.model_pk).map_or_else(|| "gpt-4o".to_string(),
  ..)` — `gpt-4o` chosen for no reason beyond being the first plausible string, added by cycle 16's
  fix for #156. A ledger row naming a model that did not serve the request is wrong data, the failure
  cycle 10 established the rule against. It is now a `502` with an actionable message, mirroring the
  unresolvable-`upstream_key_id` and unresolvable-provider lookups immediately above it. **The branch
  is dead code today** — `hard_route_checks` refuses a link whose model has left the snapshot with
  `403` before attribution runs, which mutation confirmed (restoring the fallback leaves the new test
  green). The guard is defence in depth against a snapshot/model disagreement, which is reachable via
  a partially restored database or the snapshot loader skipping a `model_pk` that fails
  `ModelPk::parse`.
- **The metric label allowlist actually gates emission (ISSUE-0128).** Thirteen call sites did
  `let _ = validate_metric_labels(&labels);`, so the closed-allowlist check was consulted and its
  answer discarded — a new call site could introduce an unbounded-cardinality label
  (`("user_id", …)`) and nothing would notice. One shared `labels_ok()` now does the gate:
  `debug_assert!` so a test build catches a bad key at the first call, and the sample is **dropped**
  in release rather than emitted unvalidated. `debug_assert!` rather than `panic!` because a metrics
  path must never take the process down. The key guard and `TOKEN_REASON_ALLOWLIST`'s value guard
  remain separate — a legal key set can carry an illegal `reason` value. A source-inspection test
  (`include_str!`) asserts no site goes back to discarding the result, so a fourteenth call site
  cannot silently reintroduce the defect.
- **A zero `instance_id` derived the wrong encryption key (ISSUE-0229).** `instance_id` is the
  **HKDF salt**, not AAD — `store/src/crypto.rs:303` builds every sealer subkey from
  `Hkdf::new(Some(instance_id), master_key)`. Three readers substituted `[0u8; 16]` when the id was
  unreachable, so a seal under the fallback produced bytes a real sealer can never open: lost
  upstream credentials or locked-out TOTP accounts, silently, because the seal itself succeeded.
  `reloader::load_instance_id` was the worst of the three — it swallowed both the query error
  (`.ok()`) and a length mismatch (`try_into().ok()`) and fed a zero salt into the initial snapshot,
  which every sealer built from that snapshot then inherited. All three now fail closed, through a
  single `load_sealer`/`load_instance_id` accessor that uses `fetch_one` and validates the length.
  The schema already forbids the state they tolerated (`instance_id BLOB NOT NULL CHECK
  (length(instance_id) = 16)`, `0001_initial.sql:11`), and `load_or_initialize` already propagated —
  so these sites were bypassing a guarantee the codebase enforces, not handling a legal one.
- **One canonical session-hash fallback (ISSUE-0137).** The product had **two** secret-less
  fallbacks for the same concept, and they disagreed: `orchestrator::session::session_hash` used a
  domain-separated SHA-256 scoped by `client_key_id`, while `usage_passthrough` used
  `compute_session_hash(&[0u8; 32], …)` scoped by `owner_user_id`. Same session, two subsystems,
  two different hashes — pinning and usage attribution could not agree on which session a request
  belonged to. The zero-key HMAC was also worse than the plain derivation beside it, because it
  *looked* keyed while providing no secrecy. Both pass-through call sites now route through the
  orchestrator's helper, and the empty-key affinity token
  (`session_hmac_secret.unwrap_or(b"")`) — a third instance of the same pattern — goes through one
  extracted `derive_salted_affinity_token`. **The keyed path is byte-identical to before**, so no
  stored pin is orphaned on upgrade. The `client_key_id`/`owner_user_id` scoping asymmetry is kept
  deliberately: changing which identity a hash is scoped by would re-key every deployment.
- **The audit HMAC key is never derived from the master key (ISSUE-0138).** Two sites —
  `execute_atomic_mutation` and the trace-view audit write — fell back to `sha256_hex(master_key)`
  when no instance key was configured. That breaks the guarantee `ADR-11` exists to make: rotating
  `LLMRoller_MASTER_KEY` must not invalidate the audit chain, and a master-key-derived audit key
  makes every previously written row stop verifying *silently* after a rotation. `resolve_audit_hmac_key`
  now returns the instance key or an error, and both call sites skip the write rather than invent a
  substitute. A missing audit row is visible and recoverable; a chain that looks valid for new rows
  and invalid for old ones is discovered during an incident.
- **The log detail drawer projects and renders every `14.m` field (ISSUE-0174).**
  `get_log_detail_internal` uses `SELECT *`, so every inspector column was already in the row and
  the hand-written projection discarded seven of them: `client_location`, `termination`,
  `incomplete`, `cost_source`, `pin_outcome`, `failovers`, `error_code`. All are now returned.
  Enum-valued columns keep their **stored vocabulary** (`termination`, `cost_source`,
  `pin_outcome` are CHECK-constrained; remapping them to display labels at the API boundary would
  break any consumer comparing against the column's own values), and `incomplete` is a JSON boolean
  rather than `0`/`1`.
- **`request_headers_json` becomes a name → value grid, and a corrupt blob is not emptied.** The
  blob was already in the JSON as a raw string; `14.m` section 6 wants a grid, so it is now shaped
  into `{pairs, parse_error, raw}`. A valid blob becomes an ordered array of pairs (JSON objects are
  unordered, and a user scanning headers expects capture order). A **corrupt** blob keeps its raw
  text with `parse_error: true` instead of collapsing to `{}` — emptying it would render "no headers
  recorded" for a value that exists and is damaged, the silent-empty failure mode cycles 10–16 kept
  fixing. Absent and corrupt remain distinct states.
- **The trace link is resolved only when a trace exists.** `trace_records.usage_event_id` is the PK
  referencing `usage_events(id)` and its `request_id` is `UNIQUE`, so the link is the same single
  lookup the existing `has_trace` flag already did. `trace_href` is `null` when no trace row exists:
  traces are opt-in (`ADR-9`) so most rows have none, and a dead link in an inspector is worse than
  an honest `null`.
- **`partials.rs` was split rather than grown.** The drawer renderer moved to
  `api/log_detail_drawer.rs` (370 lines) when `partials.rs` crossed the 1600-line `MNT-3` ceiling at
  1,634, leaving it at 1,278. The drawer is the only renderer that interpolates attacker-influenced
  text (allow-listed request header values), so isolating it makes the escaping auditable in one
  file. Every name and value in the grid is escaped.
- **CSV export honours every filter and reports truncation (ISSUE-0143).** `export_logs_csv_internal`
  issued a bare `LIMIT 5000` on both branches, so an operator who asked for "last week's errors on
  model X" received up to 5,000 unfiltered rows — in a file that leaves the system, where a wrong
  screen gets noticed but a wrong export gets acted on elsewhere. The inference branch now filters on
  `model`, `client_key_id`, `service_account_id`, `status`, and `from`/`to`; the API branch on
  `method`, `status`, and `from`/`to` (it reads `control_plane_logs`, which has none of the
  inference columns). The 5,000-row cap stays, but one row beyond it is fetched so truncation is
  detected exactly, and a capped export ends with a `# TRUNCATED: …` line — silently truncating a
  correctly-filtered export is the same class of defect as ignoring the filter.
- **The admin audit log is interrogable (ISSUE-0144).** `get_admin_audit_log_handler` took no query
  parameters at all and always returned the newest 100 rows, so "every admin action on shares last
  week" was unanswerable through the API. It now binds `action`, `target_type`, `from`/`to`,
  `cursor`, and `limit`, and returns `ip_addr` (nullable plain `TEXT`, not packed). The cursor is a
  plain `seq < ?`, which is all a `seq DESC` chain needs. The **user** audit handler was already
  correct — filters, `ip_addr`, and cursor included.
- **The usage ledger honours `initiator` (ISSUE-0142).** `LedgerQuery::initiator` was declared with
  `#[allow(dead_code)]` and never bound, so the ledger answered the unfiltered question regardless.
  It now uses cycle 23's settled three-way union from `14.n.2` — `client_key_id`, `owner_user_id`,
  `service_account_id` — applied directly to `usage_events`, which carries all three, so no join.
- **A placeholder-count test closes the class of bug that bit cycle 23.** That cycle shipped a
  four-placeholder `initiator` clause bound with three values; sqlx binds positionally, so it
  compiled and failed at runtime. `every_clause_declares_the_number_of_binds_its_binder_supplies`
  counts `?` in each clause and counts the `.bind(` calls each binder macro actually expands to,
  asserting equality with no database needed.
- **The four usage endpoints honour the filters they accept (ISSUE-0175).** `UsageFilterParams`
  deserialised `provider_id`, `model`, and `initiator`, and `summary`/`timeseries`/`breakdown`/
  `events` bound only `since`/`until` — so a filtered request returned the unfiltered number with no
  signal. The predicate now lives in one place (`api/usage_filters.rs`) and is spliced into all four
  by `rollup_query!` / `event_query!`, with `bind_usage_filters!` as the only binder; four
  hand-written `WHERE` fragments is precisely how `provider_id` came to be used in zero queries. The
  rollup flavour resolves the service-account half of the initiator union through `client_keys`,
  since `usage_rollups_hourly` has no `service_account_id`. All four also gained `cache_write_tokens`,
  `reasoning_tokens`, and a `total_tokens` computed with the canonical four-class definition, and
  the events listing gained the `since`/`until` it never had.
- **`initiator` matches any identity kind, per `14.n.2`.** The Initiator Filter Pill lists "all
  users, service accounts, and individual client keys" in one dropdown, so the predicate is a union
  over `client_key_id`, the owning `owner_user_id`, and `service_account_id` rather than a single
  column: filtering by user X returns everything X did, by key K returns only K.
- **The admin log projection sums every token class and stops hiding charged rejections
  (ISSUE-0173).** `get_logs_internal` computed `total_tokens` as `input_uncached + output`, dropping
  `cache_read`, `cache_write`, and `reasoning`, while the query already selected all five columns.
  The Usage Ledger reported 650 for an event the Logs Explorer reported as 150 — two screens, one
  request, two numbers. Both projections now use the storage layer's definition,
  `input_uncached + cache_read + cache_write + output`, which is also what
  `calculate_cost_micro_usd` prices and what the overview KPI query sums. **`reasoning` is excluded
  deliberately**: it is a diagnostic subset of `output` (`12. SRDD:133-134`), so adding it would
  double-count.
- **Non-200 rows now show the tokens and cost they actually consumed.** The `if status == 200`
  guards on `tokens_formatted` and `cost_formatted` hid exactly the rows cycles 10/11 established
  carry real `cost_micro_usd` — a charged timeout, an embedded-200 rejection. They are replaced by
  a zero-vs-absent distinction: the token columns are nullable `INTEGER`s, so `NULL` ("the provider
  never reported it") renders `—` while a real `0` on a rejected row renders as zero. `status_label`
  also now reports a 5xx as `Failed` rather than `Rejected`, because an upstream fault is not a
  decision about the caller's request.
- **The usage projections read the `reasoning` token class (ISSUE-0176).** The `TokenUsage` literal
  in `get_usage_event_breakdown_internal` hardcoded `reasoning: 0`; `get_usage_ledger_internal`'s
  aggregate and row queries omitted `reasoning_tokens` from their `SELECT` entirely. Both now read
  the column. It is surfaced as `reasoning_tokens` beside the total and deliberately **not** as a
  `line_items.reasoning` entry, since reasoning is a subset of `output` and a fifth priced line
  would imply a charge that does not exist.
- **Share approval is kind-checked and reaches the data plane (ISSUE-0191).** `SHR-3` requires
  `admin_approved_at` only for **custom** providers — `is_share_eligible` gates on
  `provider_kind == Custom` — so approving a built-in provider's share set a column nothing read:
  an approval that granted nothing. `approve_provider_share_tx` now joins `providers` and returns
  a new `ShareError::NotCustom`, and the handler maps it to `400`. The handler also now goes
  through `execute_atomic_mutation(vec![ConfigDomain::Shares])` instead of opening its own
  transaction, so the approval reaches the snapshot: previously the data plane kept believing the
  share was unapproved, the same class as cycle 18's `ISSUE-0182`. The audit row was already being
  written by the `control_mutation_barrier` middleware, so the cost of the raw transaction was the
  publish, not the trail. The pre-existing `revoked_at IS NULL` guard is unchanged.
- **`execute_atomic_mutation`'s no-writer fallback no longer swallows the closure's error.** It
  returned `503 writer_busy` for every refusal on that path, discarding the parked
  `Result<T, MutationResponseError>` that carried the meaningful status. Surfaced while adding the
  share-approval kind check, whose `400` was arriving as `503`.
- **Lowering trace retention now actually purges (ISSUE-0194).** The handler counted the rows a
  purge would affect — to fill `affected_rows` in the `409` confirmation body — and then only
  rewrote the settings row: zero `DELETE`s executed, so records older than the new window survived
  indefinitely. The periodic sweep cannot cover this, because it deletes on `expires_at`, which was
  stamped from the retention in force when each record was written. On a reduction with
  `confirm_purge: true` the handler now deletes the expired records and then collects blocks with
  zero remaining references, reusing the existing batched `sweep_orphaned_trace_blocks` (an indexed
  `NOT EXISTS` against the `trace_record_blocks` join table, not a JSON walk). Widening the window
  still deletes nothing.
- **The admin cookies now follow the request's actual scheme (ISSUE-0188).** The login CSRF
  cookie, the session cookie and the logout clear-cookie all set `Secure` — and used the
  `__Host-` name prefix — unless `--dev-insecure-cookies` was passed. Because this server has
  no TLS listener (it binds a plain `TcpListener`; TLS is always terminated upstream, and
  `docs/srdd/22`'s Caddyfile forwards `X-Forwarded-Proto`), that meant a plaintext deployment
  received cookies a browser silently discards: **login was impossible** over plain http,
  including on the documented loopback admin listener. `X-Forwarded-Proto` was never
  consulted, contrary to what the issue reported — the defect was the mirror image of the
  issue's premise.
  A new `cookie_policy_for` resolves `Secure` **and** the `__Host-` prefix together, since the
  prefix is a browser-enforced contract that requires `Secure`: `Secure` iff a *trusted* peer
  (same CIDR matching as `extract_client_ip`, now factored into one `peer_is_trusted_proxy`)
  reported `X-Forwarded-Proto: https`, else neither. Reads accept both cookie names, so a
  session established over one scheme is not lost when the other is used.
- **TOTP recovery codes and re-enrollment refusal verified, not changed (ISSUE-0189,
  ISSUE-0190, ISSUE-0221).** Both reported defects turned out to be already implemented, so
  this is a test-only change. Recovery codes are accepted at login, consumed single-use, and
  the consumption is compare-and-swapped on the stored hash set; a second enrollment on an
  already-enabled account is refused outright, so the "clicks Enroll, closes the tab, silently
  loses 2FA" scenario cannot occur. Six tests now prove it: accepting a recovery code,
  rejecting its second use, rejecting a never-issued code, refusing re-enrollment of an
  enabled account (asserting the row survives the refusal), and pinning that a *pending*
  account's secret is replaced by a second enroll — recorded as known-benign behaviour since
  pending-enrollment state does not exist. No production code changed.
- **Deleting a price that does not exist no longer reports success (ISSUE-0192).**
  `DELETE /admin/api/prices` and `DELETE /api/user/providers/{id}/prices` discarded
  `delete_price_tx`'s `bool` and returned `204` unconditionally, so an operator removing a
  stale price had no way to learn it was never there. Both now return `404` when the delete
  matched no row. No helper change was needed: `execute_atomic_mutation` is already generic
  over its closure's return type, so the `bool` travels back through it.
- **Disabling a service account evicts its child keys' pins (ISSUE-0193).** Deleting was
  already correct — both delete handlers collect the child key ids before the mutation (the
  `ON DELETE CASCADE` would take them inside it) and evict after the commit. Disabling was
  not: `patch_admin_`/`patch_user_service_account_handler` published `ServiceAccounts` and
  returned, leaving in-memory pins live for a principal that is now off. Gate G2 refuses a
  disabled service account on the *next* request, so no traffic escaped, but a pin held for
  a disabled principal is stale routing state. One `evict_service_account_child_pins`
  helper now serves both PATCH handlers, reusing the existing
  `collect_service_account_client_keys_tx` and `PinStore::invalidate_client_keys`.
- **UI-only preference updates no longer trigger a full snapshot rebuild (ISSUE-0182).** The
  handler published nothing itself, but `domains_for_path` has no branch for
  `/api/user/preferences`, so the control-mutation barrier fell through to "unknown domain,
  rebuild everything" and every theme change rebuilt all nine slices. The split now happens
  at the writer boundary: `default_routing_scope` in the payload goes through
  `execute_atomic_mutation(vec![ConfigDomain::Users])` — which also fixes a real staleness
  bug, since the data plane's `resolve_caller_scope` reads that column from the snapshot and
  a scope change that never published left the caller on the old routing — while a
  UI-only update writes directly and marks the mutation handled so the barrier does not
  publish for it.
- **The client-key TPM window no longer resets at wall-clock minute boundaries
  (follow-up found during ISSUE-0171's CI run).** `try_consume_client_key_tpm` keyed its
  window on `now_ms.div_euclid(60_000)`, so two requests a millisecond apart could straddle
  a minute boundary, land in different windows, and both pass a limit they jointly exceed.
  It surfaced as an intermittent `regression_test_f040_client_key_tpm_rate_limiting`
  failure on `aarch64-apple-darwin` — the second of two 25-token requests against a 30 TPM
  limit returned `503` instead of `429`, because the TPM gate had already reset. The window
  is now anchored on the first request of the burst and expires 60 s after it, and a
  regression test drives `now_ms` explicitly across the boundary so the race is
  reproducible rather than luck-dependent.
- **The routing matrix now agrees with the data plane about unapproved custom shares
  (ISSUE-0171).** `is_share_eligible` already refused a `custom` provider share whose
  `admin_approved_at` is `NULL`, so the inference path could not use one. The admin display
  and write queries in `admin/src/api/models.rs` filtered only on `accepted_at` /
  `revoked_at` / fingerprint, so a grantee saw the unapproved share in their routing matrix,
  could set a preference on it, and then their requests silently did not use it — with the
  matrix claiming it was available. Three sites now apply the same predicate: the routing
  matrix (`GET /api/user/routes`), the share check in `PUT /api/user/routes/{key}/{model}`,
  and the model-allow-list check in the same handler. The predicate exists once, as the
  `share_approval_guard!()` macro, pasted at compile time with `concat!` so `sqlx`'s
  injection lint stays satisfied. No traffic was ever misrouted by this: the data plane was
  correct throughout.
- **No new label redaction was added, and that is deliberate.** #171 also asked for grantor
  upstream-key labels to be hidden from share grantees. The normative specification says
  the opposite: `USR-4` scopes label-hiding to `global` keys ("nor `secret_last4` or raw
  `/usage` JSON of shared keys"), `4. SRDD:47` grants a grantee "key label … never the
  secret or `secret_last4`", `4. SRDD:118` grants it "before accepting the share", and
  `14.s.1` renders a standard user's shared column as `shared: <owner>/<label>`. A
  regression test now pins that a shared key's label is visible to the grantee while the
  upstream secret never is, so a future "redaction" cannot silently break the documented
  routing-matrix heading.
- **State-bound Responses operations no longer turn a decode failure into an empty
  `200 OK` (ISSUE-0156).** `forward_state_bound_response_op` used
  `decode_upstream_body(..).unwrap_or_default()`, so a corrupt, truncated, or
  decompression-bomb upstream body became an empty byte string while the response kept
  the upstream status — typically `200 OK`. A client reading an empty
  `GET /v1/responses/{id}` cannot distinguish "this response has no content" from "the
  gateway could not read the upstream's bytes", and proceeds on the wrong one. It is now
  `502` with `{"error":{"type":"upstream_decode_error",...}}`, mirroring the identical
  fix cycle 15 made in `post_messages_count_tokens`. The failure also lands in
  `usage_events` attributed to the linked upstream key (cost `0`, `error_code`
  `upstream_decode_error`), because a row credited to `"none"` when a real key answered
  is a row that exists but is wrong.
- **No body gate was added to state-bound operations (ISSUE-0155).** The issue asked for
  `state.budget_semaphore`, `check_transfer_encoding_smuggling`, and the three rate
  limiters on `forward_state_bound_response_op`. Three of the five are not applicable:
  the handler has no inbound request body at all — its signature takes
  `inbound: &HeaderMap` and all four callers (`GET`/`DELETE /v1/responses/{id}`,
  `/input_items`, `/cancel`) pass only `&headers`. Client RPM was already enforced
  (`auth.rs` sets `is_inference` from `path.starts_with("/v1/responses")`, which covers
  state-bound paths), and `upstream_rpm` / `upstream_concurrency` exist nowhere in the
  schema, the code, or the SRDD — they are not implemented features, so there was nothing
  to consume. Two regression tests pin the status quo so a future reader taking the
  issue's text at face value cannot make bodyless operations fail under load.
- **`count_tokens` now fails over, learns from failures, and never fakes a 200
  (ISSUE-0146).** The candidate loop in `post_messages_count_tokens` iterated every
  candidate but did nothing else right. Four defects, each with production
  consequence:
  - A send timeout or transport error hit `_ => continue`, so nothing was recorded:
    no `record_upstream_failure`, no key-health write, no slow metric. The key kept
    `healthy: true, consecutive_failures: 0` while being unusable, and because
    inference reads the same tracker it kept selecting that key. A retryable status
    (`401`/`403`/`429`/`5xx`) had the same problem inverted: it was handed to the
    client instead of failing over. Both now go through one
    `record_count_tokens_failure` helper and continue to the next candidate, and the
    retryable/non-retryable decision comes from the shared
    `classify_error_with_classifiers` vocabulary rather than a second `match` over
    status codes.
  - `decode_upstream_body(..).unwrap_or_default()` reported a corrupt compressed
    body as `200 OK` with **zero bytes**. A client reads that as "your prompt is
    empty" and sends the real request, so this was worse than an error. It is now
    `502` with `{"error":{"type":"upstream_decode_error",...}}`.
  - `record_request_metric` was never called, so `count_tokens` traffic did not exist
    in `llmroller_requests_total`. It is now emitted on every exit.
  The distinct `404`/`405`/`501` → `mark_aux_endpoint_unsupported` semantics are
  preserved: the key not implementing an endpoint is not a health problem, so it is
  cached and the key's health is left untouched.
- **Toolchain pin restored, musl CI leg repaired (ISSUE-0038).** `rust-toolchain.toml`
  pinned `channel = "stable"`, which overrides the `1.99.0` toolchain that
  `.github/workflows/ci.yml` installs. The runner's `stable` channel carries no
  `x86_64-unknown-linux-musl` `rust-std`, so the musl leg failed with
  `error[E0463]: can't find crate for 'core'`. The channel is now pinned to
  `1.99.0`, matching the MSRV, `Dockerfile`, and both workflows.
- **Declared MSRV made honest.** `Cargo.toml`'s `rust-version` changed from the
  never-tested `"1.95"` to `"1.99"`, the only version CI has ever compiled.
  The relationship between the pinned channel, the MSRV, and the four other
  places the version appears is now documented in SRDD Section 3 (assumption A5)
  and Section 15.1, and the misleading word "stable" was removed where it
  implied a floating channel.
- **Regression guard added.** The `xtask` test
  `tools::toolchain_channel_and_declared_msrv_agree` fails if the channel in
  `rust-toolchain.toml` and the MSRV in `Cargo.toml` ever name different
  `major.minor` compilers, or if the channel is a floating name rather than a
  pinned release.

- **Budget admission was blind to in-flight spend (ISSUE-0004).**
  `BudgetCounter::effective_used` meant "committed spend, projected by the latest
  upstream observation". `Reservation::try_reserve` compensated at its own call
  site by adding `reserved_in_flight`, but the two gates that decide *whether to
  try* — `headroom_bps_for_route` and `key_budget_admitted` — did not. A burst of
  concurrent streams could therefore each pass admission against a key whose
  budget was already fully spoken for, and headroom was reported as 10,000 bps for
  a key with none of it free. `effective_used` is now a default trait method that
  includes the in-flight term, and the manual addition is gone from
  `try_reserve`. The reservation term is added after the observation projection,
  not before, because a projection extrapolates upstream-reported spend and
  reservations are spend the provider never reported.
- **`counter_id` was a hash of the window id (ISSUE-0005).** `try_reserve` sorts
  counters by `counter_id` to establish the acquisition order its comment claims
  prevents deadlocks, but the id came from `DefaultHasher`, whose output is
  unspecified and unstable, in a `u64` that makes collisions improbable rather
  than impossible. Ids are now allocated monotonically from a process-wide
  `AtomicU64`, preserving an existing counter's id across rebuilds and hydration so
  the order stays stable across a config reload. No `dedup` was introduced:
  reserving on every counter in the list is correct, and deduplicating would have
  created the window-dropping bug the issue imagined.

### Fixed (metrics coverage)

- **The token counter omitted two token classes and reported list cost (ISSUE-0154).**
  Both completion paths emitted `llmroller_tokens_total` for `input`, `output` and
  `cache_read` only, so an operator could not see cache-write or reasoning volume at all —
  the two classes that most distinguish a reasoning model from a chat model. Both now emit
  `cache_write` and `reasoning`, mirroring the existing `cache_read` shape including its
  `> 0` guard.
- **The cost metric reported list price, not what was charged.** The metric now receives
  the plan-multiplied `effective_cost`, so a 50% plan multiplier shows half the list price
  instead of double what the customer paid. The **ledger is unchanged**: it keeps
  `list_cost_micro_usd` plus a separate `plan_multiplier_bps` so a later re-price can redo
  the multiplication (`USG-10`, SRDD 13). Pre-multiplying there would double-charge.
- **The token `reason` label had no closed value set.** `CLOSED_LABEL_ALLOWLIST` bounds
  *which labels exist*; nothing bounded *which values a label takes*, so any caller could
  mint a new Prometheus series and grow cardinality without limit. `reason` is the only
  value-carrying label on the token counter, so it now has its own
  `TOKEN_REASON_ALLOWLIST` (five values), with a `debug_assert!` plus early return in
  `record_tokens`. #128 will make `validate_metric_labels` enforce results; this is the set
  it will check.

### Fixed (retry and attribution)

- **A failed 429 short-retry silently reused the stale 429 (ISSUE-0152).** The
  same-key retry for `429` with `Retry-After <= 2 s` (`RTE-8`) had no failure branch: when
  the retry timed out or failed to connect, the code fell through and processed the
  **first** 429 as if it were current. The client was therefore told to back off a key
  that was actually unresponsive, the second upstream call went unrecorded in
  `usage_attempts`, and `record_upstream_failure` never ran so key health and the origin
  breaker did not learn. The branch now records the attempt, updates health, emits
  `record_upstream_slow` on the timeout shape, pushes a matching `TryOutcome` so
  `select_terminal_error` cannot re-select the stale 429, and continues the loop with a
  timeout/host-failure classification. `failovers` is deliberately not incremented — a
  same-key retry is not a failover.
- **The terminal all-failed event dropped session attribution (ISSUE-0153).** The event
  left `session_hash` and `pin_outcome` as `None` even when a session had been resolved
  and a pin consulted before every attempt failed. Both are now populated from `pin_key`
  and `pin_ticket`, using `PinOutcome::as_str`'s real vocabulary rather than a synthesised
  string. The Gate-G7 path in the same file correctly keeps `None`: no pin is consulted
  there, and a guess would look authoritative in the trace inspector.

### Changed (internal structure)

- **`orchestrator/pipeline.rs` decomposed ahead of the MNT-3 ceiling (ISSUE-0225).**
  The file had grown from 1,284 to 1,476 lines over three cycles, leaving 124 of the
  1,600-line gate's headroom — and the next three accounting issues (#152/#153/#154) all
  target it. Two pure moves, no behaviour change: terminal usage accounting
  (`TerminalUsageFacts`, `enqueue_terminal_usage`, `provider_billing`) to
  `terminal_accounting.rs`, and the 151-line outbound body/header preparation block to
  `attempt_body.rs`. `pipeline.rs` is now 1,245 lines with 355 lines of headroom, and the
  highest-churn region is out of the file that is under pressure. `orchestrator/mod.rs`
  re-exports both, so no caller changed and the test count stayed at 703.

### Fixed (dropped ledger rows)

- **An embedded 200 rejection never reached the ledger (ISSUE-0150).** When an upstream
  answered HTTP 200 with a `RequestRejected` error in the body, the non-streaming handler
  pushed an attempt row and returned a synthesised 400 — with no usage event. The
  streaming first-frame equivalent in `attempt_streaming.rs` was worse: it committed a
  failure charge and *then* returned a rejection without enqueuing, so money was billed
  with nothing recording it. Both now call the shared `enqueue_terminal_usage`, attributed
  to the key and provider that answered.
- **A Gate G7 budget refusal never reached the ledger (ISSUE-0151).** When the client-key
  monthly cap rejected a request at admission, the caller was told their budget was
  exhausted while `usage_events` held no record that they had tried — the refusal was
  invisible to `/usage`. The event now carries no key or provider attribution (the gate
  fails before any upstream call) and no cost, with `error_code` identifying which cap
  was hit.
- Also extracted `provider_billing(snapshot, provider_id)`: the same four-line billing
  lookup was repeated at four call sites and a fifth was about to be added.

Note: the non-retryable-4xx passthrough the issue also names as a drop site **already
emitted** — every non-2xx is intercepted at `pipeline.rs` before the attempt handler runs,
and that path enqueues. The test for it is kept as a status-quo pin rather than a red.

### Fixed (billing integrity)

- **A charged timeout never reached the usage ledger (ISSUE-0149).** On the
  `timeout_failover == false` branch the handler committed `charged_micro_usd` to the
  client-key accumulator and the share/global cap guards and then returned `504` —
  without enqueuing a usage event or its attempt rows. Spend therefore existed with no
  ledger entry, so `/usage` under-reported, budget rollups lost the money, and the
  per-key spend view diverged permanently from what was charged. The terminal
  all-attempts-failed emitter is now extracted into `enqueue_terminal_usage` and called
  from both sites. The timeout path attributes the charge to the upstream key and
  provider that were actually called, rather than to `"none"` — attributing real spend
  to no provider would satisfy "a row exists" while breaking the per-provider
  breakdown.

### Fixed (auxiliary endpoint gates)

- **`/v1/messages/count_tokens` skipped three pre-candidate gates (ISSUE-0145).**
  The handler was a hand-rolled miniature of the inference path: it scanned the
  envelope, resolved the model and enforced G6, but never reserved the preflight
  memory budget, never decompressed a compressed body, and never consumed
  client-key TPM. A compressed body was therefore forwarded **still compressed**, so
  the upstream received gzip bytes instead of JSON — an interop bug as well as a
  missing control. The contiguous pre-candidate prefix of `admit_inbound_request` is
  now extracted into a shared `gate_inbound_request` used by both handlers, so the
  two lists cannot drift apart again. Three of the seven controls the issue listed
  (`is_ip_rate_limited`, the RPM limiter, diagnostic-header extraction) were already
  applied by `client_key_auth_middleware`; they stay there and are deliberately not
  duplicated into the helper.

### Fixed (routing signal)

- **Non-streaming latency was always recorded as 0 ms (ISSUE-0148).**
  `handle_non_streaming_attempt` computed latency as
  `(now - attempt_started).max(0)`, but `now` is the *admission* timestamp and
  `attempt_started` is read just before the upstream send, so the difference is
  negative and clamps to zero. That zero was fed to
  `load_tracker.record_ttfb(route, 0, 0.2)`, so every successful non-streaming
  attempt dragged the route's P2C EWMA toward zero and the slowest route reported as
  the fastest — defeating `power_of_two_choices` for the whole non-streaming traffic
  class. Measured pre-fix against a mock upstream delaying 50 ms: EWMA = 0 ms. The
  measurement point now reads `state.clock.now_ms()`, matching the three sibling
  latency sites in the same file. Only the latency gets a fresh read; cost, usage,
  key-health and audit timestamps stay on the admission timestamp.

### Fixed (memory budget)

- **Decompression ran outside the preflight memory budget (ISSUE-0147).**
  `admit_inbound_request` decompressed the request body and only then requested
  preflight permits, sized on the decompressed length. The semaphore therefore
  bounded post-decompression concurrency only: N concurrent compressed requests
  each allocated up to `MAX_DECOMPRESSED_BYTES` (16 MiB) before any of them touched
  the budget, so an attacker could keep several GB live while the budget still read
  free. Measured pre-fix: 16 of 16 concurrent compressed bodies each inflated 4 MiB
  with no reservation — 64 MiB against a 17 MiB budget. The reservation is now taken
  before decompression and denominated in the decompressed ceiling, then reconciled
  down to the actual size via `OwnedSemaphorePermit::split`. Uncompressed bodies
  reserve only their wire length, so the common path keeps its throughput. The
  permit is held for the request's lifetime and released on every error path, so a
  failed decompression leaks nothing.

### Fixed (routing scope)

- **`inherit` routing scope was silently widened, not inherited (ISSUE-0009).**
  `resolve_caller_scope` resolves `inherit` against `users.default_routing_scope` and
  is used on the inference data plane, but two paths bypassed it and called the raw
  parser: `visible_models` (feeding `GET /v1/models`) and `resolve_usage_key` (the
  usage passthrough). `RoutingScope::from_str` has no `"inherit"` arm and the parser
  falls back to `RoutingScope::default()` = `PersonalFirst`, which is broader than
  `PersonalOnly`/`OwnOnly`/`GlobalOnly`. So an `inherit` key on a user whose default
  was `own_only` resolved to `PersonalFirst` — own plus shared plus global. That
  over-advertised models the data plane would refuse, and on the usage path let a
  strict user's scope resolve a key from an excluded tier. Both sites now call
  `resolve_caller_scope`. The parser's permissive fallback for *invalid* scope
  strings is unchanged and now pinned by a test, pending a decision on the
  client-key write path.

### Fixed (unbounded memory)

- **Per-IP authentication failure map was unbounded (ISSUE-0059).**
  `ProxyAuthState::ip_failures` is keyed by attacker-controlled source address and
  nothing capped it or swept it, so an unauthenticated attacker could grow it without
  limit by spraying distinct IPs. It is now bounded at `MAX_TRACKED_FAILURE_IPS`
  (10,000) with a sweep on insert: expired entries are dropped first, and if the map
  is still full one existing slot is recycled — recycling a live attacker entry
  resets that attacker's counter, which is strictly better than unbounded memory,
  while dropping the *new* entry would leave a flooding attacker never rate-limited
  at all. The sweep runs only at capacity, never on the read path, because an O(n)
  sweep per request would be a DoS amplifier. `tracked_failure_ip_count()` exposes
  the bound so it can be asserted.
- **Login limiter maps grew without bound on the failure path (ISSUE-0059).**
  `ThreeTierLoginLimiter` holds three maps keyed by source IP, arbitrary
  `(username, IP)` pairs and arbitrary usernames. Only `check()` had a size guard,
  and it dropped only empty vectors; `record_failure()` had none and is reachable
  after the CSRF gate without a preceding `check`. One shared
  `sweep_if_at_capacity` helper now serves all three maps from both paths. No
  threshold changed.
- **`record_success` left the username tier accumulating (ISSUE-0059).** It cleared
  only the `(username, IP)` pair, so a user who mistyped their password repeatedly
  across an hour stayed escalated to TOTP-or-delay permanently, even after
  authenticating successfully. It now clears the per-username tier too. The per-IP
  tier is deliberately not cleared, since one successful login from an address does
  not mean that address is not also attacking other accounts.

### Fixed (memory accounting)

- **Global trace memory release could wrap (ISSUE-0007).** `TraceTap`'s `Drop`
  used `fetch_sub` on an `AtomicUsize`, which wraps. Releasing more than the counter
  held drove it to a value near `usize::MAX`, after which every reservation failed
  and content traceability switched itself off silently and permanently. Release now
  floors at zero via compare-and-set. A `shortcut:`-style note in the code records
  that the floor is a defensive guard, not a correctness proof: under-release leaks
  upward and degrades traceability toward off (safe), while over-release wraps and
  kills it instantly (unsafe).
- **Trace memory admission was check-then-act (ISSUE-0007).** Both
  `reserve_global_bytes` and `try_feed_chunk` loaded the global counter, compared it
  against the budget, then added — so concurrent callers could each observe room that
  only one of them could have, and the sum of grants could exceed the cap. Both sites
  now share one compare-and-set helper, with saturating arithmetic so a pathological
  byte count can neither panic in debug nor wrap in release.
- **Trace memory is now observable (PERF-5).** `global_trace_memory()` returns
  `(used, budget)`, and `metrics::record_trace_memory` publishes
  `llmroller_trace_memory_bytes` and `llmroller_trace_budget_bytes` on the existing
  30-second background tick. A silent global counter is the kind of thing that
  degrades for months unnoticed.

### Fixed (security)

- **Login self-deadlock (ISSUE-0001).** `post_login` acquired the Argon2id
  semaphore twice in one request, holding the first permit while waiting for the
  second. With `N` permits, `N` concurrent logins each held one permit and
  blocked on another, so no permit could ever be released; the second acquisition
  was bounded at 2 s and mapped to `503 login_busy`, making every concurrent
  login fail. Both Argon2id call sites now go through one helper that holds a
  single permit for the blocking verification only, and the user lookup no longer
  runs under a permit. The missing-user constant-time dummy verification, which
  previously ran without a permit at all, is now bounded the same way.
- **Session CSRF key was a public constant (ISSUE-0002).** The authenticated
  session CSRF token was `HMAC(a compile-time constant in the source tree,
  session_token)`, because `AuthAppState` had no field for the instance secret
  and every caller used the fallback. Anyone who learned a session token could
  compute the matching CSRF header offline, so the header added no proof over the
  cookie. `AuthAppState` now carries `session_hmac_secret`, wired from
  `instance_secrets` in `cli::serve` exactly as the data plane already does, and
  the hand-rolled HMAC is replaced by `hmac::Hmac<sha2::Sha256>`. The no-secret
  API form is removed; the fallback for test and CLI paths is a per-process
  random key rather than a public literal.
- **Pre-login CSRF comparison leaked token length (ISSUE-0045).** The
  double-submit check compared the cookie and header with
  `subtle::ConstantTimeEq` directly, which returns early on unequal lengths and
  is constant-time only for equal-length inputs. Both sides are now compared as
  fixed-width SHA-256 digests, matching the authenticated-session path. Harmless
  today because the nonce is a constant-width base64url value, but it constrained
  any future token-format change.
- **Signed pre-login CSRF cookie declined (ISSUE-0046).** Signing the cookie was
  analysed and deliberately not implemented: it stops the same attacker for the
  same reason the existing double-submit equality check already stops them, and
  it would cost an HMAC per page render plus a cookie-format migration.

### Fixed (tests)

- **`e2e_sigterm_drain` could not fail for the right reason.** The test built
  `ShutdownSupervisor` via `new(...)` and never called `.with_shutdown_cut(...)`,
  so step 4 of `execute_graceful_shutdown` was a no-op and the assertion labelled
  "Step 4: Stragglers cancelled with ShutdownCut" actually observed
  `notify_waiters()` waking a task. Because `notify_waiters()` drops the
  notification when no waiter is registered yet, the test lost the race
  intermittently (5 failures in 10 runs under `cargo test`). The test now arms the
  cut flag, registers its simulated straggler before the drain begins, and asserts
  the flag itself; wake-up coverage is kept as a separately named assertion. The
  sibling in-crate test `shutdown_broadcasts_background_stop_and_completes` was
  aligned to cover step 4 as well. Production behaviour is unchanged.

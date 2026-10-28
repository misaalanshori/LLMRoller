//! The admin settings payload's **key set** must stay closed and fixed (`ISSUE-0231`).
//!
//! # Why this test, and not an escaping test
//!
//! `crates/admin/static/app-settings.js` interpolates each settings **key** into `id=` and
//! `for=` attributes. It is safe today for exactly one reason: `get_admin_settings_handler`
//! returns `Json<RuntimeSettings>`, a closed Rust struct, so every key the browser ever sees is
//! a compile-time-fixed `[a-z_]+` identifier. Nothing in the JS records that dependency, and
//! nothing tested it.
//!
//! The failure mode is a refactor, not an attack: change the handler to return
//! `HashMap<String, serde_json::Value>` — the natural shape, and `crates/store/src/settings.rs`
//! already keys a string registry — and a key containing `"` or `<` reaches an unescaped
//! attribute. A test that only checked escaping would pass in both worlds, because it would have
//! nothing hostile to escape. **This test is the one that goes red the day the payload stops
//! being closed**, which is why it is the deliverable rather than a decoration.
//!
//! `admin_settings_payload_key_set_is_closed` asserts the served JSON object's key set equals a
//! literal list. Adding a field to `RuntimeSettings` therefore requires editing this list, which
//! is the point: the JS's safety depends on the list, so a change to the list should be a
//! deliberate act in a diff rather than an accident of a refactor.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt as _;
use tempfile::tempdir;
use tower::ServiceExt as _;

use llmroller_admin::auth::{AuthAppState, AuthConfig, derive_session_csrf_with_secret};
use llmroller_admin::setup::SetupAppState;
use llmroller_admin::{ApiAppState, control_plane_router_full};
use llmroller_core::ids::UserId;
use llmroller_core::rbac::{ActorScope, Role, SystemSeal, SystemTask};
use llmroller_core::seams::{Clock as _, ExposeSecret as _, OsEntropy};
use llmroller_store::crypto::MasterKey;
use llmroller_store::instance::load_or_initialize;
use llmroller_store::migrations::migrate;
use llmroller_store::pool::{Role as PoolRole, open_pool};
use llmroller_store::settings::seed_defaults;
use llmroller_store::users::{NewUser, NewWebSession, create_user, insert_web_session};

/// Every key `GET /admin/api/settings` may return.
///
/// This list **is** the guarantee `app-settings.js` relies on when it puts a key into an
/// `id`/`for` attribute. It mirrors `RuntimeSettings`' fields exactly. Adding a field means
/// adding it here — deliberately, in a diff — because the browser-side safety depends on it.
const CLOSED_SETTINGS_KEYS: [&str; 14] = [
    "default_routing_strategy",
    "drain_timeout_secs",
    "global_exposure_mode",
    "headroom_scaling_enabled",
    "max_active_pins",
    "pin_ttl_secs",
    "price_sync_catalog_url",
    "recovery_repin_min_idle_secs",
    "repin_on_personal_recovery",
    "traceability_config",
    "unpriced_fallback_rates",
    "usage_events_max_rows",
    "usage_events_retention_days",
    "usage_poll_interval_secs",
];

/// A settings key is used as an HTML element id, so it must be an identifier.
///
/// Mirrors the guard in `app-settings.js` (`isValidSettingKey`) and the shape
/// `RuntimeSettings` guarantees. Kept here as the Rust-side statement of the same rule so the
/// two cannot drift into disagreeing about what "safe to interpolate" means.
fn is_identifier_shaped(key: &str) -> bool {
    !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

struct Harness {
    _dir: tempfile::TempDir,
    pool: sqlx::SqlitePool,
    master_key: MasterKey,
    admin: UserId,
    session_hmac_secret: llmroller_core::seams::SecretBytes,
}

async fn harness() -> Harness {
    let dir = tempdir().expect("temp dir");
    let pool = open_pool(&dir.path().join("settings_keys.db"), PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&pool).await.expect("migrate");
    seed_defaults(&pool, None, 1).await.expect("seed settings");

    let master_key = MasterKey::from_bytes([0xC8; 32]);
    let instance_state = load_or_initialize(&pool, "key-0", &master_key, Arc::new(OsEntropy), 1)
        .await
        .expect("instance load");

    let system_actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());
    let admin = create_user(
        &pool,
        &system_actor,
        NewUser {
            username: "admin_c37",
            password: "supersecretadminc37pass",
            role: Role::Admin,
            must_change_password: false,
        },
        &OsEntropy,
        1,
    )
    .await
    .expect("admin");

    Harness {
        _dir: dir,
        pool,
        master_key,
        admin: admin.id,
        session_hmac_secret: instance_state.session_hmac_secret.clone(),
    }
}

async fn admin_session(harness: &Harness) -> (String, String) {
    let raw_token = "admin_c37_settings_session_token_0123";
    let now_ms = llmroller_core::seams::SystemClock::new().now_ms();
    let csrf =
        derive_session_csrf_with_secret(harness.session_hmac_secret.expose_secret(), raw_token);
    insert_web_session(
        &harness.pool,
        NewWebSession {
            id_hash: &llmroller_store::crypto::sha256_hex(raw_token.as_bytes()),
            user_id: harness.admin,
            csrf_hash: &llmroller_store::crypto::sha256_hex(csrf.as_bytes()),
            ip_addr: Some("127.0.0.1"),
            user_agent: Some("c37-settings"),
            created_at: now_ms,
            expires_at: now_ms + 86_400_000,
        },
    )
    .await
    .expect("session");
    (format!("llmroller_session={raw_token}"), csrf)
}

fn build_app(harness: &Harness) -> axum::Router {
    let mut auth_state = AuthAppState::new(
        harness.pool.clone(),
        AuthConfig {
            dev_insecure_cookies: true,
            require_tls: false,
            trusted_proxies: vec![],
        },
    );
    auth_state.session_hmac_secret = Some(harness.session_hmac_secret.clone());
    let setup_state = SetupAppState {
        pool: harness.pool.clone(),
        expected_token: Some("c37-token".to_string()),
        data_dir: harness._dir.path().to_path_buf(),
        clock: Arc::new(llmroller_core::seams::SystemClock::new()),
        entropy: Arc::new(OsEntropy),
        audit_hmac_key: None,
        argon2_semaphore: llmroller_admin::setup::new_argon2_pool(),
    };
    let api_state = ApiAppState::new(
        auth_state.clone(),
        harness.master_key.clone(),
        "key-0".to_string(),
    );
    control_plane_router_full(setup_state, auth_state, api_state)
}

/// `GET /admin/api/settings` and return the parsed JSON object.
async fn get_settings(harness: &Harness) -> serde_json::Value {
    let (cookie, _csrf) = admin_session(harness).await;
    let app = build_app(harness);
    let request = Request::builder()
        .method("GET")
        .uri("/admin/api/settings")
        .header(header::COOKIE, &cookie)
        .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 54_322))))
        .body(Body::empty())
        .expect("settings request");
    let response = app.oneshot(request).await.expect("settings response");
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "GET /admin/api/settings must succeed for an authenticated admin"
    );
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    serde_json::from_slice(&bytes).expect("settings json")
}

// ---------------------------------------------------------------------------
// The durable test
// ---------------------------------------------------------------------------

/// The settings payload must be a **closed object with a fixed key set**.
///
/// `app-settings.js` puts each key into an `id`/`for` attribute without escaping it, and is safe
/// only because this payload cannot grow a key the struct does not declare. If the handler ever
/// returns `HashMap<String, _>` — or a field is added that a string-keyed source can populate —
/// this assertion fails, and it fails *before* anything hostile reaches a browser.
///
/// Note what this test deliberately does **not** do: it does not check escaping. Escaping would
/// pass against a map-shaped payload that happened to contain only benign keys, which is exactly
/// the state that makes the injection latent rather than loud.
#[tokio::test]
#[doc = "Spec: SEC-6, MDL-3 — ISSUE-0231 admin settings payload key set is closed"]
async fn admin_settings_payload_key_set_is_closed() {
    let harness = harness().await;
    let payload = get_settings(&harness).await;

    let object = payload
        .as_object()
        .expect("the settings payload must be a JSON object");
    let mut served: Vec<&str> = object.keys().map(String::as_str).collect();
    served.sort_unstable();

    assert_eq!(
        served, CLOSED_SETTINGS_KEYS,
        "the settings payload's key set must equal the closed list `app-settings.js` relies on; \
         a new or removed key changes what the UI interpolates into element ids, so it must be \
         a deliberate edit to CLOSED_SETTINGS_KEYS rather than a side effect of a refactor"
    );

    // Every key must be an identifier, because every key becomes an element id. This is the
    // property that makes the unescaped interpolation safe, stated directly rather than
    // inferred from the list above.
    for key in served {
        assert!(
            is_identifier_shaped(key),
            "{key:?} is not identifier-shaped; `app-settings.js` uses it as an element id, so a \
             key containing a quote, a space or '<' would reach an attribute unescaped"
        );
    }
}

/// A key the struct does not declare must be **dropped**, not forwarded.
///
/// This is the half of the guarantee that a key-set assertion alone would miss: the payload is
/// parsed out of a string-keyed `settings` table (`store/src/settings.rs::parse_settings_rows`),
/// so an unrecognised row is *ignored* rather than surfacing as an extra JSON key. If that
/// `_ => {}` arm were ever changed to forward unknown keys, the payload would become an open map
/// at runtime while `RuntimeSettings` still looked closed — and this test would catch it.
///
/// Reachable via a direct `INSERT`: the `settings` table has no constraint tying `key` to the
/// struct, so a stale row from an older build, a hand-edited database, or a future migration is
/// a real input.
#[tokio::test]
#[doc = "Spec: SEC-6 — ISSUE-0231 an undeclared settings row must not reach the payload"]
async fn an_undeclared_settings_row_does_not_appear_in_the_payload() {
    let harness = harness().await;

    let hostile_key = "evil\"><script>alert(1)</script>";
    sqlx::query("INSERT INTO settings (key, value_json, updated_at) VALUES (?, ?, ?)")
        .bind(hostile_key)
        .bind("\"x\"")
        .bind(1_i64)
        .execute(&harness.pool)
        .await
        .expect("insert undeclared settings row");

    let payload = get_settings(&harness).await;
    let object = payload.as_object().expect("settings object");

    assert!(
        !object.contains_key(hostile_key),
        "an undeclared settings row must be ignored by `parse_settings_rows`, not forwarded into \
         the payload where the UI would interpolate it into an element id"
    );

    let mut served: Vec<&str> = object.keys().map(String::as_str).collect();
    served.sort_unstable();
    assert_eq!(
        served, CLOSED_SETTINGS_KEYS,
        "an undeclared row must not change the payload's key set"
    );
}

// ---------------------------------------------------------------------------
// The JS-side guard (Option B)
// ---------------------------------------------------------------------------

/// `app-settings.js` must validate a settings key before using it as an element id.
///
/// # Why this is a source assertion and not a behavioural one
///
/// There is **no JS test harness in this repository**: no `package.json`, no `node_modules`, no
/// `*.test.js`, no `assert`-style self-check in any of the twelve modules, and no node/npm step
/// in `.github/workflows/`. (`app-*.js` files are exercised only indirectly, by
/// `all_static_assets_are_under_1600_lines` and the module load-order test, both of which read
/// their *bytes*.) So a behavioural JS test would mean inventing a build step, which §5 forbids.
///
/// What is assertable from Rust is the structural property that closes the injection: the key
/// must pass an identifier check *before* it reaches a template, and an invalid key must not
/// render. That is the guarantee; asserting it here fails loudly if someone deletes the guard,
/// which is the same protection a JS unit test would give.
#[test]
#[doc = "Spec: SEC-6 — ISSUE-0231 settings keys are validated before reaching an attribute"]
fn app_settings_validates_a_key_before_using_it_as_an_element_id() {
    let source = include_str!("../../admin/static/app-settings.js");

    // The guard must exist and be a real predicate, not a name mentioned in a comment.
    assert!(
        source.contains("function isValidSettingKey("),
        "app-settings.js must define `isValidSettingKey`; without it a key containing a quote, \
         '<' or a space reaches an `id`/`for` attribute unescaped, and the payload being closed \
         today is an accident rather than a guarantee"
    );

    let loader_start = source
        .find("async function loadAdminSettings()")
        .expect("loadAdminSettings must exist");
    let loader = &source[loader_start..];
    let loader_end = loader
        .find("\nasync function ")
        .or_else(|| loader.find("\nfunction "))
        .unwrap_or(loader.len());
    let loader = &loader[..loader_end];

    let guard_at = loader.find("isValidSettingKey(key)").expect(
        "`loadAdminSettings` must call `isValidSettingKey(key)`; defining the predicate without \
         calling it closes nothing",
    );
    let id_at = loader
        .find("`setting-${key}`")
        .expect("`loadAdminSettings` must still build the element id from the key");

    assert!(
        guard_at < id_at,
        "the key must be validated BEFORE it is interpolated into an element id; the guard \
         currently appears after the interpolation, so an invalid key would already be in the \
         template"
    );

    // The invalid branch must not fall through to the render. `continue` or `return` both do it;
    // silence (`if (!valid) { }`) would not.
    let between = &loader[guard_at..id_at];
    assert!(
        between.contains("continue") || between.contains("return"),
        "a key failing validation must be skipped, not rendered; the guard is present but the \
         invalid branch falls through to the template"
    );

    // The guard's actual rule, so it cannot be weakened to `() => true` while the call site
    // still reads as guarded.
    assert!(
        source.contains("/^[a-z0-9_]+$/"),
        "`isValidSettingKey` must test the identifier shape `^[a-z0-9_]+$`, which is the shape \
         `RuntimeSettings` guarantees; a weaker or missing pattern would accept a hostile key"
    );
}

// ---------------------------------------------------------------------------
// The attribute-position interpolation census
// ---------------------------------------------------------------------------

/// No `.js` module may interpolate a non-literal value into an attribute without escaping it.
///
/// # How this scans, and what it deliberately ignores
///
/// It walks `crates/admin/static/app-*.js`, and for each line finds `${...}` occurring inside a
/// quoted attribute value (`id="…"`, `for="…"`, `data-*="…"`, `value="…"`, `class="…"`, …). An
/// interpolation counts as **guarded** when the expression begins with `escapeHtml(`, and as
/// **allowed** when it is a literal that cannot carry markup:
///
/// - a numeric/constant name (`s`, `value` bound to a number, `pool_tier`),
/// - a ternary whose branches are string literals (`${value ? 'checked' : ''}`),
/// - a template of literals.
///
/// The predicate is deliberately narrow: "starts with `escapeHtml(`" rather than "contains it",
/// because `${escapeHtml(a)}${b}` would pass a `contains` check while `b` is still raw.
///
/// # Why a test instead of a one-off audit
///
/// The cycle-37 census found 18 attribute-position interpolations across five modules and only
/// six of them raw, all of them the same settings key. That count changes as modules are edited,
/// so the census belongs in CI rather than in a report that ages. This is the same argument as
/// the MNT-3 ceiling next door.
#[test]
#[doc = "Spec: SEC-6 — ISSUE-0231 no raw interpolation into an HTML attribute"]
fn no_module_interpolates_into_an_attribute_without_escaping() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates dir")
        .join("admin/static");

    // Attributes whose value ends up parsed by the HTML parser. `data-*` and `aria-*` included:
    // an unescaped `"` there still breaks out of the attribute.
    const ATTRS: [&str; 15] = [
        "id",
        "for",
        "value",
        "name",
        "class",
        "style",
        "href",
        "src",
        "title",
        "placeholder",
        "data-id",
        "data-key",
        "data-alias",
        "data-json",
        "data-action",
    ];

    let mut violations: Vec<String> = Vec::new();
    let mut inspected = 0_usize;

    for module in [
        "app-accounts.js",
        "app-catalog.js",
        "app-charts.js",
        "app-core.js",
        "app-keys.js",
        "app-prices.js",
        "app-providers.js",
        "app-sessions.js",
        "app-settings.js",
        "app-shares.js",
        "app-users.js",
        "app-auth.js",
    ] {
        let path = root.join(module);
        let Ok(source) = std::fs::read_to_string(&path) else {
            continue;
        };
        for (index, line) in source.lines().enumerate() {
            for attr in ATTRS {
                let needle = format!("{attr}=\"");
                let mut search_from = 0;
                while let Some(offset) = line[search_from..].find(&needle) {
                    let value_start = search_from + offset + needle.len();
                    // The attribute value ends at the next `"`, which may close after a `${…}`.
                    let Some(value_end_rel) = line[value_start..].find('"') else {
                        break;
                    };
                    let attribute_value = &line[value_start..value_start + value_end_rel];
                    search_from = value_start + value_end_rel + 1;

                    // Every `${…}` inside this attribute value must be guarded or literal.
                    let mut rest = attribute_value;
                    while let Some(start) = rest.find("${") {
                        let inner_start = start + 2;
                        // Brace-match, so `${f({a: 1})}` and nested templates survive.
                        let mut depth = 1_usize;
                        let mut end_rel = None;
                        for (i, ch) in rest[inner_start..].char_indices() {
                            match ch {
                                '{' => depth += 1,
                                '}' => {
                                    depth -= 1;
                                    if depth == 0 {
                                        end_rel = Some(inner_start + i);
                                        break;
                                    }
                                }
                                _ => {}
                            }
                        }
                        let Some(end_rel) = end_rel else { break };
                        let expr = rest[inner_start..end_rel].trim();
                        inspected += 1;

                        let guarded = expr.starts_with("escapeHtml(");
                        let literal = expr
                            .chars()
                            .all(|c| c.is_ascii_digit() || c == ' ' || c == '.');
                        // `${cond ? 'a' : 'b'}` with single-quoted literal branches.
                        let ternary_literals = expr.contains('?')
                            && !expr.contains('<')
                            && expr
                                .split(['?', ':'])
                                .skip(1)
                                .all(|part| part.trim_start().starts_with('\''));

                        if !guarded && !literal && !ternary_literals {
                            violations.push(format!(
                                "{module}:{} — {attr}=\"…${{{expr}}}…\" is interpolated raw",
                                index + 1
                            ));
                        }
                        rest = &rest[end_rel + 1..];
                    }
                }
            }
        }
    }

    assert!(
        inspected >= 15,
        "the census inspected only {inspected} attribute interpolations, so it is not reaching \
         the modules it claims to check and would pass vacuously"
    );
    assert!(
        violations.is_empty(),
        "every value interpolated into an HTML attribute must be escaped or a literal, or a key \
         containing a quote reaches the markup unescaped:\n  {}",
        violations.join("\n  ")
    );
}

//! The `app-*.js` module split is served, wired, and complete (`ISSUE-0013`).
//!
//! # The failure modes these tests exist for
//!
//! Splitting one embedded asset into twelve introduces two ways to break a page silently, and
//! neither shows up as a compile error:
//!
//! 1. **Registered but not served** — a module is added to `APP_JS_MODULES` and the router
//!    lookup is wrong, so `/static/app-<domain>.js` 404s and *that page loses half its
//!    behaviour*. The browser console shows it; a server-side test suite does not.
//! 2. **Template and asset drift** — a `<script src="/static/app-<domain>.js">` names a module
//!    that no longer exists, or a module exists that no template loads. The first is a silent
//!    missing feature; the second is dead weight that still ships.
//!
//! Both are cheap to check and were the most likely mistakes in this change, so they are
//! checked by parsing the templates rather than by trusting a hand-maintained list.

use super::*;
use axum::http::header;

/// Every module must be served with the right content type and a non-empty body.
#[tokio::test]
#[doc = "Spec: UI-3, MNT-3 — ISSUE-0013 every app-*.js module is served"]
async fn every_app_module_is_served_as_javascript() {
    let (dir, pool, master_key, _admin_id, instance_state) = setup_dashboard_test_db().await;
    let auth_config = AuthConfig {
        dev_insecure_cookies: true,
        require_tls: false,
        trusted_proxies: vec![],
    };
    let mut auth_state = AuthAppState::new(pool.clone(), auth_config);
    auth_state.session_hmac_secret = Some(instance_state.session_hmac_secret.clone());
    let setup_state = SetupAppState {
        pool: pool.clone(),
        expected_token: Some("mock_token".to_string()),
        data_dir: dir.path().to_path_buf(),
        clock: Arc::new(llmroller_core::seams::SystemClock::new()),
        entropy: Arc::new(OsEntropy),
        audit_hmac_key: None,
        argon2_semaphore: llmroller_admin::setup::new_argon2_pool(),
    };
    let api_state = ApiAppState::new(auth_state, master_key, "key-0".to_string());
    let app = control_plane_router_full(setup_state, api_state.auth.clone(), api_state);

    assert!(
        !llmroller_admin::assets::APP_JS_MODULES.is_empty(),
        "the module table must not be empty, or this test proves nothing"
    );

    for (name, body) in llmroller_admin::assets::APP_JS_MODULES {
        let req = Request::builder()
            .method("GET")
            .uri(format!("/static/{name}"))
            .body(Body::empty())
            .expect("req");
        let res = app.clone().oneshot(req).await.expect("res");
        assert_eq!(
            res.status(),
            StatusCode::OK,
            "/static/{name} must be served"
        );
        let content_type = res
            .headers()
            .get(header::CONTENT_TYPE)
            .expect("content-type")
            .to_str()
            .expect("str")
            .to_string();
        assert_eq!(
            content_type, "application/javascript",
            "/static/{name} must be served as JavaScript"
        );
        let served = res.into_body().collect().await.expect("body").to_bytes();
        assert!(!served.is_empty(), "/static/{name} must not be empty");
        assert_eq!(
            served.len(),
            body.len(),
            "/static/{name} must serve the registered body verbatim"
        );
    }
}

/// `/static/app.js` must still serve the whole application.
///
/// Kept as a single-file fallback and because existing tests assert on its contents. If the
/// concatenation ever dropped a module, a page loaded from this path would behave differently
/// from one loaded from the twelve tags — a divergence with no symptom until a button stops
/// working.
#[tokio::test]
#[doc = "Spec: UI-3 — ISSUE-0013 the concatenated app.js contains every module"]
async fn concatenated_app_js_contains_every_module() {
    let whole = llmroller_admin::assets::APP_JS;
    for (name, body) in llmroller_admin::assets::APP_JS_MODULES {
        // Each module body is non-trivial, so require a distinctive slice of it rather than
        // the whole text (headers differ, trailing newlines are trimmed at the join).
        let probe = body
            .lines()
            .find(|l| l.len() > 40 && !l.trim_start().starts_with("//"))
            .unwrap_or_else(|| panic!("{name} has no content line to probe for"));
        assert!(
            whole.contains(probe.trim()),
            "APP_JS must contain {name}'s content; probe {probe:?} not found"
        );
    }
}

/// Every `<script src="/static/...">` in every template must resolve to a registered asset.
///
/// This is the check that catches both drift directions at once. It parses the templates
/// rather than using a hand-written list, so adding a tag without an asset — or renaming an
/// asset without updating the tags — fails here.
#[test]
#[doc = "Spec: UI-3, MNT-3 — ISSUE-0013 every template script tag resolves to a served asset"]
fn every_template_script_tag_resolves_to_a_registered_asset() {
    use llmroller_admin::assets::APP_JS_MODULES;

    let registered: std::collections::BTreeSet<String> = APP_JS_MODULES
        .iter()
        .map(|(name, _)| format!("/static/{name}"))
        .chain(
            [
                "/static/app.js",
                "/static/app.css",
                "/static/htmx.min.js",
                "/static/uPlot.iife.min.js",
                "/static/uPlot.min.css",
            ]
            .iter()
            .map(|s| (*s).to_string()),
        )
        .collect();

    let templates_dir = super::templates_dir();
    let mut checked = 0_usize;
    let mut seen_modules: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

    for entry in std::fs::read_dir(&templates_dir).expect("read templates dir") {
        let path = entry.expect("entry").path();
        if path.extension().is_none_or(|e| e != "html") {
            continue;
        }
        let source = std::fs::read_to_string(&path).expect("template utf-8");
        for src in extract_script_srcs(&source) {
            checked += 1;
            assert!(
                registered.contains(&src),
                "{} references {src}, which is not a registered asset — that tag silently \
                 loads nothing and the page loses the behaviour in it",
                path.display()
            );
            if let Some(name) = src.strip_prefix("/static/")
                && name.starts_with("app-")
            {
                seen_modules.insert(name.to_string());
            }
        }
    }

    assert!(
        checked > 0,
        "no <script src> tags found, so this test proves nothing"
    );
    // The shell and the two standalone pages each load the full module list, so every module
    // must appear at least once. A module nothing loads is dead weight that still ships.
    for (name, _) in APP_JS_MODULES {
        assert!(
            seen_modules.contains(name),
            "{name} is registered but no template loads it"
        );
    }
}

/// Pull every `src="/static/..."` from `<script ...>` tags.
fn extract_script_srcs(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    for tag in source.split("<script") {
        if !tag.contains("</script>") && !tag.contains("/>") {
            continue;
        }
        if let Some(idx) = tag.find("src=\"") {
            let rest = &tag[idx + 5..];
            if let Some(end) = rest.find('"') {
                let src = &rest[..end];
                if src.starts_with("/static/") {
                    out.push(src.to_string());
                }
            }
        }
    }
    out
}

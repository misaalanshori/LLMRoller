//! Tests for model normalization, merge/split, and exposure rules.
//!
//! Spec: Section 6.6, Section 10.6, Requirements: MDL-2, MDL-3, MDL-4, MDL-5, MDL-6.

use super::*;
use std::collections::HashMap;

#[doc = "Spec: MDL-2, MDL-3, MDL-4, MDL-5, MDL-6"]
#[test]
fn test_normalization_and_exposure() {
    // 1. Idempotency and digit-dot preservation
    let cases = [
        ("anthropic/claude-sonnet-4.6", "claude-sonnet-4.6"),
        ("openai/gpt-5.4-pro:free", "gpt-5.4-pro-free"),
        ("mimo/mimo_v2.6_pro", "mimo-v2.6-pro"),
        (
            "qwen/qwen2.5-coder-32b-instruct",
            "qwen2.5-coder-32b-instruct",
        ),
        ("deepseek-v4-flash", "deepseek-v4-flash"),
        ("deepseek-v4.1-flash", "deepseek-v4.1-flash"),
        ("claude-sonnet-4.6:extended", "claude-sonnet-4.6-extended"),
        ("claude-sonnet-4.6:thinking", "claude-sonnet-4.6-thinking"),
        ("claude-sonnet-4.6:nitro", "claude-sonnet-4.6"),
        ("claude-sonnet-4.6:floor", "claude-sonnet-4.6"),
        ("claude-sonnet-4.6:beta", "claude-sonnet-4.6-beta"),
    ];

    for (raw, expected) in cases {
        let normalized = normalize_model_id(raw).expect("normalization must succeed");
        assert_eq!(normalized, expected, "raw={raw}");

        // Idempotency: normalizing again must yield identical string
        let second = normalize_model_id(&normalized).expect("second normalization must succeed");
        assert_eq!(second, expected, "idempotency failed for {raw}");
    }

    // Rejection of invalid characters or empty or too long
    assert!(normalize_model_id("").is_err());
    assert!(normalize_model_id("model with \n newline").is_err());
    assert!(normalize_model_id("model with \t tab").is_err());
    assert!(normalize_model_id(&"a".repeat(257)).is_err());

    // 2. Vendor keeping for internal collision resolution
    let vendor_a = normalize_keep_vendor("vendor-a/x").expect("vendor-a/x");
    let vendor_b = normalize_keep_vendor("vendor-b/x").expect("vendor-b/x");
    assert_eq!(vendor_a, "vendor-a--x");
    assert_eq!(vendor_b, "vendor-b--x");
    assert_ne!(vendor_a, vendor_b);

    // 3. Preset alias resolution and ambiguous raw upstream ID (HTTP 409)
    let mut catalog = CatalogState::default();
    let pk_sonnet = ModelPk::generate();
    let model_sonnet = ModelRecord {
        model_pk: pk_sonnet,
        canonical_id: "claude-sonnet-4.6".to_string(),
        origin: ModelOrigin::Global,
        owner_user_id: None,
        exposed: None,
        merge_state: MergeState::Confirmed,
        routing_strategy: None,
    };
    catalog.models.insert(pk_sonnet, model_sonnet);
    catalog
        .models_by_canonical
        .insert("claude-sonnet-4.6".to_string(), pk_sonnet);

    // Preset alias: claude-sonnet-4-6-20260514 -> claude-sonnet-4.6
    catalog
        .model_aliases
        .insert("claude-sonnet-4-6-20260514".to_string(), pk_sonnet);

    // Resolves directly via alias
    assert_eq!(
        catalog
            .resolve_model("claude-sonnet-4-6-20260514")
            .expect("alias must resolve"),
        pk_sonnet
    );

    // Add two distinct routes for an ambiguous raw ID "shared-raw-id"
    let pk_other = ModelPk::generate();
    let model_other = ModelRecord {
        model_pk: pk_other,
        canonical_id: "other-model".to_string(),
        origin: ModelOrigin::Global,
        owner_user_id: None,
        exposed: None,
        merge_state: MergeState::Confirmed,
        routing_strategy: None,
    };
    catalog.models.insert(pk_other, model_other);
    catalog
        .models_by_canonical
        .insert("other-model".to_string(), pk_other);

    catalog.routes.push(ModelRoute {
        upstream_key_id: UpstreamKeyId::generate(),
        model_pk: pk_sonnet,
        upstream_model_id: "shared-raw-id".to_string(),
        vendor_prefix: None,
        status: RouteStatus::Active,
        missing_since_ms: None,
    });
    catalog.routes.push(ModelRoute {
        upstream_key_id: UpstreamKeyId::generate(),
        model_pk: pk_other,
        upstream_model_id: "shared-raw-id".to_string(),
        vendor_prefix: None,
        status: RouteStatus::Active,
        missing_since_ms: None,
    });

    // Resolving "shared-raw-id" must return AmbiguousUpstreamId
    assert!(matches!(
        catalog.resolve_model("shared-raw-id"),
        Err(ResolveModelError::AmbiguousUpstreamId(_))
    ));

    // 4. Merge model A into B repoints model_pk and adds model_redirects
    let pk_a = ModelPk::generate();
    let pk_b = ModelPk::generate();
    catalog.models.insert(
        pk_a,
        ModelRecord {
            model_pk: pk_a,
            canonical_id: "model-a".to_string(),
            origin: ModelOrigin::Global,
            owner_user_id: None,
            exposed: None,
            merge_state: MergeState::Confirmed,
            routing_strategy: None,
        },
    );
    catalog
        .models_by_canonical
        .insert("model-a".to_string(), pk_a);
    catalog.models.insert(
        pk_b,
        ModelRecord {
            model_pk: pk_b,
            canonical_id: "model-b".to_string(),
            origin: ModelOrigin::Global,
            owner_user_id: None,
            exposed: None,
            merge_state: MergeState::Confirmed,
            routing_strategy: None,
        },
    );
    catalog
        .models_by_canonical
        .insert("model-b".to_string(), pk_b);

    catalog.routes.push(ModelRoute {
        upstream_key_id: UpstreamKeyId::generate(),
        model_pk: pk_a,
        upstream_model_id: "upstream-a".to_string(),
        vendor_prefix: None,
        status: RouteStatus::Active,
        missing_since_ms: None,
    });

    // Merge A -> B
    catalog.merge_models(pk_a, pk_b);
    assert!(!catalog.models.contains_key(&pk_a));
    assert!(catalog.models.contains_key(&pk_b));
    assert_eq!(catalog.model_redirects.get("model-a"), Some(&pk_b));
    // Route repointed to pk_b
    let route = catalog
        .routes
        .iter()
        .find(|r| r.upstream_model_id == "upstream-a")
        .expect("route must exist");
    assert_eq!(route.model_pk, pk_b);
    // Request for "model-a" resolves to pk_b via redirect!
    assert_eq!(
        catalog
            .resolve_model("model-a")
            .expect("redirect must resolve"),
        pk_b
    );

    // 5. Global exposure: Union vs Intersection
    let p1 = ProviderId::generate();
    let p2 = ProviderId::generate();
    let p3 = ProviderId::generate();

    let mut provider_usable = HashMap::new();
    provider_usable.insert(p1, true); // p1 has usable keys
    provider_usable.insert(p2, false); // p2 has NO usable keys
    // p3 is absent from map (zero configured keys)

    let test_model = ModelRecord {
        model_pk: ModelPk::generate(),
        canonical_id: "test-exposure".to_string(),
        origin: ModelOrigin::Global,
        owner_user_id: None,
        exposed: None, // Auto exposure
        merge_state: MergeState::Confirmed,
        routing_strategy: None,
    };

    // Served by p1 and p2:
    // In Union mode: p1 is usable -> true
    assert!(is_global_model_exposed(
        &test_model,
        GlobalExposureMode::Union,
        &provider_usable,
        &[p1, p2]
    ));
    // In Intersection mode: p2 is active but not usable -> false
    assert!(!is_global_model_exposed(
        &test_model,
        GlobalExposureMode::Intersection,
        &provider_usable,
        &[p1, p2]
    ));

    // Served by p1 and p3 (p3 has zero keys, so ignored in Intersection):
    assert!(is_global_model_exposed(
        &test_model,
        GlobalExposureMode::Intersection,
        &provider_usable,
        &[p1, p3]
    ));

    // Admin kill switch (exposed = Some(false))
    let mut kill_switched = test_model.clone();
    kill_switched.exposed = Some(false);
    assert!(!is_global_model_exposed(
        &kill_switched,
        GlobalExposureMode::Union,
        &provider_usable,
        &[p1]
    ));

    // 6. Personal origin upgrade and cap check
    let user_id = UserId::generate();
    let mut personal_model = ModelRecord {
        model_pk: ModelPk::generate(),
        canonical_id: "personal-model".to_string(),
        origin: ModelOrigin::Personal,
        owner_user_id: Some(user_id),
        exposed: None,
        merge_state: MergeState::Confirmed,
        routing_strategy: None,
    };
    upgrade_personal_to_global(&mut personal_model);
    assert_eq!(personal_model.origin, ModelOrigin::Global);
    assert_eq!(personal_model.owner_user_id, None);
}

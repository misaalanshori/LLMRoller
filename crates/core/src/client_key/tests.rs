//! Unit tests for client key generation, grace rotation, and request gates G1-G6.
//!
//! Spec: ./docs/srdd/6. SRDD - Functional Requirements.md#65-client-keys-clk
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec
//! Spec: ./docs/srdd/10. SRDD - Routing Design.md#100-ordered-request-gates-g1-through-g7
//! Phase: ./docs/bigbang/big-bang-phases-2.md#22-sub-phase-22-client-key-llr_-auth-rotation-grace-model-request-gates-g1g6-and-listeners

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;

use crate::client_key::{
    ClientKeyRecord, ClientKeyRpmLimiter, GateFailure, HashRole, evaluate_request_gates,
    generate_client_key, rotate_client_key_in_place,
};
use crate::ids::{ClientKeyId, UserId};
use crate::rbac::Role;
use crate::seams::DeterministicRng;
use crate::snapshot::{ConfigSnapshot, UserRecord};

fn setup_test_snapshot(
    user_id: UserId,
    user_disabled: bool,
    key_record: ClientKeyRecord,
    hash_role: HashRole,
    key_hash: [u8; 32],
) -> ConfigSnapshot {
    let mut users = HashMap::new();
    users.insert(
        user_id,
        UserRecord {
            id: user_id,
            username: "test_user".to_string(),
            role: Role::User,
            disabled: user_disabled,
            can_use_global_providers: true,
            global_budget_micro_usd_month: None,
        },
    );

    let mut client_keys = HashMap::new();
    client_keys.insert(key_hash, (key_record, hash_role));

    ConfigSnapshot {
        config_version: 1,
        users: Arc::new(users),
        settings: Arc::new(crate::settings::RuntimeSettings::default()),
        providers: Arc::new(HashMap::new()),
        upstream_keys: Arc::new(HashMap::new()),
        client_keys_by_hash: Arc::new(client_keys),
    }
}

/// Spec: CLK-1, SEC-3
/// Verify that client key generation generates `llr_` + 32 base62 characters,
/// extracts a valid prefix, and computes the canonical SHA-256 hash.
#[doc = "Spec: CLK-1, SEC-3"]
#[test]
fn test_client_key_generation() {
    let entropy = DeterministicRng::new(0x1234_5678);
    let (token, prefix, hash) = generate_client_key(&entropy);

    assert!(token.starts_with("llr_"));
    assert_eq!(token.len(), 36);
    assert_eq!(prefix.len(), 10);
    assert!(prefix.starts_with("llr_"));
    assert_ne!(hash, [0u8; 32]);

    // Deterministic entropy replay produces identical token
    let replay_entropy = DeterministicRng::new(0x1234_5678);
    let (token2, prefix2, hash2) = generate_client_key(&replay_entropy);
    assert_eq!(token, token2);
    assert_eq!(prefix, prefix2);
    assert_eq!(hash, hash2);
}

/// Spec: CLK-1, SEC-3
/// Verify in-place rotation with grace period:
/// At +299 s, predecessor hash is accepted; at +301 s, predecessor hash fails G1.
#[doc = "Spec: CLK-1, SEC-3"]
#[test]
fn test_client_key_grace_rotation() {
    let entropy = DeterministicRng::new(0xABCD);
    let user_id = UserId::generate();
    let (token1, _prefix1, hash1) = generate_client_key(&entropy);

    let mut record = ClientKeyRecord {
        id: ClientKeyId::generate(),
        owner_user_id: user_id,
        label: "agent".to_string(),
        prefix: token1[..10].to_string(),
        key_hash: hash1,
        prev_key_hash: None,
        prev_key_expires_at: None,
        routing_scope: "inherit".to_string(),
        allowed_models: None,
        rate_limit_rpm: None,
        budget_micro_usd_month: None,
        allowed_cidrs: None,
        constant_session_mode: "flag".to_string(),
        disabled: false,
        revoked_at: None,
        expires_at: None,
    };

    let now_ms = 1_000_000;
    let grace_secs = 300;
    let (_token2, hash2) = rotate_client_key_in_place(&mut record, &entropy, grace_secs, now_ms);

    assert_ne!(hash1, hash2);
    assert_eq!(record.key_hash, hash2);
    assert_eq!(record.prev_key_hash, Some(hash1));
    assert_eq!(record.prev_key_expires_at, Some(now_ms + 300_000));

    // At +299 s: predecessor hash succeeds as HashRole::Previous
    let snapshot_299 = {
        let mut map = HashMap::new();
        map.insert(hash2, (record.clone(), HashRole::Current));
        map.insert(hash1, (record.clone(), HashRole::Previous));
        let mut users = HashMap::new();
        users.insert(
            user_id,
            UserRecord {
                id: user_id,
                username: "u".to_string(),
                role: Role::User,
                disabled: false,
                can_use_global_providers: true,
                global_budget_micro_usd_month: None,
            },
        );
        ConfigSnapshot {
            config_version: 2,
            users: Arc::new(users),
            settings: Arc::new(crate::settings::RuntimeSettings::default()),
            providers: Arc::new(HashMap::new()),
            upstream_keys: Arc::new(HashMap::new()),
            client_keys_by_hash: Arc::new(map),
        }
    };

    let at_299_ms = now_ms + 299_000;
    let res_curr = evaluate_request_gates(&hash2, None, None, None, &snapshot_299, None, at_299_ms)
        .expect("current hash at +299s");
    assert_eq!(res_curr.1, HashRole::Current);

    let res_prev = evaluate_request_gates(&hash1, None, None, None, &snapshot_299, None, at_299_ms)
        .expect("predecessor hash at +299s");
    assert_eq!(res_prev.1, HashRole::Previous);

    // At +301 s: predecessor hash fails G1 (authentication_error)
    let at_301_ms = now_ms + 301_000;
    let err_prev = evaluate_request_gates(&hash1, None, None, None, &snapshot_299, None, at_301_ms)
        .expect_err("predecessor hash at +301s must fail G1");
    assert_eq!(err_prev, GateFailure::G1AuthenticationError);

    // Current hash still succeeds at +301 s
    let res_curr_301 =
        evaluate_request_gates(&hash2, None, None, None, &snapshot_299, None, at_301_ms)
            .expect("current hash at +301s");
    assert_eq!(res_curr_301.1, HashRole::Current);
}

/// Spec: CLK-1, SEC-3
/// Verify that ordered request gates G1 through G6 fail with the exact deterministic failure code.
#[doc = "Spec: CLK-1, SEC-3"]
#[test]
fn test_request_gates_g1_through_g6_ordering() {
    let entropy = DeterministicRng::new(0x9999);
    let user_id = UserId::generate();
    let (_token, _prefix, hash) = generate_client_key(&entropy);
    let now_ms = 500_000;

    let base_record = ClientKeyRecord {
        id: ClientKeyId::generate(),
        owner_user_id: user_id,
        label: "test".to_string(),
        prefix: "llr_test".to_string(),
        key_hash: hash,
        prev_key_hash: None,
        prev_key_expires_at: None,
        routing_scope: "inherit".to_string(),
        allowed_models: Some(vec!["claude-sonnet-4.6".to_string()]),
        rate_limit_rpm: Some(60),
        budget_micro_usd_month: None,
        allowed_cidrs: Some(vec!["192.168.1.0/24".to_string()]),
        constant_session_mode: "flag".to_string(),
        disabled: false,
        revoked_at: None,
        expires_at: Some(now_ms + 100_000),
    };

    // G1: Unknown hash -> G1AuthenticationError
    let snap_g1 = setup_test_snapshot(user_id, false, base_record.clone(), HashRole::Current, hash);
    let unknown_hash = [0xFF; 32];
    let err = evaluate_request_gates(&unknown_hash, None, None, None, &snap_g1, None, now_ms)
        .expect_err("unknown hash must fail G1");
    assert_eq!(err, GateFailure::G1AuthenticationError);

    // G2: Disabled client key -> G2KeyDisabledOrExpired
    let mut rec_disabled = base_record.clone();
    rec_disabled.disabled = true;
    let snap_g2_disabled =
        setup_test_snapshot(user_id, false, rec_disabled, HashRole::Current, hash);
    let err = evaluate_request_gates(&hash, None, None, None, &snap_g2_disabled, None, now_ms)
        .expect_err("disabled key must fail G2");
    assert_eq!(err, GateFailure::G2KeyDisabledOrExpired);

    // G2: Revoked client key -> G2KeyDisabledOrExpired
    let mut rec_revoked = base_record.clone();
    rec_revoked.revoked_at = Some(now_ms - 10);
    let snap_g2_revoked = setup_test_snapshot(user_id, false, rec_revoked, HashRole::Current, hash);
    let err = evaluate_request_gates(&hash, None, None, None, &snap_g2_revoked, None, now_ms)
        .expect_err("revoked key must fail G2");
    assert_eq!(err, GateFailure::G2KeyDisabledOrExpired);

    // G2: Expired client key -> G2KeyDisabledOrExpired
    let mut rec_expired = base_record.clone();
    rec_expired.expires_at = Some(now_ms - 1);
    let snap_g2_expired = setup_test_snapshot(user_id, false, rec_expired, HashRole::Current, hash);
    let err = evaluate_request_gates(&hash, None, None, None, &snap_g2_expired, None, now_ms)
        .expect_err("expired key must fail G2");
    assert_eq!(err, GateFailure::G2KeyDisabledOrExpired);

    // G3: Owner disabled -> G3OwnerDisabled
    let snap_g3 = setup_test_snapshot(user_id, true, base_record.clone(), HashRole::Current, hash);
    let err = evaluate_request_gates(&hash, None, None, None, &snap_g3, None, now_ms)
        .expect_err("disabled owner must fail G3");
    assert_eq!(err, GateFailure::G3OwnerDisabled);

    // G4: Client IP outside allowed CIDRs -> G4CidrForbidden
    let snap_valid =
        setup_test_snapshot(user_id, false, base_record.clone(), HashRole::Current, hash);
    let wrong_ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
    let err = evaluate_request_gates(&hash, Some(wrong_ip), None, None, &snap_valid, None, now_ms)
        .expect_err("IP outside CIDR must fail G4");
    assert_eq!(err, GateFailure::G4CidrForbidden);

    // G4: Correct IP passes G4
    let correct_ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 42));

    // G5: Rate limit exceeded -> G5RateLimitExceeded
    let limiter = ClientKeyRpmLimiter::default();
    // Consume all 60 tokens
    for _ in 0..60 {
        let _ = evaluate_request_gates(
            &hash,
            Some(correct_ip),
            None,
            None,
            &snap_valid,
            Some(&limiter),
            now_ms,
        );
    }
    let err = evaluate_request_gates(
        &hash,
        Some(correct_ip),
        None,
        None,
        &snap_valid,
        Some(&limiter),
        now_ms,
    )
    .expect_err("rate limit exceeded must fail G5");
    assert!(matches!(err, GateFailure::G5RateLimitExceeded { .. }));

    // G6: Requested model not in allowed_models -> G6ModelForbidden
    let err = evaluate_request_gates(
        &hash,
        Some(correct_ip),
        Some("gpt-5-pro"),
        Some(&["gpt-5-pro".to_string(), "claude-sonnet-4.6".to_string()]),
        &snap_valid,
        None,
        now_ms,
    )
    .expect_err("forbidden model must fail G6");
    assert_eq!(
        err,
        GateFailure::G6ModelForbidden {
            model: "gpt-5-pro".to_string()
        }
    );

    // G6: Requested model unknown to catalog -> G6ModelNotFound
    let err = evaluate_request_gates(
        &hash,
        Some(correct_ip),
        Some("unknown-model-xyz"),
        Some(&["claude-sonnet-4.6".to_string()]),
        &snap_valid,
        None,
        now_ms,
    )
    .expect_err("unknown model must fail G6");
    assert_eq!(
        err,
        GateFailure::G6ModelNotFound {
            model: "unknown-model-xyz".to_string()
        }
    );

    // All gates pass!
    let (key_rec, role) = evaluate_request_gates(
        &hash,
        Some(correct_ip),
        Some("claude-sonnet-4.6"),
        Some(&["claude-sonnet-4.6".to_string()]),
        &snap_valid,
        None,
        now_ms,
    )
    .expect("all gates must pass");
    assert_eq!(key_rec.id, base_record.id);
    assert_eq!(role, HashRole::Current);
}

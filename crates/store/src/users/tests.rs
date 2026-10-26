//! User repository, password, TOTP, session, and break-glass tests.
//!
//! Spec: ./docs/srdd/6. SRDD - Functional Requirements.md#61-setup-accounts-and-authentication-auth
//! Spec: ./docs/srdd/8. SRDD - Security and Privacy.md#82-security-requirements-sec
//! Phase: ./docs/bigbang/big-bang-phases-1.md#14-sub-phase-14-atomic-first-admin-setup-argon2id--totp-auth-and-reset-admin-cli

use std::sync::Arc;

use llmroller_core::rbac::{ActorScope, CliCommand, Role, SystemSeal, SystemTask};
use llmroller_core::seams::{DeterministicRng, ExposeSecret};
use sqlx::{AssertSqlSafe, SqlitePool};
use tempfile::TempDir;
use totp_rs::{Algorithm as TotpAlgorithm, Builder as TotpBuilder};

use super::{
    NewUser, NewWebSession, UserError, UserPatch, change_password, clear_totp, confirm_totp,
    count_users, create_user, delete_user, enroll_totp, find_login_record, hash_password,
    insert_web_session, list_users, load_web_session, patch_user, reset_admin, set_disabled,
    set_role, validate_password, verify_login_totp, verify_password,
};
use crate::crypto::{MasterKey, XChaCha20Poly1305Sealer};
use crate::migrations::migrate;
use crate::pool::{Role as PoolRole, open_pool};

const NOW: i64 = 1_760_000_000_000;
const AUDIT_KEY: &[u8] = b"0123456789abcdef0123456789abcdef";

async fn database() -> (TempDir, SqlitePool) {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("llmroller.db");
    let pool = open_pool(&path, PoolRole::Writer)
        .await
        .expect("writer pool must open");
    migrate(&pool).await.expect("schema must migrate");
    (directory, pool)
}

fn system_actor() -> ActorScope {
    ActorScope::system(SystemTask::Cli(CliCommand::ResetAdmin), SystemSeal::store())
}

fn sealer(entropy: Arc<DeterministicRng>) -> XChaCha20Poly1305Sealer {
    XChaCha20Poly1305Sealer::new(MasterKey::from_bytes([0x42; 32]), [0x11; 16], entropy)
}

async fn create_admin(
    pool: &SqlitePool,
    entropy: &DeterministicRng,
    username: &str,
) -> super::User {
    create_user(
        pool,
        &system_actor(),
        NewUser {
            username,
            password: "correct horse battery",
            role: Role::Admin,
            must_change_password: false,
        },
        entropy,
        NOW,
    )
    .await
    .expect("admin must be created")
}

fn totp_for(secret_base32: &str) -> totp_rs::Totp {
    TotpBuilder::new()
        .with_algorithm(TotpAlgorithm::SHA1)
        .with_digits(6)
        .with_skew(1)
        .with_step_duration(30)
        .with_secret(totp_rs::Secret::try_from_base32(secret_base32).expect("base32 secret"))
        .build()
        .expect("TOTP builds")
}

#[tokio::test]
#[doc = "Spec: AUTH-4, SEC-4"]
async fn argon2id_hashes_use_mandated_parameters_and_unique_128_bit_salts() {
    let entropy = DeterministicRng::new(7);
    let first = hash_password("correct horse battery", &entropy).expect("password hashes");
    let second = hash_password("correct horse battery", &entropy).expect("password hashes");
    assert_ne!(first, second, "every hash must use a fresh salt");
    assert!(verify_password("correct horse battery", &first));
    assert!(!verify_password("wrong password", &first));

    assert!(first.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"));
    let first_salt = first.split('$').nth(4).expect("salt component");
    let second_salt = second.split('$').nth(4).expect("salt component");
    assert_ne!(first_salt, second_salt);
    assert_eq!(
        first_salt.len(),
        22,
        "128-bit salts encode to 22 characters"
    );
    assert!(matches!(
        validate_password("short"),
        Err(UserError::WeakPassword)
    ));
}

#[tokio::test]
#[doc = "Spec: AUTH-4, AUTH-5"]
async fn create_list_and_last_admin_guards_are_atomic() {
    let (_directory, pool) = database().await;
    let entropy = DeterministicRng::new(8);
    let admin = create_admin(&pool, &entropy, "admin").await;
    let second = create_admin(&pool, &entropy, "second").await;
    let actor = ActorScope::user(admin.id, Role::Admin);
    assert_eq!(list_users(&pool, &actor).await.expect("list").len(), 2);
    assert_eq!(count_users(&pool).await.expect("count"), 2);

    let admin_after = set_role(&pool, &actor, second.id, Role::User, NOW)
        .await
        .expect("demote second");
    assert_eq!(admin_after.role, Role::User);
    let error = set_disabled(&pool, &actor, admin.id, true, NOW)
        .await
        .expect_err("last admin cannot be disabled");
    assert!(matches!(error, UserError::LastAdmin));
    let still_admin = super::get_user_by_id(&pool, admin.id).await.expect("user");
    assert!(!still_admin.disabled);
    assert_eq!(still_admin.role, Role::Admin);

    let patch = UserPatch {
        can_use_global_providers: Some(false),
        ..UserPatch::default()
    };
    let patched = patch_user(&pool, &actor, second.id, patch, NOW)
        .await
        .expect("patch");
    assert!(!patched.can_use_global_providers);
}

#[tokio::test]
#[doc = "Spec: AUTH-4"]
async fn duplicate_usernames_and_weak_passwords_are_rejected() {
    let (_directory, pool) = database().await;
    let entropy = DeterministicRng::new(9);
    create_admin(&pool, &entropy, "admin").await;
    let weak = create_user(
        &pool,
        &system_actor(),
        NewUser {
            username: "weak",
            password: "short",
            role: Role::User,
            must_change_password: false,
        },
        &entropy,
        NOW,
    )
    .await;
    assert!(matches!(weak, Err(UserError::WeakPassword)));
    let duplicate = create_user(
        &pool,
        &system_actor(),
        NewUser {
            username: "admin",
            password: "correct horse battery",
            role: Role::User,
            must_change_password: false,
        },
        &entropy,
        NOW,
    )
    .await;
    assert!(matches!(duplicate, Err(UserError::DuplicateUsername)));
}

#[tokio::test]
#[doc = "Spec: AUTH-4"]
async fn deletion_cascades_personal_state_but_retains_usage_and_records_tombstone() {
    let (_directory, pool) = database().await;
    let entropy = DeterministicRng::new(10);
    let admin = create_admin(&pool, &entropy, "admin").await;
    let target = create_admin(&pool, &entropy, "target").await;
    let actor = ActorScope::user(admin.id, Role::Admin);
    let provider = "01ARZ3NDEKTSV4RRFFQ69G5FAX";
    let key = "01ARZ3NDEKTSV4RRFFQ69G5FAY";
    let client = "01ARZ3NDEKTSV4RRFFQ69G5FAZ";
    sqlx::query(
        "INSERT INTO providers (id, ownership_scope, owner_user_id, kind, name, base_url, config_json, created_at)
         VALUES (?, 'personal', ?, 'custom', 'p', 'https://example.test', '{}', ?)",
    )
    .bind(provider)
    .bind(target.id.to_string())
    .bind(NOW)
    .execute(&pool)
    .await
    .expect("provider");
    sqlx::query(
        "INSERT INTO upstream_keys (id, provider_id, owner_user_id, label, secret_enc, master_key_id, secret_last4, created_at)
         VALUES (?, ?, ?, 'k', x'00', 'k1', 'last', ?)",
    )
    .bind(key)
    .bind(provider)
    .bind(target.id.to_string())
    .bind(NOW)
    .execute(&pool)
    .await
    .expect("upstream key");
    sqlx::query(
        "INSERT INTO client_keys (id, owner_user_id, label, prefix, key_hash, created_at)
         VALUES (?, ?, 'c', 'llr_', 'hash', ?)",
    )
    .bind(client)
    .bind(target.id.to_string())
    .bind(NOW)
    .execute(&pool)
    .await
    .expect("client key");
    insert_web_session(
        &pool,
        NewWebSession {
            id_hash: "session-hash",
            user_id: target.id,
            csrf_hash: "csrf-hash",
            ip_addr: None,
            user_agent: None,
            created_at: NOW,
            expires_at: NOW + 1_000,
        },
    )
    .await
    .expect("session");
    sqlx::query(
        "INSERT INTO usage_events \
         (id, ts, request_id, client_key_id, owner_user_id, upstream_key_id, \
          endpoint, status, stream, cost_micro_usd, cost_source) \
         VALUES ('01ARZ3NDEKTSV4RRFFQ69G5FB0', ?, 'req', ?, ?, ?, 'chat', 200, 0, 1, 'provider')",
    )
    .bind(NOW)
    .bind(client)
    .bind(target.id.to_string())
    .bind(key)
    .execute(&pool)
    .await
    .expect("usage event");

    delete_user(&pool, &actor, target.id, "target", NOW + 2)
        .await
        .expect("delete");
    assert_eq!(count_users(&pool).await.expect("count"), 1);
    for table in ["providers", "upstream_keys", "client_keys", "web_sessions"] {
        let count: i64 = sqlx::query_scalar(AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}")))
            .fetch_one(&pool)
            .await
            .expect("count table");
        assert_eq!(count, 0, "{table} must be empty");
    }
    let usage: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM usage_events")
        .fetch_one(&pool)
        .await
        .expect("usage count");
    assert_eq!(usage, 1);
    let tombstone: String =
        sqlx::query_scalar("SELECT username FROM deleted_users WHERE user_id = ?")
            .bind(target.id.to_string())
            .fetch_one(&pool)
            .await
            .expect("tombstone");
    assert_eq!(tombstone, "target");
}

#[tokio::test]
#[doc = "Spec: AUTH-5"]
async fn deletion_requires_exact_username_confirmation() {
    let (_directory, pool) = database().await;
    let entropy = DeterministicRng::new(11);
    let admin = create_admin(&pool, &entropy, "admin").await;
    let target = create_admin(&pool, &entropy, "target").await;
    let actor = ActorScope::user(admin.id, Role::Admin);
    let error = delete_user(&pool, &actor, target.id, "wrong", NOW)
        .await
        .expect_err("mismatch");
    assert!(matches!(error, UserError::ConfirmationMismatch));
    assert_eq!(count_users(&pool).await.expect("count"), 2);
}

#[tokio::test]
#[doc = "Spec: AUTH-9"]
async fn totp_enrollment_verification_replay_and_recovery_are_enforced() {
    let (_directory, pool) = database().await;
    let entropy = Arc::new(DeterministicRng::new(12));
    let admin = create_admin(&pool, &entropy, "admin").await;
    let actor = ActorScope::user(admin.id, Role::Admin);
    let sealer = sealer(Arc::clone(&entropy));
    let enrollment = enroll_totp(&pool, &actor, &sealer, "key-1", entropy.as_ref())
        .await
        .expect("enroll");
    assert_eq!(enrollment.recovery_codes.len(), 10);
    let totp = totp_for(enrollment.secret_base32.expose_secret());
    let now_seconds = u64::try_from(NOW / 1000).expect("now");
    let code = totp.generate(now_seconds).to_string();
    confirm_totp(&pool, &actor, &code, &sealer, "key-1", NOW)
        .await
        .expect("confirm");
    let record = find_login_record(&pool, "admin")
        .await
        .expect("record")
        .expect("exists");
    assert!(record.user.totp_enabled);

    let replay = verify_login_totp(&pool, admin.id, &code, &sealer, "key-1", NOW).await;
    assert!(matches!(replay, Err(UserError::TotpReplay)));
    let next = totp.generate(now_seconds + 30).to_string();
    let verified = verify_login_totp(&pool, admin.id, &next, &sealer, "key-1", NOW + 30_000)
        .await
        .expect("next step");
    assert_eq!(verified, super::TotpVerification::Totp);

    let recovery = enrollment.recovery_codes[0].expose_secret().to_owned();
    let used = verify_login_totp(&pool, admin.id, &recovery, &sealer, "key-1", NOW + 60_000)
        .await
        .expect("recovery");
    assert_eq!(used, super::TotpVerification::Recovery);
    let reused =
        verify_login_totp(&pool, admin.id, &recovery, &sealer, "key-1", NOW + 90_000).await;
    assert!(matches!(reused, Err(UserError::InvalidTotp)));
    clear_totp(&pool, &actor, admin.id).await.expect("clear");
}

#[tokio::test]
#[doc = "Spec: AUTH-4, AUTH-6"]
async fn password_change_requires_current_password_and_revokes_other_sessions() {
    let (_directory, pool) = database().await;
    let entropy = DeterministicRng::new(13);
    let admin = create_admin(&pool, &entropy, "admin").await;
    let actor = ActorScope::user(admin.id, Role::Admin);
    insert_web_session(
        &pool,
        NewWebSession {
            id_hash: "keep",
            user_id: admin.id,
            csrf_hash: "csrf",
            ip_addr: None,
            user_agent: None,
            created_at: NOW,
            expires_at: NOW + 1_000,
        },
    )
    .await
    .expect("session one");
    insert_web_session(
        &pool,
        NewWebSession {
            id_hash: "drop",
            user_id: admin.id,
            csrf_hash: "csrf2",
            ip_addr: None,
            user_agent: None,
            created_at: NOW,
            expires_at: NOW + 1_000,
        },
    )
    .await
    .expect("session two");
    let wrong = change_password(
        &pool,
        &actor,
        "wrong password",
        "new correct horse battery",
        &entropy,
        Some("keep"),
    )
    .await;
    assert!(matches!(wrong, Err(UserError::InvalidCredentials)));
    change_password(
        &pool,
        &actor,
        "correct horse battery",
        "new correct horse battery",
        &entropy,
        Some("keep"),
    )
    .await
    .expect("change");
    assert!(
        load_web_session(&pool, "keep")
            .await
            .expect("load")
            .is_some()
    );
    assert!(
        load_web_session(&pool, "drop")
            .await
            .expect("load")
            .is_none()
    );
    let record = find_login_record(&pool, "admin")
        .await
        .expect("record")
        .expect("exists");
    assert!(verify_password(
        "new correct horse battery",
        &record.password_hash
    ));
}

#[tokio::test]
#[doc = "Spec: AUTH-6"]
async fn reset_admin_restores_access_and_appends_break_glass_audit_row() {
    let (_directory, pool) = database().await;
    let entropy = DeterministicRng::new(14);
    let user = create_admin(&pool, &entropy, "admin").await;
    let actor = ActorScope::user(user.id, Role::Admin);
    set_role(&pool, &actor, user.id, Role::User, NOW)
        .await
        .expect_err("cannot demote only admin");
    sqlx::query("UPDATE users SET role = 'user', disabled = 1 WHERE id = ?")
        .bind(user.id.to_string())
        .execute(&pool)
        .await
        .expect("simulate lockout");
    let result = reset_admin(&pool, "admin", &entropy, AUDIT_KEY, NOW + 5)
        .await
        .expect("reset");
    assert_eq!(result.user_id, user.id);
    let restored = super::get_user_by_id(&pool, user.id).await.expect("user");
    assert_eq!(restored.role, Role::Admin);
    assert!(!restored.disabled);
    assert!(restored.must_change_password);
    let record = find_login_record(&pool, "admin")
        .await
        .expect("record")
        .expect("exists");
    assert!(verify_password(
        result.temporary_password.expose_secret(),
        &record.password_hash
    ));
    let action: String =
        sqlx::query_scalar("SELECT action FROM audit_log ORDER BY seq DESC LIMIT 1")
            .fetch_one(&pool)
            .await
            .expect("audit");
    assert_eq!(action, "admin.break_glass");
}

#[tokio::test]
#[doc = "Spec: AUTH-4, AUTH-9, SEC-3, SEC-5"]
async fn totp_recovery_optimistic_concurrency_and_reset_admin_bump_config_version() {
    let (_directory, pool) = database().await;
    crate::instance::load_or_initialize(
        &pool,
        "key-1",
        &MasterKey::from_bytes([0x42; 32]),
        Arc::new(DeterministicRng::new(0x42)),
        NOW,
    )
    .await
    .expect("bootstrap instance_meta");

    let entropy = Arc::new(DeterministicRng::new(0x77));
    let admin = create_admin(&pool, &entropy, "admin").await;
    let actor = ActorScope::user(admin.id, Role::Admin);
    let seal = sealer(Arc::clone(&entropy));
    let enrollment = enroll_totp(&pool, &actor, &seal, "key-1", entropy.as_ref())
        .await
        .expect("enroll");
    let totp = totp_for(enrollment.secret_base32.expose_secret());
    let now_seconds = u64::try_from(NOW / 1_000).expect("now");
    let code = totp.generate(now_seconds).to_string();
    confirm_totp(&pool, &actor, &code, &seal, "key-1", NOW)
        .await
        .expect("confirm");

    let recovery = enrollment.recovery_codes[0].expose_secret().to_owned();
    let used = verify_login_totp(&pool, admin.id, &recovery, &seal, "key-1", NOW + 60_000)
        .await
        .expect("recovery first use");
    assert_eq!(used, super::TotpVerification::Recovery);
    let reused = verify_login_totp(&pool, admin.id, &recovery, &seal, "key-1", NOW + 61_000).await;
    assert!(matches!(reused, Err(UserError::InvalidTotp)));

    let v_before: i64 = sqlx::query_scalar("SELECT config_version FROM instance_meta WHERE singleton = 1")
        .fetch_one(&pool)
        .await
        .expect("v_before");
    let _reset = reset_admin(&pool, None, entropy.as_ref(), NOW + 70_000, AUDIT_KEY)
        .await
        .expect("reset_admin");
    let v_after: i64 = sqlx::query_scalar("SELECT config_version FROM instance_meta WHERE singleton = 1")
        .fetch_one(&pool)
        .await
        .expect("v_after");
    assert!(v_after > v_before, "reset_admin must bump instance_meta.config_version (FND-020)");
}

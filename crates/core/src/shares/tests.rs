//! Tests for consented sharing, overlap checks, and cap enforcement.
//!
//! Spec: Section 4.4, Section 4.6, Section 6.4, Section 8.2, Section 10.1,
//! Requirements: SHR-1, SHR-2, SHR-3, SHR-4, USR-2, USR-3, USR-4.

use super::*;
use crate::accumulators::{BudgetScopeId, MonthlyAccumulatorRegistry};
use crate::ids::ShareId;

#[doc = "Spec: SHR-1, SHR-2, SHR-3, SHR-4, USR-2, USR-3, USR-4"]
#[test]
fn test_effective_share_eligibility_and_consent_lifecycle() {
    let alice = UserId::generate();
    let bob = UserId::generate();
    let provider_id = ProviderId::generate();
    let key_id = UpstreamKeyId::generate();
    let fp = "sha256:abc123canonicalfingerprint";

    let mut share = ShareConfig {
        share_id: ShareId::generate(),
        grantor_user_id: alice,
        grantee_user_id: bob,
        provider_id,
        upstream_key_id: Some(key_id),
        allowed_models: Some(vec!["deepseek-v4.1-flash".to_string()]),
        max_monthly_micro_usd: Some(5_000_000), // $5.00
        accepted_config_fingerprint: None,      // Not accepted yet
        admin_approved_at: None,
        enabled: true,
    };

    // 1. Ineligible before Bob accepts
    assert_eq!(
        is_share_eligible(
            &share,
            ProviderKind::Custom,
            fp,
            false,
            Some("deepseek-v4.1-flash")
        ),
        Err(IneligibleReason::FingerprintMismatch)
    );

    // 2. Bob accepts fingerprint, but provider is Custom and admin hasn't approved
    share.accepted_config_fingerprint = Some(fp.to_string());
    assert_eq!(
        is_share_eligible(
            &share,
            ProviderKind::Custom,
            fp,
            false,
            Some("deepseek-v4.1-flash")
        ),
        Err(IneligibleReason::CustomProviderNeedsAdminApproval)
    );

    // For a preset provider (e.g. OpenRouter), admin approval is not required:
    assert!(
        is_share_eligible(
            &share,
            ProviderKind::Openrouter,
            fp,
            false,
            Some("deepseek-v4.1-flash")
        )
        .is_ok()
    );

    // 3. Admin approves custom provider -> now fully eligible!
    share.admin_approved_at = Some(1_000_000);
    assert!(
        is_share_eligible(
            &share,
            ProviderKind::Custom,
            fp,
            false,
            Some("deepseek-v4.1-flash")
        )
        .is_ok()
    );

    // 4. Modifying provider config changes fingerprint -> immediately suspends share
    let new_fp = "sha256:xyz999modifiedfingerprint";
    assert_eq!(
        is_share_eligible(
            &share,
            ProviderKind::Custom,
            new_fp,
            false,
            Some("deepseek-v4.1-flash")
        ),
        Err(IneligibleReason::FingerprintMismatch)
    );

    // 5. Disabling grantor makes share immediately ineligible
    assert_eq!(
        is_share_eligible(
            &share,
            ProviderKind::Custom,
            fp,
            true,
            Some("deepseek-v4.1-flash")
        ),
        Err(IneligibleReason::GrantorDisabled)
    );

    // 6. Requesting unshared model returns ModelNotAllowed
    assert_eq!(
        is_share_eligible(
            &share,
            ProviderKind::Custom,
            fp,
            false,
            Some("unshared-model")
        ),
        Err(IneligibleReason::ModelNotAllowed)
    );
}

#[doc = "Spec: SHR-1, SHR-2, SHR-3, SHR-4, USR-2, USR-3, USR-4"]
#[test]
fn test_share_overlap_prevention() {
    let alice = UserId::generate();
    let bob = UserId::generate();
    let provider_id = ProviderId::generate();
    let key1 = UpstreamKeyId::generate();
    let key2 = UpstreamKeyId::generate();

    // Existing provider-level share
    let existing_provider_share = ShareConfig {
        share_id: ShareId::generate(),
        grantor_user_id: alice,
        grantee_user_id: bob,
        provider_id,
        upstream_key_id: None, // provider-level
        allowed_models: None,
        max_monthly_micro_usd: None,
        accepted_config_fingerprint: Some("fp".to_string()),
        admin_approved_at: None,
        enabled: true,
    };

    // Attempting to add a key-level share when provider-level share exists must fail
    assert_eq!(
        check_share_overlap(
            std::slice::from_ref(&existing_provider_share),
            &alice,
            &bob,
            &provider_id,
            Some(&key1)
        ),
        Err(ShareOverlapError)
    );

    // Existing key-level share
    let existing_key_share = ShareConfig {
        share_id: ShareId::generate(),
        grantor_user_id: alice,
        grantee_user_id: bob,
        provider_id,
        upstream_key_id: Some(key1),
        allowed_models: None,
        max_monthly_micro_usd: None,
        accepted_config_fingerprint: Some("fp".to_string()),
        admin_approved_at: None,
        enabled: true,
    };

    // Attempting to add provider-level share when key share exists must fail
    assert_eq!(
        check_share_overlap(
            std::slice::from_ref(&existing_key_share),
            &alice,
            &bob,
            &provider_id,
            None
        ),
        Err(ShareOverlapError)
    );

    // Attempting to add duplicate key-1 share must fail
    assert_eq!(
        check_share_overlap(
            std::slice::from_ref(&existing_key_share),
            &alice,
            &bob,
            &provider_id,
            Some(&key1)
        ),
        Err(ShareOverlapError)
    );

    // Adding share for distinct key-2 is allowed
    assert!(
        check_share_overlap(
            &[existing_key_share],
            &alice,
            &bob,
            &provider_id,
            Some(&key2)
        )
        .is_ok()
    );
}

#[doc = "Spec: SHR-1, SHR-2, SHR-3, SHR-4, USR-2, USR-3, USR-4"]
#[test]
fn test_monthly_cap_reservation_on_shares() {
    let registry = MonthlyAccumulatorRegistry::new();
    let share_id = ShareId::generate();
    let scope_id = BudgetScopeId::ProviderShare(share_id);

    let ts = 1_700_000_000_000;
    let cap = 5_000_000; // $5.00
    let est = 100_000; // $0.10

    // Reserve 50 times $0.10 = $5.00
    let mut guards = Vec::new();
    for _ in 0..50 {
        let guard = registry
            .try_reserve(scope_id.clone(), Some(cap), est, ts)
            .expect("should reserve up to cap");
        guards.push(guard);
    }

    // Attempting 51st reservation must be rejected
    let err = registry.try_reserve(scope_id.clone(), Some(cap), est, ts);
    assert!(err.is_err(), "cannot exceed $5.00 monthly cap");

    // Commit all guards
    for g in guards {
        g.commit(est);
    }

    let month = crate::accumulators::UtcYearMonth::from_epoch_millis(ts);
    let acc = registry.get_or_create(month, scope_id);
    assert_eq!(acc.used(), 5_000_000);
    assert_eq!(acc.reserved(), 0);
}

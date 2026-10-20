//! Database repository for provider_shares with overlap checks and revocation.
//!
//! Spec: Section 4.4, Section 4.6, Section 6.4, Section 8.2, Section 10.1,
//! Requirements: SHR-1, SHR-2, SHR-3, SHR-4, SHR-5, USR-2, USR-3, USR-4, SEC-17.

use sqlx::{Row as _, Sqlite, Transaction};

use llmroller_core::ids::{ProviderId, ShareId, UpstreamKeyId, UserId};
use llmroller_core::shares::ShareConfig;

/// Error returned by share repository operations.
#[derive(Debug, thiserror::Error)]
pub enum ShareError {
    #[error("share not found")]
    NotFound,
    #[error("forbidden")]
    Forbidden,
    #[error("share overlap")]
    ShareOverlap,
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
}

/// Parameters for creating a personal provider or key share.
#[derive(Debug, Clone)]
pub struct NewShare {
    pub provider_id: ProviderId,
    pub upstream_key_id: Option<UpstreamKeyId>,
    pub grantee_user_id: UserId,
    pub allowed_models: Option<Vec<String>>,
    pub max_monthly_micro_usd: Option<i64>,
}

/// Creates a new provider share with overlap checks (SHR-1).
pub async fn create_provider_share_tx(
    tx: &mut Transaction<'_, Sqlite>,
    owner_user_id: &UserId,
    new_share: NewShare,
    now_ms: i64,
) -> Result<ShareConfig, ShareError> {
    let owner_str = owner_user_id.to_string();
    let grantee_str = new_share.grantee_user_id.to_string();
    let provider_str = new_share.provider_id.to_string();
    let key_str = new_share.upstream_key_id.as_ref().map(|k| k.to_string());

    // 1. Check for overlapping shares (SHR-1)
    if new_share.upstream_key_id.is_none() {
        // New is provider-level: check if any active key-level share exists for this provider & grantee
        let key_shares_count: i64 = sqlx::query_scalar(
            r#"SELECT COUNT(*) FROM provider_shares
               WHERE grantee_user_id = ? AND provider_id = ? AND upstream_key_id IS NOT NULL AND revoked_at IS NULL"#,
        )
        .bind(&grantee_str)
        .bind(&provider_str)
        .fetch_one(&mut **tx)
        .await?;

        if key_shares_count > 0 {
            return Err(ShareError::ShareOverlap);
        }
    } else {
        // New is key-level: check if active provider-level share exists for this provider & grantee
        let provider_shares_count: i64 = sqlx::query_scalar(
            r#"SELECT COUNT(*) FROM provider_shares
               WHERE grantee_user_id = ? AND provider_id = ? AND upstream_key_id IS NULL AND revoked_at IS NULL"#,
        )
        .bind(&grantee_str)
        .bind(&provider_str)
        .fetch_one(&mut **tx)
        .await?;

        if provider_shares_count > 0 {
            return Err(ShareError::ShareOverlap);
        }
    }

    let share_id = ShareId::generate();
    let share_str = share_id.to_string();
    let allowed_models_json = new_share
        .allowed_models
        .as_ref()
        .map(|m| serde_json::to_string(m).unwrap_or_else(|_| "[]".to_string()));

    let insert_result = sqlx::query(
        r#"INSERT INTO provider_shares (
               id, provider_id, upstream_key_id, owner_user_id, grantee_user_id,
               allowed_models, max_monthly_micro_usd, enabled, created_at
           )
           VALUES (?, ?, ?, ?, ?, ?, ?, 1, ?)"#,
    )
    .bind(&share_str)
    .bind(&provider_str)
    .bind(&key_str)
    .bind(&owner_str)
    .bind(&grantee_str)
    .bind(&allowed_models_json)
    .bind(new_share.max_monthly_micro_usd)
    .bind(now_ms)
    .execute(&mut **tx)
    .await;

    match insert_result {
        Ok(_) => Ok(ShareConfig {
            share_id,
            grantor_user_id: *owner_user_id,
            grantee_user_id: new_share.grantee_user_id,
            provider_id: new_share.provider_id,
            upstream_key_id: new_share.upstream_key_id,
            allowed_models: new_share.allowed_models,
            max_monthly_micro_usd: new_share.max_monthly_micro_usd,
            accepted_config_fingerprint: None,
            admin_approved_at: None,
            enabled: true,
        }),
        Err(e) => {
            // Check SQLite unique constraint violation
            if let Some(db_err) = e.as_database_error()
                && db_err.is_unique_violation()
            {
                return Err(ShareError::ShareOverlap);
            }
            Err(ShareError::Database(e))
        }
    }
}

/// Grantee accepts share by echoing current provider fingerprint (SHR-2).
pub async fn accept_provider_share_tx(
    tx: &mut Transaction<'_, Sqlite>,
    share_id: &ShareId,
    grantee_user_id: &UserId,
    fingerprint: &str,
    now_ms: i64,
) -> Result<(), ShareError> {
    let share_str = share_id.to_string();
    let grantee_str = grantee_user_id.to_string();

    let rows_affected = sqlx::query(
        r#"UPDATE provider_shares SET accepted_at = ?, accepted_config_fingerprint = ?
           WHERE id = ? AND grantee_user_id = ? AND revoked_at IS NULL"#,
    )
    .bind(now_ms)
    .bind(fingerprint)
    .bind(&share_str)
    .bind(&grantee_str)
    .execute(&mut **tx)
    .await?
    .rows_affected();

    if rows_affected == 0 {
        return Err(ShareError::NotFound);
    }
    Ok(())
}

/// Admin approves a share of a custom provider (SHR-2).
pub async fn approve_provider_share_tx(
    tx: &mut Transaction<'_, Sqlite>,
    share_id: &ShareId,
    now_ms: i64,
) -> Result<(), ShareError> {
    let share_str = share_id.to_string();
    let rows_affected = sqlx::query(
        r#"UPDATE provider_shares SET admin_approved_at = ? WHERE id = ? AND revoked_at IS NULL"#,
    )
    .bind(now_ms)
    .bind(&share_str)
    .execute(&mut **tx)
    .await?
    .rows_affected();

    if rows_affected == 0 {
        return Err(ShareError::NotFound);
    }
    Ok(())
}

/// Revokes a provider share (SHR-5).
pub async fn revoke_provider_share_tx(
    tx: &mut Transaction<'_, Sqlite>,
    share_id: &ShareId,
    owner_user_id: &UserId,
    now_ms: i64,
) -> Result<(), ShareError> {
    let share_str = share_id.to_string();
    let owner_str = owner_user_id.to_string();

    let rows_affected = sqlx::query(
        r#"UPDATE provider_shares SET revoked_at = ?, enabled = 0 WHERE id = ? AND owner_user_id = ?"#,
    )
    .bind(now_ms)
    .bind(&share_str)
    .bind(&owner_str)
    .execute(&mut **tx)
    .await?
    .rows_affected();

    if rows_affected == 0 {
        return Err(ShareError::NotFound);
    }
    Ok(())
}

/// Loads a share configuration from the database.
pub async fn get_provider_share_tx(
    tx: &mut Transaction<'_, Sqlite>,
    share_id: &ShareId,
) -> Result<ShareConfig, ShareError> {
    let share_str = share_id.to_string();
    let row = sqlx::query(
        r#"SELECT id, provider_id, upstream_key_id, owner_user_id, grantee_user_id,
                  allowed_models, max_monthly_micro_usd, accepted_config_fingerprint,
                  admin_approved_at, enabled
           FROM provider_shares WHERE id = ?"#,
    )
    .bind(&share_str)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(ShareError::NotFound)?;

    let id_str: String = row.try_get("id")?;
    let provider_str: String = row.try_get("provider_id")?;
    let key_str: Option<String> = row.try_get("upstream_key_id")?;
    let owner_str: String = row.try_get("owner_user_id")?;
    let grantee_str: String = row.try_get("grantee_user_id")?;
    let allowed_models_json: Option<String> = row.try_get("allowed_models")?;
    let max_monthly_micro_usd: Option<i64> = row.try_get("max_monthly_micro_usd")?;
    let accepted_config_fingerprint: Option<String> = row.try_get("accepted_config_fingerprint")?;
    let admin_approved_at: Option<i64> = row.try_get("admin_approved_at")?;
    let enabled_int: i64 = row.try_get("enabled")?;

    let allowed_models = allowed_models_json.and_then(|j| serde_json::from_str(&j).ok());

    Ok(ShareConfig {
        share_id: ShareId::parse(&id_str).map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
        grantor_user_id: UserId::parse(&owner_str).map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
        grantee_user_id: UserId::parse(&grantee_str)
            .map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
        provider_id: ProviderId::parse(&provider_str)
            .map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
        upstream_key_id: key_str
            .as_deref()
            .map(UpstreamKeyId::parse)
            .transpose()
            .map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
        allowed_models,
        max_monthly_micro_usd,
        accepted_config_fingerprint,
        admin_approved_at,
        enabled: enabled_int == 1,
    })
}

//! Database repositories for response_links and persistent pins.
//!
//! Spec: Section 6.7, Section 10.4, Section 11.1.2,
//! Requirements: RTE-9, RTE-12, PRV-8.

use sqlx::{Row as _, Sqlite, Transaction};

use llmroller_core::ids::{ClientKeyId, ModelPk, UpstreamKeyId, UserId};

/// Durable response link linking response_id to parent upstream_key and session_hash (Section 11.1.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseLinkRow {
    pub response_id: String,
    pub owner_user_id: UserId,
    pub client_key_id: ClientKeyId,
    pub upstream_key_id: UpstreamKeyId,
    pub model_pk: ModelPk,
    pub session_hash: String,
    pub status: String,
    pub store_flag: bool,
    pub created_at: i64,
    pub updated_at: i64,
    pub expires_at: i64,
}

/// Inserts a durable response link (RTE-9, RTE-12).
pub async fn insert_response_link_tx(
    tx: &mut Transaction<'_, Sqlite>,
    link: &ResponseLinkRow,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"INSERT INTO response_links (
               response_id, owner_user_id, client_key_id, upstream_key_id,
               model_pk, session_hash, protocol, status, store_flag,
               created_at, updated_at, expires_at
           )
           VALUES (?, ?, ?, ?, ?, ?, 'responses', ?, ?, ?, ?, ?)
           ON CONFLICT (response_id) DO UPDATE SET
               upstream_key_id = excluded.upstream_key_id,
               session_hash = excluded.session_hash,
               status = excluded.status,
               updated_at = excluded.updated_at,
               expires_at = excluded.expires_at"#,
    )
    .bind(&link.response_id)
    .bind(link.owner_user_id.to_string())
    .bind(link.client_key_id.to_string())
    .bind(link.upstream_key_id.to_string())
    .bind(link.model_pk.to_string())
    .bind(&link.session_hash)
    .bind(&link.status)
    .bind(if link.store_flag { 1 } else { 0 })
    .bind(link.created_at)
    .bind(link.updated_at)
    .bind(link.expires_at)
    .execute(&mut **tx)
    .await?;

    Ok(())
}

/// Retrieves a response link by response_id.
pub async fn get_response_link_tx(
    tx: &mut Transaction<'_, Sqlite>,
    response_id: &str,
) -> Result<Option<ResponseLinkRow>, sqlx::Error> {
    let row = sqlx::query(
        r#"SELECT response_id, owner_user_id, client_key_id, upstream_key_id,
                  model_pk, session_hash, status, store_flag, created_at, updated_at, expires_at
           FROM response_links WHERE response_id = ?"#,
    )
    .bind(response_id)
    .fetch_optional(&mut **tx)
    .await?;

    match row {
        Some(r) => {
            let key_str: String = r.try_get("upstream_key_id")?;
            let model_str: String = r.try_get("model_pk")?;
            let user_str: String = r.try_get("owner_user_id")?;
            let client_str: String = r.try_get("client_key_id")?;
            let flag: i64 = r.try_get("store_flag")?;
            Ok(Some(ResponseLinkRow {
                response_id: r.try_get("response_id")?,
                owner_user_id: UserId::parse(&user_str)
                    .map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
                client_key_id: ClientKeyId::parse(&client_str)
                    .map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
                upstream_key_id: UpstreamKeyId::parse(&key_str)
                    .map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
                model_pk: ModelPk::parse(&model_str)
                    .map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
                session_hash: r.try_get("session_hash")?,
                status: r.try_get("status")?,
                store_flag: flag == 1,
                created_at: r.try_get("created_at")?,
                updated_at: r.try_get("updated_at")?,
                expires_at: r.try_get("expires_at")?,
            }))
        }
        None => Ok(None),
    }
}

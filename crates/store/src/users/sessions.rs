use std::str::FromStr;

use llmroller_core::ids::UserId;
use llmroller_core::rbac::ActorScope;
use sqlx::{Row, SqlitePool, Transaction};

use super::crud::get_user_by_id;
use super::types::{
    NewWebSession, SessionWithUser, UserError, WebSession, ensure_self_or_admin,
};

/// Insert a newly issued web session.
///
/// # Errors
///
/// Returns SQLite failures.
pub async fn insert_web_session(
    pool: &SqlitePool,
    session: NewWebSession<'_>,
) -> Result<(), UserError> {
    sqlx::query(
        "INSERT INTO web_sessions
            (id_hash, user_id, csrf_hash, ip_addr, user_agent, created_at, last_seen_at, expires_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(session.id_hash)
    .bind(session.user_id.to_string())
    .bind(session.csrf_hash)
    .bind(session.ip_addr)
    .bind(session.user_agent)
    .bind(session.created_at)
    .bind(session.created_at)
    .bind(session.expires_at)
    .execute(pool)
    .await?;
    Ok(())
}

/// Load a session and its owning account.
///
/// # Errors
///
/// Returns corruption or SQLite failures.
pub async fn load_web_session(
    pool: &SqlitePool,
    id_hash: &str,
) -> Result<Option<SessionWithUser>, UserError> {
    let session = sqlx::query(
        "SELECT id_hash, user_id, csrf_hash, ip_addr, user_agent,
                created_at, last_seen_at, expires_at
         FROM web_sessions WHERE id_hash = ?",
    )
    .bind(id_hash)
    .fetch_optional(pool)
    .await?;
    let Some(session) = session else {
        return Ok(None);
    };
    let user_id: String = session.try_get("user_id")?;
    let user_id = UserId::from_str(&user_id).map_err(|_| UserError::Corrupt)?;
    let user = get_user_by_id(pool, user_id).await?;
    Ok(Some(SessionWithUser {
        session: WebSession {
            id_hash: session.try_get("id_hash")?,
            user_id,
            csrf_hash: session.try_get("csrf_hash")?,
            ip_addr: session.try_get("ip_addr")?,
            user_agent: session.try_get("user_agent")?,
            created_at: session.try_get("created_at")?,
            last_seen_at: session.try_get("last_seen_at")?,
            expires_at: session.try_get("expires_at")?,
        },
        user,
    }))
}

/// Update the last-seen timestamp for an active session.
///
/// # Errors
///
/// Returns SQLite failures.
pub async fn touch_web_session(
    pool: &SqlitePool,
    id_hash: &str,
    now_ms: i64,
) -> Result<(), UserError> {
    sqlx::query("UPDATE web_sessions SET last_seen_at = ? WHERE id_hash = ?")
        .bind(now_ms)
        .bind(id_hash)
        .execute(pool)
        .await?;
    Ok(())
}

/// Delete one session by its hashed identifier.
///
/// # Errors
///
/// Returns SQLite failures.
pub async fn delete_web_session(pool: &SqlitePool, id_hash: &str) -> Result<(), UserError> {
    sqlx::query("DELETE FROM web_sessions WHERE id_hash = ?")
        .bind(id_hash)
        .execute(pool)
        .await?;
    Ok(())
}

/// Delete all sessions for an account, optionally preserving one.
///
/// # Errors
///
/// Returns SQLite failures.
pub async fn delete_user_web_sessions(
    pool: &SqlitePool,
    user_id: UserId,
    keep: Option<&str>,
) -> Result<(), UserError> {
    if let Some(keep) = keep {
        sqlx::query("DELETE FROM web_sessions WHERE user_id = ? AND id_hash != ?")
            .bind(user_id.to_string())
            .bind(keep)
            .execute(pool)
            .await?;
    } else {
        sqlx::query("DELETE FROM web_sessions WHERE user_id = ?")
            .bind(user_id.to_string())
            .execute(pool)
            .await?;
    }
    Ok(())
}

/// Delete one session belonging to a user, enforcing self-or-admin access.
///
/// # Errors
///
/// Returns permission or SQLite failures.
pub async fn delete_owned_web_session(
    pool: &SqlitePool,
    actor: &ActorScope,
    id_hash: &str,
) -> Result<(), UserError> {
    let mut tx = pool.begin().await?;
    delete_owned_web_session_tx(&mut tx, actor, id_hash).await?;
    tx.commit().await?;
    Ok(())
}

/// Delete one owned session inside an active transaction.
pub async fn delete_owned_web_session_tx(
    tx: &mut Transaction<'_, sqlx::Sqlite>,
    actor: &ActorScope,
    id_hash: &str,
) -> Result<(), UserError> {
    let user_id: String = sqlx::query_scalar("SELECT user_id FROM web_sessions WHERE id_hash = ?")
        .bind(id_hash)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(UserError::NotFound)?;
    let user_id = UserId::from_str(&user_id).map_err(|_| UserError::Corrupt)?;
    ensure_self_or_admin(actor, user_id)?;
    sqlx::query("DELETE FROM web_sessions WHERE id_hash = ?")
        .bind(id_hash)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

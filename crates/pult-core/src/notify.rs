//! Уведомления в портале (запись, список, «прочитано», счётчик).
//! Доставка — SSE (`events`), резервный опрос — раз в минуту.

use crate::rbac::ROLE_SUPERADMIN;
use serde::Serialize;
use sqlx::PgExecutor;
use uuid::Uuid;

/// Виды уведомлений ядра.
pub mod kind {
    pub const AD_SYNC_FAILED: &str = "ad.sync_failed";
    pub const AUDIT_CHAIN_BROKEN: &str = "audit.chain_broken";
    pub const LINK_CANDIDATES: &str = "people.link_candidates";
    pub const ROLE_GRANTED: &str = "core.role_granted";
}

pub async fn notify(
    db: impl PgExecutor<'_>,
    user_id: Uuid,
    kind: &str,
    payload: serde_json::Value,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "WITH ins AS (INSERT INTO notifications (user_id, kind, payload) VALUES ($1, $2, $3) RETURNING user_id)
         SELECT pg_notify('pult_events', json_build_object('kind', 'notification', 'users', json_agg(user_id))::text) FROM ins",
    )
        .bind(user_id)
        .bind(kind)
        .bind(payload)
        .execute(db)
        .await?;
    Ok(())
}

/// Всем активным суперадминистраторам. Повтор того же уведомления, пока прежнее не прочитано,
/// не создаётся — иначе сбой синхронизации каждые 5 минут завалит колокольчик.
pub async fn notify_superadmins(
    db: impl PgExecutor<'_>,
    kind: &str,
    payload: serde_json::Value,
) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query(
        "WITH ins AS (INSERT INTO notifications (user_id, kind, payload)
         SELECT DISTINCT u.id, $2, $3
           FROM users u
           JOIN user_roles ur ON ur.user_id = u.id
           JOIN roles r ON r.id = ur.role_id AND r.code = $1
          WHERE u.is_active
            AND NOT EXISTS (SELECT 1 FROM notifications n
                             WHERE n.user_id = u.id AND n.kind = $2 AND n.read_at IS NULL)
         RETURNING user_id)
         SELECT pg_notify('pult_events', json_build_object('kind', 'notification', 'users', json_agg(user_id))::text)
           FROM ins HAVING count(*) > 0",
    )
    .bind(ROLE_SUPERADMIN)
    .bind(kind)
    .bind(payload)
    .execute(db)
    .await?
    .rows_affected())
}

#[derive(Debug, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct Notification {
    pub id: Uuid,
    pub kind: String,
    #[schema(value_type = Object)]
    pub payload: serde_json::Value,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: time::OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub read_at: Option<time::OffsetDateTime>,
}

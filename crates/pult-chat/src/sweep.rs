//! Учётка отключена в AD: сотрудник выбывает из групповых чатов (переписка сохраняется, личные диалоги
//! остаются как есть). Если он был владельцем, владение переходит администратору чата с самым давним
//! назначением и действующей учёткой — уведомление только ему. Если такого нет, чат блокируется до
//! назначения владельца суперадминистратором, суперадминистраторы получают уведомление.
//! После включения учётки членство само не восстанавливается.

use crate::kind;
use pult_core::{
    audit::{self, Actor, Entry, Source},
    notify,
};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

/// Фоновая задача: проверка раз в 5 минут (и сразу после синхронизации AD по расписанию).
pub const SWEEP_JOB: &str = "chat.blocked_sweep";

/// Сколько участий закрыто.
pub async fn sweep_blocked(db: &PgPool) -> Result<u64, sqlx::Error> {
    let rows: Vec<(Uuid, Uuid, String)> = sqlx::query_as(
        "SELECT m.chat_id, m.user_id, m.role
           FROM chat_members m
           JOIN chats c ON c.id = m.chat_id AND c.kind = 'group'
           JOIN users u ON u.id = m.user_id AND NOT u.is_active
          WHERE m.left_at IS NULL
          ORDER BY m.chat_id",
    )
    .fetch_all(db)
    .await?;
    let mut n = 0;
    for (chat, user, role) in rows {
        let mut tx = db.begin().await?;
        let done = sqlx::query(
            "UPDATE chat_members SET left_at = now(), left_reason = 'blocked', role = 'member', admin_since = NULL
              WHERE chat_id = $1 AND user_id = $2 AND left_at IS NULL",
        )
        .bind(chat)
        .bind(user)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if done == 0 {
            continue;
        }
        n += 1;
        sqlx::query("INSERT INTO chat_member_events (chat_id, user_id, action) VALUES ($1, $2, 'blocked')")
            .bind(chat)
            .bind(user)
            .execute(&mut *tx)
            .await?;
        let title: Option<String> = sqlx::query_scalar("SELECT title FROM chats WHERE id = $1 FOR UPDATE")
            .bind(chat)
            .fetch_one(&mut *tx)
            .await?;
        let mut details = json!({"user": user, "was": role});
        if role == "owner" {
            let heir: Option<Uuid> = sqlx::query_scalar(
                "SELECT m.user_id FROM chat_members m JOIN users u ON u.id = m.user_id AND u.is_active
                  WHERE m.chat_id = $1 AND m.role = 'admin' AND m.left_at IS NULL
                  ORDER BY m.admin_since NULLS LAST, m.joined_at LIMIT 1",
            )
            .bind(chat)
            .fetch_optional(&mut *tx)
            .await?;
            if let Some(h) = heir {
                sqlx::query("UPDATE chat_members SET role = 'owner' WHERE chat_id = $1 AND user_id = $2")
                    .bind(chat)
                    .bind(h)
                    .execute(&mut *tx)
                    .await?;
                sqlx::query("INSERT INTO chat_member_events (chat_id, user_id, action) VALUES ($1, $2, 'owner')")
                    .bind(chat)
                    .bind(h)
                    .execute(&mut *tx)
                    .await?;
                notify::notify(&mut *tx, h, kind::OWNER, json!({"chat": chat, "title": title})).await?;
                details["new_owner"] = json!(h);
            } else {
                sqlx::query("UPDATE chats SET locked = true WHERE id = $1").bind(chat).execute(&mut *tx).await?;
                sqlx::query("INSERT INTO chat_member_events (chat_id, user_id, action) VALUES ($1, $2, 'locked')")
                    .bind(chat)
                    .bind(user)
                    .execute(&mut *tx)
                    .await?;
                notify::notify_superadmins(&mut *tx, kind::LOCKED, json!({"chat": chat, "title": title})).await?;
                details["locked"] = json!(true);
            }
        }
        audit::record(
            &mut *tx,
            &Actor::system(),
            Entry::new("chat.member.blocked", Source::Job).object("chat", chat).details(details),
        )
        .await?;
        crate::api::changed(&mut tx, chat, &[]).await?;
        tx.commit().await?;
    }
    if n > 0 {
        tracing::info!(count = n, "участники с отключённой учёткой выбыли из групповых чатов");
    }
    Ok(n)
}

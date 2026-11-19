//! Задачи внутри заявки (чек-лист): у пункта свой исполнитель, обязательные пункты
//! не дают решить заявку. Пункт отмечает его исполнитель; пункт без исполнителя — любой инженер.

use crate::{
    assignment, notify,
    tickets::{T, Who, audit_rec, bad_state, event, is_open, lock, need_engineer, ping, start_work},
};
use pult_core::{ApiError, ApiResult, Core};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Transaction};
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

const MAX_ITEMS: i64 = 100;

#[derive(Debug, Clone, Serialize, sqlx::FromRow, ToSchema)]
pub struct ChecklistItem {
    pub id: Uuid,
    pub position: i32,
    pub title: String,
    pub assignee_id: Option<Uuid>,
    pub assignee_name: Option<String>,
    pub is_required: bool,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub done_at: Option<OffsetDateTime>,
    pub done_by_name: Option<String>,
    /// Выполняется через каталог AD: `directory.enable`, `directory.disable`, `directory.add_member`.
    pub action: Option<String>,
}

/// Пункт для добавления: вручную или из шаблона.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct NewItem {
    pub title: String,
    pub assignee_id: Option<Uuid>,
    #[serde(default = "yes")]
    pub is_required: bool,
    /// Действие каталога AD (из шаблона).
    #[serde(default)]
    pub action: Option<String>,
}

/// Действия пунктов чек-листа, которые выполняет каталог AD.
pub const ACTIONS: &[&str] = &["directory.enable", "directory.disable", "directory.add_member"];

fn yes() -> bool {
    true
}

pub async fn items(db: &PgPool, ticket: Uuid) -> Result<Vec<ChecklistItem>, sqlx::Error> {
    sqlx::query_as(
        "SELECT i.id, i.position, i.title, i.assignee_id, i.is_required, i.done_at, i.action,
                (SELECT coalesce(nullif(trim(concat_ws(' ', p.last_name, p.first_name)), ''), a.display_name, a.sam_account_name)
                   FROM users u JOIN ad_accounts a ON a.id = u.ad_account_id
                   LEFT JOIN persons p ON p.id = u.person_id WHERE u.id = i.assignee_id) AS assignee_name,
                (SELECT coalesce(nullif(trim(concat_ws(' ', p.last_name, p.first_name)), ''), a.display_name, a.sam_account_name)
                   FROM users u JOIN ad_accounts a ON a.id = u.ad_account_id
                   LEFT JOIN persons p ON p.id = u.person_id WHERE u.id = i.done_by) AS done_by_name
           FROM ticket_checklist_items i WHERE i.ticket_id = $1 ORDER BY i.position, i.created_at",
    )
    .bind(ticket)
    .fetch_all(db)
    .await
}

/// Нельзя решить, пока открыты обязательные пункты.
pub async fn ensure_done(tx: &mut Transaction<'_, Postgres>, t: &T) -> ApiResult<()> {
    let open: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ticket_checklist_items WHERE ticket_id = $1 AND is_required AND done_at IS NULL",
    )
    .bind(t.id)
    .fetch_one(&mut **tx)
    .await?;
    if open > 0 {
        return Err(ApiError::bad_request(
            "checklist_open",
            format!("Не выполнено обязательных пунктов чек-листа: {open}."),
        ));
    }
    Ok(())
}

async fn check_assignee(tx: &mut Transaction<'_, Postgres>, user: Option<Uuid>) -> ApiResult<()> {
    if let Some(u) = user
        && (!assignment::is_engineer(tx, u).await? || !assignment::is_active(tx, u).await?)
    {
        return Err(ApiError::bad_request("invalid_item", "Исполнитель пункта — активный инженер."));
    }
    Ok(())
}

/// Добавить пункты в конец (из шаблона или вручную). Исполнители получают колокольчик.
/// Шаблон мог устареть (инженера отключили): такой пункт остаётся без исполнителя, заявка создаётся.
pub async fn insert(
    tx: &mut Transaction<'_, Postgres>,
    t: &T,
    who: &Who,
    list: &[NewItem],
    from_template: bool,
) -> ApiResult<()> {
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM ticket_checklist_items WHERE ticket_id = $1")
        .bind(t.id)
        .fetch_one(&mut **tx)
        .await?;
    if count + list.len() as i64 > MAX_ITEMS {
        return Err(ApiError::bad_request("invalid_item", "В чек-листе не больше 100 пунктов."));
    }
    let mut pos: i32 =
        sqlx::query_scalar("SELECT coalesce(max(position), 0) FROM ticket_checklist_items WHERE ticket_id = $1")
            .bind(t.id)
            .fetch_one(&mut **tx)
            .await?;
    let mut notified = Vec::new();
    for it in list {
        let title: String = it.title.trim().chars().take(300).collect();
        if title.is_empty() {
            return Err(ApiError::bad_request("invalid_item", "Пункт без текста."));
        }
        let assignee = match check_assignee(tx, it.assignee_id).await {
            Ok(()) => it.assignee_id,
            Err(_) if from_template => {
                tracing::warn!(
                    ticket = t.number,
                    "исполнитель пункта шаблона — не активный инженер, пункт без исполнителя"
                );
                None
            }
            Err(e) => return Err(e),
        };
        if it.action.as_deref().is_some_and(|a| !ACTIONS.contains(&a)) {
            return Err(ApiError::bad_request("invalid_item", "Неизвестное действие пункта."));
        }
        pos += 1;
        sqlx::query(
            "INSERT INTO ticket_checklist_items (ticket_id, position, title, assignee_id, is_required, action)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(t.id)
        .bind(pos)
        .bind(&title)
        .bind(assignee)
        .bind(it.is_required)
        .bind(&it.action)
        .execute(&mut **tx)
        .await?;
        if let Some(u) = assignee.filter(|u| who.user != Some(*u) && !notified.contains(u)) {
            notified.push(u);
            notify::bell(tx, u, "helpdesk.task_assigned", t).await?;
        }
    }
    Ok(())
}

pub async fn add(core: &Core, who: &Who, number: i64, list: &[NewItem]) -> ApiResult<()> {
    need_engineer(who)?;
    let mut tx = core.db.begin().await?;
    let t = lock(&mut tx, number, who).await?;
    if !is_open(&t.status) {
        return Err(bad_state("Заявка уже решена или закрыта."));
    }
    insert(&mut tx, &t, who, list, false).await?;
    event(&mut tx, t.id, who.user, "checklist_added", Value::Null, json!(list.len())).await?;
    audit_rec(&mut tx, who, "ticket.checklist.add", &t, json!({"items": list.len()})).await?;
    ping(&mut tx, &t).await?;
    tx.commit().await?;
    Ok(())
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ItemUpdate {
    pub done: Option<bool>,
    pub title: Option<String>,
    /// Сменить исполнителя пункта; `clear_assignee` — убрать.
    pub assignee_id: Option<Uuid>,
    #[serde(default)]
    pub clear_assignee: bool,
    pub is_required: Option<bool>,
}

#[derive(sqlx::FromRow)]
struct Row {
    title: String,
    assignee_id: Option<Uuid>,
    done_at: Option<OffsetDateTime>,
}

/// Отметить или изменить пункт.
pub async fn update(core: &Core, who: &Who, number: i64, item: Uuid, b: ItemUpdate) -> ApiResult<()> {
    need_engineer(who)?;
    let mut tx = core.db.begin().await?;
    let mut t = lock(&mut tx, number, who).await?;
    if !is_open(&t.status) {
        return Err(bad_state("Заявка уже решена или закрыта."));
    }
    let row: Row = sqlx::query_as(
        "SELECT title, assignee_id, done_at FROM ticket_checklist_items WHERE id = $1 AND ticket_id = $2 FOR UPDATE",
    )
    .bind(item)
    .bind(t.id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(ApiError::NotFound)?;
    if let Some(done) = b.done
        && done != row.done_at.is_some()
    {
        // Пункт отмечает его исполнитель, исполнитель заявки или суперадминистратор.
        let mine = row.assignee_id.is_none_or(|a| who.user == Some(a))
            || (who.user.is_some() && who.user == t.assignee_id)
            || who.superadmin;
        if !mine {
            return Err(ApiError::bad_request("not_your_item", "Пункт отмечает назначенный на него инженер."));
        }
        sqlx::query(
            "UPDATE ticket_checklist_items SET done_at = CASE WHEN $2 THEN now() END, done_by = CASE WHEN $2 THEN $3::uuid END
              WHERE id = $1",
        )
        .bind(item)
        .bind(done)
        .bind(who.user)
        .execute(&mut *tx)
        .await?;
        let kind = if done { "checklist_done" } else { "checklist_undone" };
        event(&mut tx, t.id, who.user, kind, Value::Null, json!(row.title)).await?;
        start_work(&mut tx, &mut t, who).await?;
        if let Some(a) = t.assignee_id.filter(|a| who.user != Some(*a)) {
            notify::bell(&mut tx, a, "helpdesk.changed_by_other", &t).await?;
        }
    }
    let new_assignee = if b.clear_assignee { Some(None) } else { b.assignee_id.map(Some) };
    if let Some(a) = new_assignee.filter(|a| *a != row.assignee_id) {
        check_assignee(&mut tx, a).await?;
        sqlx::query("UPDATE ticket_checklist_items SET assignee_id = $2 WHERE id = $1")
            .bind(item)
            .bind(a)
            .execute(&mut *tx)
            .await?;
        if let Some(u) = a.filter(|u| who.user != Some(*u)) {
            notify::bell(&mut tx, u, "helpdesk.task_assigned", &t).await?;
        }
    }
    let title = b.title.map(|s| s.trim().chars().take(300).collect::<String>()).filter(|s| !s.is_empty());
    sqlx::query(
        "UPDATE ticket_checklist_items SET title = coalesce($2, title), is_required = coalesce($3, is_required) WHERE id = $1",
    )
    .bind(item)
    .bind(title)
    .bind(b.is_required)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE tickets SET version = version + 1 WHERE id = $1").bind(t.id).execute(&mut *tx).await?;
    audit_rec(&mut tx, who, "ticket.checklist.update", &t, json!({"item": item, "done": b.done})).await?;
    ping(&mut tx, &t).await?;
    tx.commit().await?;
    Ok(())
}

pub async fn remove(core: &Core, who: &Who, number: i64, item: Uuid) -> ApiResult<()> {
    need_engineer(who)?;
    let mut tx = core.db.begin().await?;
    let t = lock(&mut tx, number, who).await?;
    if !is_open(&t.status) {
        return Err(bad_state("Заявка уже решена или закрыта."));
    }
    let title: Option<String> =
        sqlx::query_scalar("DELETE FROM ticket_checklist_items WHERE id = $1 AND ticket_id = $2 RETURNING title")
            .bind(item)
            .bind(t.id)
            .fetch_optional(&mut *tx)
            .await?;
    let title = title.ok_or(ApiError::NotFound)?;
    event(&mut tx, t.id, who.user, "checklist_removed", json!(title), Value::Null).await?;
    audit_rec(&mut tx, who, "ticket.checklist.remove", &t, json!({"item": item})).await?;
    ping(&mut tx, &t).await?;
    tx.commit().await?;
    Ok(())
}

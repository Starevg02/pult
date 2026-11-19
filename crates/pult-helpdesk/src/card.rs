//! Карточка заявки: назначение исполнителя, «Отложена», наблюдатели, трудозатраты.

use crate::{
    assignment,
    notify::{self, Msg},
    tickets::{
        NewComment, T, Who, audit_rec, bad_state, event, insert_comment, is_open, lock, need_engineer, ping,
        set_assignee, set_status, start_work, tell_assignee,
    },
};
use pult_core::{ApiError, ApiResult, Core};
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::{Postgres, Transaction};
use utoipa::ToSchema;
use uuid::Uuid;

/// Сколько минут можно записать за раз: не больше рабочей недели.
const MAX_MINUTES: i32 = 60 * 40;

/// Наблюдатель — по сотруднику справочника. Учётки может и не быть: тогда только письма.
pub async fn is_watcher(tx: &mut Transaction<'_, Postgres>, ticket: Uuid, who: &Who) -> Result<bool, sqlx::Error> {
    let Some(person) = who.person else { return Ok(false) };
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM ticket_watchers WHERE ticket_id = $1 AND person_id = $2)")
        .bind(ticket)
        .bind(person)
        .fetch_one(&mut **tx)
        .await
}

/// Записать трудозатраты инженера (к ответу, заметке, решению).
pub async fn log_work(
    tx: &mut Transaction<'_, Postgres>,
    t: &T,
    who: &Who,
    minutes: Option<i32>,
    comment: Option<Uuid>,
) -> ApiResult<()> {
    let Some(m) = minutes.filter(|m| *m != 0) else { return Ok(()) };
    if !who.engineer {
        return Err(ApiError::Forbidden);
    }
    if !(1..=MAX_MINUTES).contains(&m) {
        return Err(ApiError::bad_request("invalid_minutes", "Время — от 1 минуты до 40 часов."));
    }
    sqlx::query("INSERT INTO ticket_worklogs (ticket_id, user_id, minutes, comment_id) VALUES ($1, $2, $3, $4)")
        .bind(t.id)
        .bind(who.user)
        .bind(m)
        .bind(comment)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Назначить исполнителя или вернуть заявку в очередь (`None`). Комментарий — по желанию,
/// внутренней заметкой. Линия не меняется: для L2 — «Передать».
pub async fn assign(core: &Core, who: &Who, number: i64, assignee: Option<Uuid>, note: Option<&str>) -> ApiResult<()> {
    need_engineer(who)?;
    let c = notify::ctx(core).await?;
    let mut tx = core.db.begin().await?;
    let mut t = lock(&mut tx, number, who).await?;
    if !is_open(&t.status) {
        return Err(bad_state("Заявка уже решена или закрыта."));
    }
    if let Some(n) = note.map(str::trim).filter(|n| !n.is_empty()) {
        insert_comment(&mut tx, t.id, who, &NewComment { body: n.to_string(), internal: true, ..Default::default() })
            .await?;
    }
    let previous = t.assignee_id;
    match assignee {
        Some(u) if Some(u) == previous => {}
        Some(u) => {
            if !assignment::is_engineer(&mut tx, u).await? || !assignment::is_active(&mut tx, u).await? {
                return Err(ApiError::bad_request("invalid_ticket", "Такого активного инженера нет."));
            }
            let status = if who.user == Some(u) { "in_progress" } else { "assigned" };
            set_assignee(&mut tx, &mut t, who, Some(u), status, &c).await?;
            if status == "in_progress" {
                start_work(&mut tx, &mut t, who).await?;
            }
        }
        None if previous.is_none() => {}
        None => {
            sqlx::query("UPDATE tickets SET assignee_id = NULL, version = version + 1 WHERE id = $1")
                .bind(t.id)
                .execute(&mut *tx)
                .await?;
            t.assignee_id = None;
            event(&mut tx, t.id, who.user, "assigned", json!(previous), Value::Null).await?;
            audit_rec(&mut tx, who, "ticket.assign", &t, json!({"before": previous, "after": null})).await?;
            set_status(&mut tx, &mut t, who, "new").await?;
        }
    }
    if let Some(p) = previous.filter(|p| assignee != Some(*p) && who.user != Some(*p)) {
        notify::bell(&mut tx, p, "helpdesk.changed_by_other", &t).await?;
    }
    ping(&mut tx, &t).await?;
    tx.commit().await?;
    Ok(())
}

/// «Отложена»: ждём поставки или подрядчика. Причина видна заявителю, срок SLA на паузе.
pub async fn hold(core: &Core, who: &Who, number: i64, body: &str, minutes: Option<i32>) -> ApiResult<()> {
    need_engineer(who)?;
    let body = body.trim();
    if body.is_empty() {
        return Err(ApiError::bad_request("comment_required", "Напишите, чего ждём: заявитель увидит это."));
    }
    let c = notify::ctx(core).await?;
    let mut tx = core.db.begin().await?;
    let mut t = lock(&mut tx, number, who).await?;
    if !is_open(&t.status) {
        return Err(bad_state("Заявка уже решена или закрыта."));
    }
    let id = insert_comment(&mut tx, t.id, who, &NewComment { body: body.to_string(), ..Default::default() }).await?;
    log_work(&mut tx, &t, who, minutes, Some(id)).await?;
    sqlx::query("UPDATE tickets SET first_response_at = coalesce(first_response_at, now()) WHERE id = $1")
        .bind(t.id)
        .execute(&mut *tx)
        .await?;
    start_work(&mut tx, &mut t, who).await?;
    set_status(&mut tx, &mut t, who, "on_hold").await?;
    notify::to_requester(&mut tx, &t, &Msg::OnHold { body }, &c).await?;
    tell_assignee(&mut tx, &t, who).await?;
    ping(&mut tx, &t).await?;
    tx.commit().await?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow, ToSchema)]
pub struct WatcherView {
    pub person_id: Uuid,
    pub name: String,
    pub email: Option<String>,
}

pub async fn watchers(db: &sqlx::PgPool, ticket: Uuid) -> Result<Vec<WatcherView>, sqlx::Error> {
    sqlx::query_as(
        "SELECT p.id AS person_id, trim(concat_ws(' ', p.last_name, p.first_name, p.middle_name)) AS name,
                nullif(p.email, '') AS email
           FROM ticket_watchers w JOIN persons p ON p.id = w.person_id
          WHERE w.ticket_id = $1 ORDER BY w.created_at",
    )
    .bind(ticket)
    .fetch_all(db)
    .await
}

/// Добавить наблюдателя: заявитель или инженер, из справочника.
pub async fn add_watcher(core: &Core, who: &Who, number: i64, person: Uuid) -> ApiResult<()> {
    let mut tx = core.db.begin().await?;
    let t = lock(&mut tx, number, who).await?;
    if !who.engineer && !who.is_requester(&t) {
        return Err(ApiError::Forbidden);
    }
    if matches!(t.status.as_str(), "closed" | "canceled") {
        return Err(bad_state("Заявка закрыта."));
    }
    if Some(person) == t.requester_id {
        return Err(ApiError::bad_request("invalid_watcher", "Заявитель и так получает все сообщения."));
    }
    let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM persons WHERE id = $1 AND NOT is_archived)")
        .bind(person)
        .fetch_one(&mut *tx)
        .await?;
    if !exists {
        return Err(ApiError::bad_request("invalid_watcher", "Сотрудник не найден."));
    }
    let added = sqlx::query(
        "INSERT INTO ticket_watchers (ticket_id, person_id, added_by) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
    )
    .bind(t.id)
    .bind(person)
    .bind(who.user)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if added > 0 {
        event(&mut tx, t.id, who.user, "watcher_added", Value::Null, json!(person)).await?;
        audit_rec(&mut tx, who, "ticket.watcher.add", &t, json!({"person": person})).await?;
        let users: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM users WHERE person_id = $1 AND is_active")
            .bind(person)
            .fetch_all(&mut *tx)
            .await?;
        for u in users {
            notify::bell(&mut tx, u, "helpdesk.watching", &t).await?;
        }
    }
    ping(&mut tx, &t).await?;
    tx.commit().await?;
    Ok(())
}

/// Убрать наблюдателя: инженер, заявитель или сам наблюдатель.
pub async fn remove_watcher(core: &Core, who: &Who, number: i64, person: Uuid) -> ApiResult<()> {
    let mut tx = core.db.begin().await?;
    let t = lock(&mut tx, number, who).await?;
    if !who.engineer && !who.is_requester(&t) && who.person != Some(person) {
        return Err(ApiError::Forbidden);
    }
    let gone = sqlx::query("DELETE FROM ticket_watchers WHERE ticket_id = $1 AND person_id = $2")
        .bind(t.id)
        .bind(person)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if gone > 0 {
        event(&mut tx, t.id, who.user, "watcher_removed", json!(person), Value::Null).await?;
        audit_rec(&mut tx, who, "ticket.watcher.remove", &t, json!({"person": person})).await?;
    }
    ping(&mut tx, &t).await?;
    tx.commit().await?;
    Ok(())
}

/// Пользователи-наблюдатели — им тоже приходит событие об изменении заявки.
pub async fn watcher_users(tx: &mut Transaction<'_, Postgres>, ticket: Uuid) -> Result<Vec<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT u.id FROM ticket_watchers w JOIN users u ON u.person_id = w.person_id WHERE w.ticket_id = $1",
    )
    .bind(ticket)
    .fetch_all(&mut **tx)
    .await
}

/// Проверка кода закрытия по списку из настроек.
pub fn check_close_code(codes: &[crate::settings::CloseCode], code: &str) -> ApiResult<String> {
    let code = code.trim();
    if codes.is_empty() {
        return Ok(code.to_string());
    }
    if code.is_empty() {
        return Err(ApiError::bad_request("close_code_required", "Выберите код закрытия."));
    }
    codes
        .iter()
        .find(|c| c.code == code)
        .map(|c| c.code.clone())
        .ok_or_else(|| ApiError::bad_request("invalid_close_code", "Такого кода закрытия нет."))
}

//! Обслуживание заявок: напоминания по «Ждёт ответа» и решение без ответа, оценка
//! после решения, группы исполнителей, массовые действия в списке.

use crate::{
    HelpdeskState, card,
    notify::{self, Msg},
    perm, schedule,
    settings::HelpdeskSettings,
    tickets::{self, Resolution, T, TicketUpdate, Who, event, lock, ping, select_t},
};
use axum::{
    Json,
    extract::{FromRef, Path, State},
    http::StatusCode,
};
use pult_core::{
    ApiError, ApiResult, Core, CurrentUser,
    audit::{self, Entry, Source},
    settings,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::OffsetDateTime;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub fn routes<S>() -> OpenApiRouter<S>
where
    S: Clone + Send + Sync + 'static,
    Core: FromRef<S>,
    HelpdeskState: FromRef<S>,
{
    OpenApiRouter::new()
        .routes(routes!(rate_ticket))
        .routes(routes!(bulk))
        .routes(routes!(list_groups, create_group))
        .routes(routes!(update_group, delete_group))
        .routes(routes!(list_vip))
        .routes(routes!(set_vip))
}

// ------------------------------------------------------------------ «Ждёт ответа»

/// Напоминание через N рабочих дней, решение с кодом «нет ответа» ещё через M.
/// Ошибка по одной заявке не останавливает остальные: пишется в журнал, обход продолжается.
pub async fn waiting_reminders(core: &Core) -> ApiResult<usize> {
    let cfg: HelpdeskSettings = settings::load(&core.db).await?;
    if cfg.waiting_remind_days == 0 && cfg.waiting_resolve_days == 0 {
        return Ok(0);
    }
    let c = notify::ctx(core).await?;
    // Заявки с открытыми обязательными пунктами чек-листа сами не решаются — их не берём.
    let rows: Vec<(i64, Option<Uuid>, OffsetDateTime, Option<OffsetDateTime>)> = sqlx::query_as(
        "SELECT number, site_id, waiting_since, reminded_at FROM tickets t
          WHERE status = 'waiting' AND waiting_since IS NOT NULL AND waiting_since < now() - interval '12 hours'
            AND NOT EXISTS (SELECT 1 FROM ticket_checklist_items i
                             WHERE i.ticket_id = t.id AND i.is_required AND i.done_at IS NULL)
          ORDER BY waiting_since LIMIT 500",
    )
    .fetch_all(&core.db)
    .await?;
    let mut n = 0;
    for (number, site, since, reminded) in rows {
        match remind_one(core, &cfg, &c, number, site, since, reminded).await {
            Ok(true) => n += 1,
            Ok(false) => {}
            Err(e) => tracing::warn!(ticket = number, error = %e, "напоминание «ждёт ответа» не отправлено"),
        }
    }
    Ok(n)
}

async fn remind_one(
    core: &Core,
    cfg: &HelpdeskSettings,
    c: &notify::Ctx,
    number: i64,
    site: Option<Uuid>,
    since: OffsetDateTime,
    reminded: Option<OffsetDateTime>,
) -> ApiResult<bool> {
    let mut tx = core.db.begin().await?;
    let (sch, tz) = schedule::for_site(&mut tx, site, cfg).await?;
    let (from, days) = match reminded {
        None if cfg.waiting_remind_days > 0 => (since, cfg.waiting_remind_days),
        None => (since, cfg.waiting_resolve_days),
        Some(r) => (r, cfg.waiting_resolve_days),
    };
    if days == 0 {
        return Ok(false);
    }
    let local = schedule::to_local(&mut *tx, from, &tz).await?;
    let at = schedule::to_utc(&mut *tx, sch.add_work_days(local, days), &tz).await?;
    if at > OffsetDateTime::now_utc() {
        return Ok(false);
    }
    // Пока считали, заявитель мог ответить: тогда заявка уже не «ждёт».
    let Some(t): Option<T> = sqlx::query_as(&select_t("WHERE number = $1 AND status = 'waiting' FOR UPDATE"))
        .bind(number)
        .fetch_optional(&mut *tx)
        .await?
    else {
        return Ok(false);
    };
    if reminded.is_none() && cfg.waiting_remind_days > 0 {
        sqlx::query("UPDATE tickets SET reminded_at = now() WHERE id = $1").bind(t.id).execute(&mut *tx).await?;
        event(&mut tx, t.id, None, "reminder", Value::Null, Value::Null).await?;
        notify::to_requester(&mut tx, &t, &Msg::Reminder, c).await?;
        tx.commit().await?;
    } else {
        tx.commit().await?;
        // Фоновая задача: текст — на языке заявителя.
        let lang = tickets::person_lang(core, t.requester_id).await?;
        let body = pult_core::l10n::tr_to(
            lang,
            "Ответа не было, заявка решена автоматически. Если проблема осталась — нажмите «Нет, не решена» или ответьте в заявке.",
        );
        let body = body.as_ref();
        let who = Who { engineer: true, ..Who::system() };
        tickets::resolve(core, &who, number, Resolution { body, closure_code: "no_response", ..Default::default() })
            .await?;
    }
    Ok(true)
}

// ------------------------------------------------------------------ Оценка

#[derive(Deserialize, ToSchema)]
pub struct RatingInput {
    /// `up` — хорошо, `down` — плохо.
    pub value: String,
    pub comment: Option<String>,
}

/// Заявитель оценивает решённую или закрытую заявку. «Плохо» — исполнителю и руководителю IT.
#[utoipa::path(post, path = "/api/helpdesk/tickets/{number}/rating", tag = "helpdesk", params(("number" = i64, Path)), request_body = RatingInput, responses((status = 204)))]
async fn rate_ticket(
    State(core): State<Core>,
    user: CurrentUser,
    Path(number): Path<i64>,
    Json(b): Json<RatingInput>,
) -> ApiResult<StatusCode> {
    let who = Who::from_user(&user);
    if !matches!(b.value.as_str(), "up" | "down") {
        return Err(ApiError::bad_request("invalid_rating", "Оценка — хорошо или плохо."));
    }
    let comment =
        b.comment.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(|s| s.chars().take(2000).collect::<String>());
    let cfg: HelpdeskSettings = settings::load(&core.db).await?;
    let c = notify::ctx(&core).await?;
    let mut tx = core.db.begin().await?;
    let t = lock(&mut tx, number, &who).await?;
    if !who.is_requester(&t) || !matches!(t.status.as_str(), "resolved" | "closed") {
        return Err(tickets::bad_state("Оценить можно свою решённую заявку."));
    }
    let before: Option<String> = sqlx::query_scalar("SELECT value FROM ticket_ratings WHERE ticket_id = $1")
        .bind(t.id)
        .fetch_optional(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO ticket_ratings (ticket_id, value, comment, rated_by) VALUES ($1, $2, $3, $4)
         ON CONFLICT (ticket_id) DO UPDATE SET value = EXCLUDED.value, comment = EXCLUDED.comment, updated_at = now()",
    )
    .bind(t.id)
    .bind(&b.value)
    .bind(&comment)
    .bind(who.user)
    .execute(&mut *tx)
    .await?;
    event(&mut tx, t.id, who.user, "rated", json!(before), json!(b.value)).await?;
    if b.value == "down" && before.as_deref() != Some("down") {
        let msg = Msg::BadRating { comment: comment.as_deref() };
        if let Some(a) = t.assignee_id {
            notify::bell(&mut tx, a, "helpdesk.bad_rating", &t).await?;
            notify::to_user(&mut tx, a, "helpdesk.bad_rating", &t, &msg, &c).await?;
        }
        if cfg.bad_rating_notify.is_empty() {
            pult_core::notify::notify_superadmins(
                &mut *tx,
                "helpdesk.bad_rating",
                json!({"number": t.number, "title": t.title}),
            )
            .await?;
        }
        for u in cfg.bad_rating_notify.iter().filter(|u| Some(**u) != t.assignee_id) {
            notify::bell(&mut tx, *u, "helpdesk.bad_rating", &t).await?;
            notify::to_user(&mut tx, *u, "helpdesk.bad_rating", &t, &msg, &c).await?;
        }
    }
    ping(&mut tx, &t).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

// ------------------------------------------------------------------ Массовые действия

#[derive(Deserialize, ToSchema)]
pub struct BulkInput {
    pub numbers: Vec<i64>,
    /// `assign`, `take`, `priority`, `category`, `group`, `resolve`, `note`, `merge`.
    pub action: String,
    pub assignee_id: Option<Uuid>,
    pub priority: Option<String>,
    pub category_id: Option<Uuid>,
    pub group_id: Option<Uuid>,
    pub comment: Option<String>,
    pub closure_code: Option<String>,
    /// Для `merge`: основная заявка.
    pub into: Option<i64>,
}

#[derive(Serialize, ToSchema)]
pub struct BulkResult {
    pub number: i64,
    pub ok: bool,
    pub error: Option<String>,
}

/// Применить действие к нескольким заявкам. Каждая — отдельно: ошибка одной не отменяет другие.
#[utoipa::path(post, path = "/api/helpdesk/tickets/bulk", tag = "helpdesk", request_body = BulkInput, responses((status = 200, body = Vec<BulkResult>)))]
async fn bulk(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<BulkInput>,
) -> ApiResult<Json<Vec<BulkResult>>> {
    user.require(&perm::WORK)?;
    let who = Who::from_user(&user);
    if b.numbers.is_empty() || b.numbers.len() > 200 {
        return Err(ApiError::bad_request("invalid_bulk", "Выберите от 1 до 200 заявок."));
    }
    let mut out = Vec::new();
    for &number in &b.numbers {
        let r: ApiResult<()> = async {
            match b.action.as_str() {
                "assign" => card::assign(&core, &who, number, b.assignee_id, b.comment.as_deref()).await,
                "take" => tickets::take(&core, &who, number).await,
                "priority" | "category" | "group" => {
                    let version: i32 = sqlx::query_scalar("SELECT version FROM tickets WHERE number = $1")
                        .bind(number)
                        .fetch_optional(&core.db)
                        .await?
                        .ok_or(ApiError::NotFound)?;
                    let u = TicketUpdate {
                        version,
                        priority: b.priority.clone().filter(|_| b.action == "priority"),
                        category_id: b.category_id.filter(|_| b.action == "category"),
                        site_id: None,
                        title: None,
                        computer_name: None,
                        impact: None,
                        urgency: None,
                        group_id: b.group_id.filter(|_| b.action == "group"),
                        clear_group: b.action == "group" && b.group_id.is_none(),
                    };
                    tickets::update(&core, &who, number, u).await
                }
                "resolve" => {
                    let body = b.comment.as_deref().unwrap_or_default();
                    let code = b.closure_code.as_deref().unwrap_or_default();
                    tickets::resolve(&core, &who, number, Resolution { body, closure_code: code, ..Default::default() })
                        .await
                }
                "note" => {
                    let body = b.comment.clone().unwrap_or_default();
                    let input = tickets::NewComment { body, internal: true, ..Default::default() };
                    tickets::comment(&core, &who, number, input).await.map(|_| ())
                }
                "merge" => {
                    let into =
                        b.into.ok_or_else(|| ApiError::bad_request("invalid_bulk", "Укажите основную заявку."))?;
                    if into == number { Ok(()) } else { crate::objects::merge(&core, &who, number, into).await }
                }
                _ => Err(ApiError::bad_request("invalid_bulk", "Неизвестное действие.")),
            }
        }
        .await;
        out.push(match r {
            Ok(()) => BulkResult { number, ok: true, error: None },
            // Текст — на языке запроса, как у обычных ошибок API.
            Err(e) => BulkResult { number, ok: false, error: Some(pult_core::l10n::tr(&e.to_string()).into_owned()) },
        });
    }
    audit::record(
        &core.db,
        &user.actor,
        Entry::new("ticket.bulk", Source::Portal).details(json!({"action": b.action, "numbers": b.numbers})),
    )
    .await?;
    Ok(Json(out))
}

// ------------------------------------------------------------------ Группы исполнителей

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct Group {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub auto_assign: bool,
    pub members: Vec<Uuid>,
    pub open_tickets: i64,
}

#[derive(Deserialize, ToSchema)]
pub struct GroupInput {
    pub name: String,
    pub description: Option<String>,
    #[serde(default = "yes")]
    pub auto_assign: bool,
    #[serde(default)]
    pub members: Vec<Uuid>,
}

fn yes() -> bool {
    true
}

/// Группы — инженерам для назначения, администратору для настройки.
#[utoipa::path(get, path = "/api/helpdesk/groups", tag = "helpdesk", responses((status = 200, body = Vec<Group>)))]
async fn list_groups(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<Vec<Group>>> {
    user.require(&perm::WORK)?;
    let rows = sqlx::query_as(
        "SELECT g.id, g.name, g.description, g.auto_assign,
                coalesce(array_agg(m.user_id) FILTER (WHERE m.user_id IS NOT NULL), '{}') AS members,
                (SELECT count(*) FROM tickets t WHERE t.group_id = g.id AND t.status = ANY($1)) AS open_tickets
           FROM engineer_groups g LEFT JOIN engineer_group_members m ON m.group_id = g.id
          GROUP BY g.id ORDER BY g.name",
    )
    .bind(tickets::OPEN_STATUSES)
    .fetch_all(&core.db)
    .await?;
    Ok(Json(rows))
}

async fn save_group(core: &Core, id: Option<Uuid>, b: &GroupInput) -> ApiResult<Uuid> {
    if b.name.trim().is_empty() {
        return Err(ApiError::bad_request("invalid_group", "Укажите название группы."));
    }
    let mut tx = core.db.begin().await?;
    let id: Uuid = match id {
        Some(id) => {
            let n =
                sqlx::query("UPDATE engineer_groups SET name = $2, description = $3, auto_assign = $4 WHERE id = $1")
                    .bind(id)
                    .bind(b.name.trim())
                    .bind(&b.description)
                    .bind(b.auto_assign)
                    .execute(&mut *tx)
                    .await
                    .map_err(dup)?
                    .rows_affected();
            if n == 0 {
                return Err(ApiError::NotFound);
            }
            id
        }
        None => sqlx::query_scalar(
            "INSERT INTO engineer_groups (name, description, auto_assign) VALUES ($1, $2, $3) RETURNING id",
        )
        .bind(b.name.trim())
        .bind(&b.description)
        .bind(b.auto_assign)
        .fetch_one(&mut *tx)
        .await
        .map_err(dup)?,
    };
    sqlx::query("DELETE FROM engineer_group_members WHERE group_id = $1").bind(id).execute(&mut *tx).await?;
    for u in &b.members {
        if !crate::assignment::is_engineer(&mut tx, *u).await? {
            return Err(ApiError::bad_request("invalid_group", "В группе — только инженеры площадок."));
        }
        sqlx::query("INSERT INTO engineer_group_members (group_id, user_id) VALUES ($1, $2) ON CONFLICT DO NOTHING")
            .bind(id)
            .bind(u)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(id)
}

fn dup(e: sqlx::Error) -> ApiError {
    match &e {
        sqlx::Error::Database(d) if d.is_unique_violation() => {
            ApiError::bad_request("duplicate", "Группа с таким названием уже есть.")
        }
        _ => e.into(),
    }
}

#[utoipa::path(post, path = "/api/admin/helpdesk/groups", tag = "helpdesk-admin", request_body = GroupInput, responses((status = 201, body = Object)))]
async fn create_group(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<GroupInput>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    user.require(&perm::ADMIN)?;
    let id = save_group(&core, None, &b).await?;
    audit::record(&core.db, &user.actor, Entry::new("helpdesk.group.create", Source::Portal).object("group", id))
        .await?;
    Ok((StatusCode::CREATED, Json(json!({"id": id}))))
}

#[utoipa::path(put, path = "/api/admin/helpdesk/groups/{id}", tag = "helpdesk-admin", params(("id" = Uuid, Path)), request_body = GroupInput, responses((status = 204)))]
async fn update_group(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<GroupInput>,
) -> ApiResult<StatusCode> {
    user.require(&perm::ADMIN)?;
    save_group(&core, Some(id), &b).await?;
    audit::record(&core.db, &user.actor, Entry::new("helpdesk.group.update", Source::Portal).object("group", id))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(delete, path = "/api/admin/helpdesk/groups/{id}", tag = "helpdesk-admin", params(("id" = Uuid, Path)), responses((status = 204)))]
async fn delete_group(State(core): State<Core>, user: CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    user.require(&perm::ADMIN)?;
    let n = sqlx::query("DELETE FROM engineer_groups WHERE id = $1").bind(id).execute(&core.db).await?.rows_affected();
    if n == 0 {
        return Err(ApiError::NotFound);
    }
    audit::record(&core.db, &user.actor, Entry::new("helpdesk.group.delete", Source::Portal).object("group", id))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ------------------------------------------------------------------ VIP

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct VipPerson {
    pub id: Uuid,
    pub name: String,
    pub department_name: Option<String>,
}

/// Сотрудники VIP: их заявки на ступень приоритетнее (если включено в настройках).
#[utoipa::path(get, path = "/api/admin/helpdesk/vip", tag = "helpdesk-admin", responses((status = 200, body = Vec<VipPerson>)))]
async fn list_vip(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<Vec<VipPerson>>> {
    user.require(&perm::ADMIN)?;
    let rows = sqlx::query_as(
        "SELECT p.id, trim(concat_ws(' ', p.last_name, p.first_name, p.middle_name)) AS name, d.name AS department_name
           FROM persons p LEFT JOIN departments d ON d.id = p.department_id
          WHERE p.is_vip ORDER BY p.last_name, p.first_name",
    )
    .fetch_all(&core.db)
    .await?;
    Ok(Json(rows))
}

#[derive(Deserialize, ToSchema)]
pub struct VipInput {
    pub is_vip: bool,
}

#[utoipa::path(put, path = "/api/admin/helpdesk/vip/{person_id}", tag = "helpdesk-admin", params(("person_id" = Uuid, Path)), request_body = VipInput, responses((status = 204)))]
async fn set_vip(
    State(core): State<Core>,
    user: CurrentUser,
    Path(person_id): Path<Uuid>,
    Json(b): Json<VipInput>,
) -> ApiResult<StatusCode> {
    user.require(&perm::ADMIN)?;
    let n = sqlx::query("UPDATE persons SET is_vip = $2 WHERE id = $1")
        .bind(person_id)
        .bind(b.is_vip)
        .execute(&core.db)
        .await?
        .rows_affected();
    if n == 0 {
        return Err(ApiError::NotFound);
    }
    audit::record(
        &core.db,
        &user.actor,
        Entry::new("helpdesk.vip.set", Source::Portal).object("person", person_id).details(json!({"is_vip": b.is_vip})),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

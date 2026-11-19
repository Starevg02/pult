//! Шаблоны заявок с чек-листами, регламентные заявки по расписанию, шаблоны ответов.

use crate::{
    HelpdeskState,
    checklist::NewItem,
    perm,
    tickets::{self, NewTicket, Who, lock},
};
use axum::{
    Json,
    extract::{FromRef, Path, State},
    http::StatusCode,
};
use pult_core::{
    ApiError, ApiResult, Core, CurrentUser,
    audit::{self, Entry, Source},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Postgres, Transaction};
use time::{OffsetDateTime, UtcOffset};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// Шаблон, применённый к новой заявке.
#[derive(Debug, Clone)]
pub struct Applied {
    pub title: String,
    pub description: String,
    pub category_id: Option<Uuid>,
    pub group_id: Option<Uuid>,
    pub checklist: Vec<NewItem>,
}

type TemplateRow = (String, String, Option<Uuid>, Option<Uuid>, Value);

pub async fn load(tx: &mut Transaction<'_, Postgres>, id: Uuid, engineer: bool) -> ApiResult<Applied> {
    let row: Option<TemplateRow> = sqlx::query_as(
        "SELECT title, description, category_id, group_id, checklist FROM ticket_templates
          WHERE id = $1 AND is_active AND ($2 OR audience = 'all')",
    )
    .bind(id)
    .bind(engineer)
    .fetch_optional(&mut **tx)
    .await?;
    let (title, description, category_id, group_id, checklist) =
        row.ok_or_else(|| ApiError::bad_request("invalid_ticket", "Шаблон не найден."))?;
    Ok(Applied {
        title,
        description,
        category_id,
        group_id,
        checklist: serde_json::from_value(checklist).unwrap_or_default(),
    })
}

pub fn routes<S>() -> OpenApiRouter<S>
where
    S: Clone + Send + Sync + 'static,
    Core: FromRef<S>,
    HelpdeskState: FromRef<S>,
{
    OpenApiRouter::new()
        .routes(routes!(list_templates))
        .routes(routes!(admin_templates, create_template))
        .routes(routes!(update_template, delete_template))
        .routes(routes!(list_recurring, create_recurring))
        .routes(routes!(update_recurring, delete_recurring))
        .routes(routes!(run_recurring_now))
        .routes(routes!(list_responses, create_response))
        .routes(routes!(update_response, delete_response))
        .routes(routes!(render_response))
}

async fn rec(core: &Core, user: &CurrentUser, action: &'static str, kind: &'static str, id: Uuid) -> ApiResult<()> {
    audit::record(&core.db, &user.actor, Entry::new(action, Source::Portal).object(kind, id)).await?;
    Ok(())
}

fn not_found(n: u64) -> ApiResult<StatusCode> {
    if n == 0 { Err(ApiError::NotFound) } else { Ok(StatusCode::NO_CONTENT) }
}

// ------------------------------------------------------------------ Шаблоны заявок

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct TicketTemplate {
    pub id: Uuid,
    pub name: String,
    pub category_id: Option<Uuid>,
    pub group_id: Option<Uuid>,
    pub title: String,
    pub description: String,
    #[schema(value_type = Vec<NewItem>)]
    pub checklist: Value,
    /// `all` — и сотрудникам в форме заявки, `engineers` — только инженерам.
    pub audience: String,
    pub is_active: bool,
    pub sort: i32,
}

const TPL_COLS: &str = "id, name, category_id, group_id, title, description, checklist, audience, is_active, sort";

/// Шаблоны для формы заявки.
#[utoipa::path(get, path = "/api/helpdesk/templates", tag = "helpdesk", responses((status = 200, body = Vec<TicketTemplate>)))]
async fn list_templates(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<Vec<TicketTemplate>>> {
    let rows = sqlx::query_as(&format!(
        "SELECT {TPL_COLS} FROM ticket_templates WHERE is_active AND ($1 OR audience = 'all') ORDER BY sort, name" // SAFE: константа
    ))
    .bind(user.grants.has(&perm::WORK))
    .fetch_all(&core.db)
    .await?;
    Ok(Json(rows))
}

#[utoipa::path(get, path = "/api/admin/helpdesk/templates", tag = "helpdesk-admin", responses((status = 200, body = Vec<TicketTemplate>)))]
async fn admin_templates(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<Vec<TicketTemplate>>> {
    user.require(&perm::ADMIN)?;
    let rows = sqlx::query_as(&format!("SELECT {TPL_COLS} FROM ticket_templates ORDER BY sort, name")) // SAFE: константа
        .fetch_all(&core.db)
        .await?;
    Ok(Json(rows))
}

#[derive(Deserialize, ToSchema)]
pub struct TemplateInput {
    pub name: String,
    pub category_id: Option<Uuid>,
    pub group_id: Option<Uuid>,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub checklist: Vec<NewItem>,
    pub audience: String,
    #[serde(default = "yes")]
    pub is_active: bool,
    #[serde(default = "hundred")]
    pub sort: i32,
}

fn yes() -> bool {
    true
}
fn hundred() -> i32 {
    100
}

fn check_template(b: &TemplateInput) -> ApiResult<()> {
    let bad = |m: &str| Err(ApiError::bad_request("invalid_template", m));
    if b.name.trim().is_empty() || b.title.trim().is_empty() {
        return bad("Укажите название и тему заявки.");
    }
    if !matches!(b.audience.as_str(), "all" | "engineers") {
        return bad("Кому доступен — all или engineers.");
    }
    if b.checklist.len() > 100 || b.checklist.iter().any(|i| i.title.trim().is_empty()) {
        return bad("Пункты чек-листа — с текстом, не больше 100.");
    }
    Ok(())
}

#[utoipa::path(post, path = "/api/admin/helpdesk/templates", tag = "helpdesk-admin", request_body = TemplateInput, responses((status = 201, body = Object)))]
async fn create_template(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<TemplateInput>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    user.require(&perm::ADMIN)?;
    check_template(&b)?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO ticket_templates (name, category_id, group_id, title, description, checklist, audience, is_active, sort)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING id",
    )
    .bind(b.name.trim())
    .bind(b.category_id)
    .bind(b.group_id)
    .bind(b.title.trim())
    .bind(&b.description)
    .bind(json!(b.checklist))
    .bind(&b.audience)
    .bind(b.is_active)
    .bind(b.sort)
    .fetch_one(&core.db)
    .await?;
    rec(&core, &user, "helpdesk.template.create", "ticket_template", id).await?;
    Ok((StatusCode::CREATED, Json(json!({"id": id}))))
}

#[utoipa::path(put, path = "/api/admin/helpdesk/templates/{id}", tag = "helpdesk-admin", params(("id" = Uuid, Path)), request_body = TemplateInput, responses((status = 204)))]
async fn update_template(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<TemplateInput>,
) -> ApiResult<StatusCode> {
    user.require(&perm::ADMIN)?;
    check_template(&b)?;
    let n = sqlx::query(
        "UPDATE ticket_templates SET name = $2, category_id = $3, group_id = $4, title = $5, description = $6, checklist = $7,
                audience = $8, is_active = $9, sort = $10, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(b.name.trim())
    .bind(b.category_id)
    .bind(b.group_id)
    .bind(b.title.trim())
    .bind(&b.description)
    .bind(json!(b.checklist))
    .bind(&b.audience)
    .bind(b.is_active)
    .bind(b.sort)
    .execute(&core.db)
    .await?
    .rows_affected();
    rec(&core, &user, "helpdesk.template.update", "ticket_template", id).await?;
    not_found(n)
}

#[utoipa::path(delete, path = "/api/admin/helpdesk/templates/{id}", tag = "helpdesk-admin", params(("id" = Uuid, Path)), responses((status = 204)))]
async fn delete_template(State(core): State<Core>, user: CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    user.require(&perm::ADMIN)?;
    let n = sqlx::query("DELETE FROM ticket_templates WHERE id = $1").bind(id).execute(&core.db).await?.rows_affected();
    rec(&core, &user, "helpdesk.template.delete", "ticket_template", id).await?;
    not_found(n)
}

// ------------------------------------------------------------------ Регламентные заявки

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct Recurring {
    pub id: Uuid,
    pub name: String,
    pub template_id: Uuid,
    /// Расписание cron: `0 9 * * 1` — по понедельникам в 9:00 (местное время площадки).
    pub schedule: String,
    pub site_id: Option<Uuid>,
    pub requester_id: Option<Uuid>,
    pub is_active: bool,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub next_run_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub last_run_at: Option<OffsetDateTime>,
    pub last_number: Option<i64>,
}

#[derive(Deserialize, ToSchema)]
pub struct RecurringInput {
    pub name: String,
    pub template_id: Uuid,
    /// cron или `daily`, `weekly`, `monthly`, `quarterly` (в 9:00; weekly — понедельник, monthly — 1-е число).
    pub schedule: String,
    pub site_id: Option<Uuid>,
    /// Заявитель; пусто — заявка без заявителя от имени системы.
    pub requester_id: Option<Uuid>,
    #[serde(default = "yes")]
    pub is_active: bool,
}

/// Сокращения расписаний для админки.
pub fn expand_schedule(s: &str) -> String {
    match s.trim() {
        "daily" => "0 9 * * *".into(),
        "weekly" => "0 9 * * 1".into(),
        "monthly" => "0 9 1 * *".into(),
        "quarterly" => "0 9 1 1,4,7,10 *".into(),
        other => other.to_string(),
    }
}

/// Смещение пояса площадки от UTC сейчас.
async fn site_offset(tx: &mut Transaction<'_, Postgres>, site: Option<Uuid>) -> Result<UtcOffset, sqlx::Error> {
    let cfg: crate::settings::HelpdeskSettings = pult_core::settings::load(&mut **tx).await?;
    let tz: String = match site {
        Some(s) => sqlx::query_scalar("SELECT timezone FROM sites WHERE id = $1")
            .bind(s)
            .fetch_optional(&mut **tx)
            .await?
            .unwrap_or(cfg.timezone),
        None => cfg.timezone,
    };
    let tz = crate::schedule::known_tz(&mut **tx, &tz).await?;
    let secs: i32 =
        sqlx::query_scalar("SELECT extract(epoch FROM (now() AT TIME ZONE $1) - (now() AT TIME ZONE 'UTC'))::int")
            .bind(&tz)
            .fetch_one(&mut **tx)
            .await?;
    Ok(UtcOffset::from_whole_seconds(secs).unwrap_or(UtcOffset::UTC))
}

async fn next_run(
    tx: &mut Transaction<'_, Postgres>,
    schedule: &str,
    site: Option<Uuid>,
    after: OffsetDateTime,
) -> ApiResult<OffsetDateTime> {
    let cron = pult_jobs::cron::Schedule::parse(schedule)
        .map_err(|e| ApiError::bad_request("invalid_schedule", e.to_string()))?;
    let offset = site_offset(tx, site).await?;
    cron.next_after(after, offset)
        .ok_or_else(|| ApiError::bad_request("invalid_schedule", "Расписание никогда не срабатывает."))
}

#[utoipa::path(get, path = "/api/admin/helpdesk/recurring", tag = "helpdesk-admin", responses((status = 200, body = Vec<Recurring>)))]
async fn list_recurring(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<Vec<Recurring>>> {
    user.require(&perm::ADMIN)?;
    let rows = sqlx::query_as(
        "SELECT r.id, r.name, r.template_id, r.schedule, r.site_id, r.requester_id, r.is_active, r.next_run_at, r.last_run_at,
                (SELECT number FROM tickets t WHERE t.id = r.last_ticket_id) AS last_number
           FROM recurring_tickets r ORDER BY r.name",
    )
    .fetch_all(&core.db)
    .await?;
    Ok(Json(rows))
}

async fn save_recurring(core: &Core, id: Option<Uuid>, b: &RecurringInput) -> ApiResult<Uuid> {
    if b.name.trim().is_empty() {
        return Err(ApiError::bad_request("invalid_schedule", "Укажите название."));
    }
    let schedule = expand_schedule(&b.schedule);
    let mut tx = core.db.begin().await?;
    let next = next_run(&mut tx, &schedule, b.site_id, OffsetDateTime::now_utc()).await?;
    let id: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO recurring_tickets (id, name, template_id, schedule, site_id, requester_id, is_active, next_run_at)
         VALUES (coalesce($1, uuid_v7()), $2, $3, $4, $5, $6, $7, $8)
         ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name, template_id = EXCLUDED.template_id, schedule = EXCLUDED.schedule,
                site_id = EXCLUDED.site_id, requester_id = EXCLUDED.requester_id, is_active = EXCLUDED.is_active,
                next_run_at = EXCLUDED.next_run_at
         RETURNING id",
    )
    .bind(id)
    .bind(b.name.trim())
    .bind(b.template_id)
    .bind(&schedule)
    .bind(b.site_id)
    .bind(b.requester_id)
    .bind(b.is_active)
    .bind(next)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    id.ok_or(ApiError::NotFound)
}

#[utoipa::path(post, path = "/api/admin/helpdesk/recurring", tag = "helpdesk-admin", request_body = RecurringInput, responses((status = 201, body = Object)))]
async fn create_recurring(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<RecurringInput>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    user.require(&perm::ADMIN)?;
    let id = save_recurring(&core, None, &b).await?;
    rec(&core, &user, "helpdesk.recurring.create", "recurring", id).await?;
    Ok((StatusCode::CREATED, Json(json!({"id": id}))))
}

#[utoipa::path(put, path = "/api/admin/helpdesk/recurring/{id}", tag = "helpdesk-admin", params(("id" = Uuid, Path)), request_body = RecurringInput, responses((status = 204)))]
async fn update_recurring(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<RecurringInput>,
) -> ApiResult<StatusCode> {
    user.require(&perm::ADMIN)?;
    let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM recurring_tickets WHERE id = $1)")
        .bind(id)
        .fetch_one(&core.db)
        .await?;
    if !exists {
        return Err(ApiError::NotFound);
    }
    save_recurring(&core, Some(id), &b).await?;
    rec(&core, &user, "helpdesk.recurring.update", "recurring", id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(delete, path = "/api/admin/helpdesk/recurring/{id}", tag = "helpdesk-admin", params(("id" = Uuid, Path)), responses((status = 204)))]
async fn delete_recurring(State(core): State<Core>, user: CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    user.require(&perm::ADMIN)?;
    let n =
        sqlx::query("DELETE FROM recurring_tickets WHERE id = $1").bind(id).execute(&core.db).await?.rows_affected();
    rec(&core, &user, "helpdesk.recurring.delete", "recurring", id).await?;
    not_found(n)
}

/// Создать заявку по регламенту сейчас (проверка настройки).
#[utoipa::path(post, path = "/api/admin/helpdesk/recurring/{id}/run", tag = "helpdesk-admin", params(("id" = Uuid, Path)), responses((status = 200, body = Object)))]
async fn run_recurring_now(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    user.require(&perm::ADMIN)?;
    let number = run_one(&core, id, false).await?;
    rec(&core, &user, "helpdesk.recurring.run", "recurring", id).await?;
    Ok(Json(json!({"number": number})))
}

/// Создать заявку регламента. `scheduled` — по расписанию: сдвигает следующий запуск.
async fn run_one(core: &Core, id: Uuid, scheduled: bool) -> ApiResult<i64> {
    #[derive(sqlx::FromRow)]
    struct Rec {
        name: String,
        template_id: Uuid,
        schedule: String,
        site_id: Option<Uuid>,
        requester_id: Option<Uuid>,
        next_run_at: Option<OffsetDateTime>,
    }
    let mut tx = core.db.begin().await?;
    let r: Rec = sqlx::query_as(
        "SELECT name, template_id, schedule, site_id, requester_id, next_run_at FROM recurring_tickets WHERE id = $1 FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(ApiError::NotFound)?;
    if scheduled {
        // Пропущенные запуски (сервер был выключен) не наверстываем: одна заявка и следующий срок.
        let base = r.next_run_at.unwrap_or_else(OffsetDateTime::now_utc).max(OffsetDateTime::now_utc());
        let next = next_run(&mut tx, &r.schedule, r.site_id, base).await?;
        sqlx::query("UPDATE recurring_tickets SET next_run_at = $2 WHERE id = $1")
            .bind(id)
            .bind(next)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    let requester_user: Option<Uuid> = match r.requester_id {
        Some(p) => {
            sqlx::query_scalar("SELECT id FROM users WHERE person_id = $1 LIMIT 1")
                .bind(p)
                .fetch_optional(&core.db)
                .await?
        }
        None => None,
    };
    let created = tickets::create(
        core,
        &Who { engineer: true, ..Who::system() },
        NewTicket {
            title: String::new(),
            description: String::new(),
            site_id: r.site_id,
            requester_id: r.requester_id,
            requester_user_id: requester_user,
            channel: "api".into(),
            form_answers: json!({}),
            template_id: Some(r.template_id),
            recurring_id: Some(id),
            system: true,
            ..Default::default()
        },
    )
    .await?;
    sqlx::query(
        "UPDATE recurring_tickets SET last_run_at = now(), last_ticket_id = (SELECT id FROM tickets WHERE number = $2) WHERE id = $1",
    )
    .bind(id)
    .bind(created.number)
    .execute(&core.db)
    .await?;
    tracing::info!(name = %r.name, number = created.number, "регламентная заявка создана");
    Ok(created.number)
}

/// Задача по расписанию: создать подошедшие регламентные заявки.
pub async fn run_due(core: &Core) -> ApiResult<usize> {
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM recurring_tickets WHERE is_active AND next_run_at <= now() ORDER BY next_run_at LIMIT 50",
    )
    .fetch_all(&core.db)
    .await?;
    let mut n = 0;
    for id in ids {
        match run_one(core, id, true).await {
            Ok(_) => n += 1,
            Err(e) => tracing::warn!(%id, error = %e, "регламентная заявка не создана"),
        }
    }
    Ok(n)
}

// ------------------------------------------------------------------ Шаблоны ответов

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct ResponseTemplate {
    pub id: Uuid,
    pub title: String,
    pub body: String,
    /// Общий — видят все инженеры; личный — только автор.
    pub shared: bool,
    pub mine: bool,
}

#[derive(Deserialize, ToSchema)]
pub struct ResponseInput {
    pub title: String,
    pub body: String,
    /// Общий шаблон может завести администратор Helpdesk.
    #[serde(default)]
    pub shared: bool,
}

#[utoipa::path(get, path = "/api/helpdesk/response-templates", tag = "helpdesk", responses((status = 200, body = Vec<ResponseTemplate>)))]
async fn list_responses(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<Vec<ResponseTemplate>>> {
    user.require(&perm::WORK)?;
    let rows = sqlx::query_as(
        "SELECT id, title, body, owner_id IS NULL AS shared, owner_id IS NOT DISTINCT FROM $1 AS mine
           FROM response_templates WHERE owner_id IS NULL OR owner_id = $1 ORDER BY owner_id IS NULL, title",
    )
    .bind(user.user_id)
    .fetch_all(&core.db)
    .await?;
    Ok(Json(rows))
}

fn check_response(b: &ResponseInput) -> ApiResult<()> {
    if b.title.trim().is_empty() || b.body.trim().is_empty() || b.body.len() > 20_000 {
        return Err(ApiError::bad_request("invalid_template", "Укажите название и текст шаблона."));
    }
    Ok(())
}

#[utoipa::path(post, path = "/api/helpdesk/response-templates", tag = "helpdesk", request_body = ResponseInput, responses((status = 201, body = Object)))]
async fn create_response(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<ResponseInput>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    user.require(&perm::WORK)?;
    if b.shared {
        user.require(&perm::ADMIN)?;
    }
    check_response(&b)?;
    let id: Uuid =
        sqlx::query_scalar("INSERT INTO response_templates (title, body, owner_id) VALUES ($1, $2, $3) RETURNING id")
            .bind(b.title.trim())
            .bind(&b.body)
            .bind(if b.shared { None } else { Some(user.user_id) })
            .fetch_one(&core.db)
            .await?;
    Ok((StatusCode::CREATED, Json(json!({"id": id}))))
}

/// Изменить шаблон: свой личный или общий (администратор).
async fn may_edit(core: &Core, user: &CurrentUser, id: Uuid) -> ApiResult<()> {
    user.require(&perm::WORK)?;
    let owner: Option<Option<Uuid>> = sqlx::query_scalar("SELECT owner_id FROM response_templates WHERE id = $1")
        .bind(id)
        .fetch_optional(&core.db)
        .await?;
    match owner {
        None => Err(ApiError::NotFound),
        Some(Some(o)) if o == user.user_id => Ok(()),
        Some(None) => user.require(&perm::ADMIN),
        Some(Some(_)) => Err(ApiError::NotFound),
    }
}

#[utoipa::path(put, path = "/api/helpdesk/response-templates/{id}", tag = "helpdesk", params(("id" = Uuid, Path)), request_body = ResponseInput, responses((status = 204)))]
async fn update_response(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<ResponseInput>,
) -> ApiResult<StatusCode> {
    may_edit(&core, &user, id).await?;
    check_response(&b)?;
    let n = sqlx::query("UPDATE response_templates SET title = $2, body = $3, updated_at = now() WHERE id = $1")
        .bind(id)
        .bind(b.title.trim())
        .bind(&b.body)
        .execute(&core.db)
        .await?
        .rows_affected();
    not_found(n)
}

#[utoipa::path(delete, path = "/api/helpdesk/response-templates/{id}", tag = "helpdesk", params(("id" = Uuid, Path)), responses((status = 204)))]
async fn delete_response(State(core): State<Core>, user: CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    may_edit(&core, &user, id).await?;
    let n =
        sqlx::query("DELETE FROM response_templates WHERE id = $1").bind(id).execute(&core.db).await?.rows_affected();
    not_found(n)
}

/// Подстановки: `{{requester.first_name}}`, `{{requester.name}}`, `{{ticket.number}}`, `{{ticket.title}}`,
/// `{{engineer.name}}`. Неизвестные оставляются как есть. Пустое значение убирается вместе с запятой
/// или пробелом перед ним: «Здравствуйте, {{requester.first_name}}!» без имени — «Здравствуйте!».
pub fn substitute(body: &str, vars: &[(&str, String)]) -> String {
    // Один проход: подставленный текст (например, тема заявки с «{{engineer.name}}») повторно не разбирается.
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            out.push_str(&rest[start..]);
            return out;
        };
        let key = after[..end].trim();
        match vars.iter().find(|(k, _)| *k == key) {
            Some((_, v)) => {
                if v.is_empty() {
                    let cut = if out.ends_with(", ") {
                        2
                    } else if out.ends_with(' ') {
                        1
                    } else {
                        0
                    };
                    out.truncate(out.len() - cut);
                }
                out.push_str(v);
            }
            None => out.push_str(&rest[start..start + 2 + end + 2]),
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    out
}

#[derive(Serialize, ToSchema)]
pub struct Rendered {
    pub body: String,
}

/// Текст шаблона с подстановками для этой заявки.
#[utoipa::path(get, path = "/api/helpdesk/tickets/{number}/response-templates/{id}", tag = "helpdesk",
    params(("number" = i64, Path), ("id" = Uuid, Path)), responses((status = 200, body = Rendered)))]
async fn render_response(
    State(core): State<Core>,
    user: CurrentUser,
    Path((number, id)): Path<(i64, Uuid)>,
) -> ApiResult<Json<Rendered>> {
    user.require(&perm::WORK)?;
    let who = Who::from_user(&user);
    let mut tx = core.db.begin().await?;
    let t = lock(&mut tx, number, &who).await?;
    let body: String =
        sqlx::query_scalar("SELECT body FROM response_templates WHERE id = $1 AND (owner_id IS NULL OR owner_id = $2)")
            .bind(id)
            .bind(user.user_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(ApiError::NotFound)?;
    let (first, full): (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT nullif(p.first_name, ''), nullif(trim(concat_ws(' ', p.first_name, p.middle_name)), '')
           FROM persons p WHERE p.id = $1",
    )
    .bind(t.requester_id)
    .fetch_optional(&mut *tx)
    .await?
    .unwrap_or_default();
    let me = tickets::user_name(&mut tx, user.user_id).await?.unwrap_or_default();
    tx.rollback().await?;
    let vars = [
        ("requester.first_name", first.clone().unwrap_or_default()),
        ("requester.name", full.or(first).unwrap_or_default()),
        ("ticket.number", t.number.to_string()),
        ("ticket.title", t.title.clone()),
        ("engineer.name", me),
    ];
    Ok(Json(Rendered { body: substitute(&body, &vars) }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitutions() {
        let s = substitute(
            "Здравствуйте, {{requester.first_name}}! По заявке #{{ ticket.number }} — {{unknown}}",
            &[("requester.first_name", "Иван".into()), ("ticket.number", "1234".into())],
        );
        assert_eq!(s, "Здравствуйте, Иван! По заявке #1234 — {{unknown}}");
        let s = substitute(
            "Здравствуйте, {{requester.first_name}}! {{engineer.name}}",
            &[("requester.first_name", String::new()), ("engineer.name", String::new())],
        );
        assert_eq!(s, "Здравствуйте!", "пустое имя без «, !»");
        let s = substitute(
            "{{ticket.title}} — {{engineer.name}}",
            &[("ticket.title", "{{engineer.name}}".into()), ("engineer.name", "Пётр".into())],
        );
        assert_eq!(s, "{{engineer.name}} — Пётр", "подставленное значение не разбирается повторно");
        assert_eq!(substitute("a {{b", &[("b", "x".into())]), "a {{b");
    }

    #[test]
    fn schedule_presets() {
        assert_eq!(expand_schedule("weekly"), "0 9 * * 1");
        assert!(pult_jobs::cron::Schedule::parse(&expand_schedule("quarterly")).is_ok());
        assert_eq!(expand_schedule("*/5 * * * *"), "*/5 * * * *");
    }
}

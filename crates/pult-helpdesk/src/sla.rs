//! SLA: политики по площадке, категории и приоритету; сроки в рабочих минутах
//! по календарю площадки; пауза на «Ждёт ответа» (настройка), «Отложена» и после решения;
//! «скоро срок» при остатке меньше 20 %, «просрочено»; уведомления исполнителю и суперадминистраторам.
//! Здесь же матрица приоритета «влияние × срочность» и отметка VIP.

use crate::{
    HelpdeskState,
    notify::{self, Msg},
    perm, schedule,
    schedule::DayHours,
    settings::HelpdeskSettings,
    tickets::{OPEN_STATUSES, T, event, select_t},
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
use sqlx::{Postgres, Transaction};
use time::{Date, OffsetDateTime, Time};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// Доля срока, после которой загорается «скоро срок».
const WARN_SHARE: f64 = 0.8;

pub const PRIORITIES: &[&str] = &["low", "normal", "high", "critical"];
pub const IMPACTS: &[&str] = &["person", "department", "organization"];
pub const URGENCIES: &[&str] = &["normal", "hinders", "blocked"];

/// Состояние срока для списков: `ok`, `warning`, `breached`, `paused`, `met`; без SLA и у отменённых — NULL.
pub const SLA_STATE: &str = "CASE
    WHEN t.resolve_due_at IS NULL AND t.response_due_at IS NULL THEN NULL
    WHEN t.status = 'canceled' THEN NULL
    WHEN t.status IN ('resolved', 'closed', 'canceled') THEN
        CASE WHEN (t.resolve_due_at IS NULL OR t.resolved_at <= t.resolve_due_at)
                  AND (t.response_due_at IS NULL OR coalesce(t.taken_at, t.resolved_at) <= t.response_due_at)
             THEN 'met' ELSE 'breached' END
    WHEN (t.taken_at IS NULL AND t.response_due_at <= now()) OR (t.resolve_due_at <= now() AND t.sla_paused_at IS NULL)
        OR (t.taken_at > t.response_due_at) THEN 'breached'
    WHEN t.sla_paused_at IS NOT NULL THEN 'paused'
    WHEN (t.taken_at IS NULL AND t.response_warn_at <= now()) OR t.resolve_warn_at <= now() THEN 'warning'
    ELSE 'ok' END";

/// Статусы, на которых срок решения не идёт.
fn paused(status: &str, cfg: &HelpdeskSettings) -> bool {
    match status {
        "waiting" => cfg.sla_pause_waiting,
        "on_hold" | "waiting_approval" | "resolved" | "closed" | "canceled" => true,
        _ => false,
    }
}

/// Приоритет по матрице; VIP поднимает на ступень (настройка).
pub fn matrix_priority(cfg: &HelpdeskSettings, impact: &str, urgency: &str, vip: bool) -> String {
    let i = IMPACTS.iter().position(|x| *x == impact).unwrap_or(0);
    let u = URGENCIES.iter().position(|x| *x == urgency).unwrap_or(0);
    let p = cfg.priority_matrix.get(i).and_then(|r| r.get(u)).map(String::as_str).unwrap_or("normal");
    if vip && cfg.vip_bump { bump(p) } else { p.to_string() }
}

pub fn bump(p: &str) -> String {
    let i = PRIORITIES.iter().position(|x| *x == p).unwrap_or(1);
    PRIORITIES[(i + 1).min(PRIORITIES.len() - 1)].to_string()
}

#[derive(sqlx::FromRow)]
struct Clock {
    created_at: OffsetDateTime,
    site_id: Option<Uuid>,
    category_id: Option<Uuid>,
    priority: String,
    sla_paused_at: Option<OffsetDateTime>,
    sla_paused_minutes: i32,
}

#[derive(sqlx::FromRow)]
struct Policy {
    id: Uuid,
    response_minutes: Option<i32>,
    resolve_minutes: Option<i32>,
}

async fn clock(tx: &mut Transaction<'_, Postgres>, ticket: Uuid) -> Result<Clock, sqlx::Error> {
    sqlx::query_as(
        "SELECT created_at, site_id, category_id, priority, sla_paused_at, sla_paused_minutes FROM tickets WHERE id = $1",
    )
    .bind(ticket)
    .fetch_one(&mut **tx)
    .await
}

/// Подобрать политику и пересчитать сроки (создание, смена приоритета, категории, площадки, конец паузы).
pub async fn apply(tx: &mut Transaction<'_, Postgres>, ticket: Uuid) -> Result<(), sqlx::Error> {
    let cfg: HelpdeskSettings = settings::load(&mut **tx).await?;
    let c = clock(tx, ticket).await?;
    // Точнее заданная политика важнее; при равенстве — порядок из админки.
    let policy: Option<Policy> = sqlx::query_as(
        "SELECT id, response_minutes, resolve_minutes FROM sla_policies
          WHERE is_active AND (site_id IS NULL OR site_id = $1) AND (category_id IS NULL OR category_id = $2)
            AND (priority IS NULL OR priority = $3)
            -- Проблемы и изменения живут по своему плану, а не по SLA обращений.
            AND EXISTS (SELECT 1 FROM tickets WHERE id = $4 AND kind IN ('incident', 'request'))
          ORDER BY (site_id IS NOT NULL)::int + (category_id IS NOT NULL)::int + (priority IS NOT NULL)::int DESC, sort, name
          LIMIT 1",
    )
    .bind(c.site_id)
    .bind(c.category_id)
    .bind(&c.priority)
    .bind(ticket)
    .fetch_optional(&mut **tx)
    .await?;
    let Some(p) = policy else {
        sqlx::query(
            "UPDATE tickets SET sla_policy_id = NULL, response_due_at = NULL, response_warn_at = NULL,
                    resolve_due_at = NULL, resolve_warn_at = NULL WHERE id = $1",
        )
        .bind(ticket)
        .execute(&mut **tx)
        .await?;
        return Ok(());
    };
    let (sch, tz) = schedule::for_site(tx, c.site_id, &cfg).await?;
    let start = schedule::to_local(&mut **tx, c.created_at, &tz).await?;
    let at = |m: Option<i32>, share: f64, extra: i32| {
        m.map(|m| sch.add_work_minutes(start, (f64::from(m) * share).round() as i64 + i64::from(extra)))
    };
    let mut due: Vec<Option<OffsetDateTime>> = Vec::new();
    for t in [
        at(p.response_minutes, 1.0, 0),
        at(p.response_minutes, WARN_SHARE, 0),
        at(p.resolve_minutes, 1.0, c.sla_paused_minutes),
        at(p.resolve_minutes, WARN_SHARE, c.sla_paused_minutes),
    ] {
        due.push(match t {
            Some(t) => Some(schedule::to_utc(&mut **tx, t, &tz).await?),
            None => None,
        });
    }
    sqlx::query(
        "UPDATE tickets SET sla_policy_id = $2, response_due_at = $3, response_warn_at = $4, resolve_due_at = $5,
                resolve_warn_at = $6,
                sla_warned_at = CASE WHEN coalesce($6::timestamptz, $4::timestamptz) > now() THEN NULL ELSE sla_warned_at END,
                sla_breached_at = CASE WHEN coalesce($5::timestamptz, $3::timestamptz) > now() THEN NULL ELSE sla_breached_at END
          WHERE id = $1",
    )
    .bind(ticket)
    .bind(p.id)
    .bind(due[0])
    .bind(due[1])
    .bind(due[2])
    .bind(due[3])
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Смена статуса: начало и конец паузы.
pub async fn on_status(
    tx: &mut Transaction<'_, Postgres>,
    ticket: Uuid,
    from: &str,
    to: &str,
) -> Result<(), sqlx::Error> {
    let cfg: HelpdeskSettings = settings::load(&mut **tx).await?;
    match (paused(from, &cfg), paused(to, &cfg)) {
        (false, true) => {
            sqlx::query("UPDATE tickets SET sla_paused_at = now() WHERE id = $1 AND sla_paused_at IS NULL")
                .bind(ticket)
                .execute(&mut **tx)
                .await?;
        }
        (true, false) => {
            let c = clock(tx, ticket).await?;
            let Some(since) = c.sla_paused_at else { return Ok(()) };
            let (sch, tz) = schedule::for_site(tx, c.site_id, &cfg).await?;
            let a = schedule::to_local(&mut **tx, since, &tz).await?;
            let b = schedule::to_local(&mut **tx, OffsetDateTime::now_utc(), &tz).await?;
            let add = i32::try_from(sch.work_minutes_between(a, b)).unwrap_or(i32::MAX / 2);
            sqlx::query(
                "UPDATE tickets SET sla_paused_at = NULL, sla_paused_minutes = sla_paused_minutes + $2 WHERE id = $1",
            )
            .bind(ticket)
            .bind(add)
            .execute(&mut **tx)
            .await?;
            apply(tx, ticket).await?;
        }
        _ => {}
    }
    Ok(())
}

// ------------------------------------------------------------------ Фоновая проверка

/// «Скоро срок» и «просрочено»: колокольчик и письмо исполнителю; о просрочке — и суперадминистраторам.
pub async fn check(core: &Core) -> ApiResult<usize> {
    let c = notify::ctx(core).await?;
    let numbers: Vec<i64> = sqlx::query_scalar(
        "SELECT number FROM tickets WHERE status = ANY($1) AND sla_paused_at IS NULL AND (
             (sla_warned_at IS NULL AND (resolve_warn_at <= now() OR (taken_at IS NULL AND response_warn_at <= now())))
          OR (sla_breached_at IS NULL AND (resolve_due_at <= now() OR (taken_at IS NULL AND response_due_at <= now()))))
          LIMIT 500",
    )
    .bind(OPEN_STATUSES)
    .fetch_all(&core.db)
    .await?;
    let mut n = 0;
    for number in numbers {
        match check_one(core, &c, number).await {
            Ok(true) => n += 1,
            Ok(false) => {}
            Err(e) => tracing::warn!(ticket = number, error = %e, "проверка срока SLA не выполнена"),
        }
    }
    Ok(n)
}

async fn check_one(core: &Core, c: &notify::Ctx, number: i64) -> ApiResult<bool> {
    let mut tx = core.db.begin().await?;
    // Пока шёл обход, заявку могли решить или поставить на паузу.
    let t: Option<T> =
        sqlx::query_as(&select_t("WHERE number = $1 AND status = ANY($2) AND sla_paused_at IS NULL FOR UPDATE"))
            .bind(number)
            .bind(OPEN_STATUSES)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(t) = t else { return Ok(false) };
    let (breached, warned_at, breached_at): (bool, Option<OffsetDateTime>, Option<OffsetDateTime>) = sqlx::query_as(
        "SELECT coalesce(resolve_due_at <= now(), false) OR coalesce(taken_at IS NULL AND response_due_at <= now(), false),
                sla_warned_at, sla_breached_at FROM tickets WHERE id = $1",
    )
    .bind(t.id)
    .fetch_one(&mut *tx)
    .await?;
    if breached && breached_at.is_none() {
        sqlx::query(
            "UPDATE tickets SET sla_breached_at = now(), sla_warned_at = coalesce(sla_warned_at, now()) WHERE id = $1",
        )
        .bind(t.id)
        .execute(&mut *tx)
        .await?;
        event(&mut tx, t.id, None, "sla_breached", Value::Null, Value::Null).await?;
        if let Some(a) = t.assignee_id {
            notify::bell(&mut tx, a, "helpdesk.sla_breached", &t).await?;
            notify::to_user(&mut tx, a, "helpdesk.sla_breached", &t, &Msg::SlaBreached, c).await?;
        }
        pult_core::notify::notify_superadmins(
            &mut *tx,
            "helpdesk.sla_breached",
            json!({"number": t.number, "title": t.title}),
        )
        .await?;
        crate::rules::fire(&mut tx, &t, crate::rules::Trigger::SlaBreached).await?;
    } else if !breached && warned_at.is_none() {
        sqlx::query("UPDATE tickets SET sla_warned_at = now() WHERE id = $1").bind(t.id).execute(&mut *tx).await?;
        event(&mut tx, t.id, None, "sla_warning", Value::Null, Value::Null).await?;
        if let Some(a) = t.assignee_id {
            notify::bell(&mut tx, a, "helpdesk.sla_warning", &t).await?;
            notify::to_user(&mut tx, a, "helpdesk.sla_warning", &t, &Msg::SlaWarning, c).await?;
        }
        crate::rules::fire(&mut tx, &t, crate::rules::Trigger::SlaWarning).await?;
    } else {
        return Ok(false);
    }
    tx.commit().await?;
    Ok(true)
}

// ------------------------------------------------------------------ Админка: календари и политики

pub fn routes<S>() -> OpenApiRouter<S>
where
    S: Clone + Send + Sync + 'static,
    Core: FromRef<S>,
    HelpdeskState: FromRef<S>,
{
    OpenApiRouter::new()
        .routes(routes!(list_calendars, create_calendar))
        .routes(routes!(update_calendar, delete_calendar))
        .routes(routes!(list_days, put_days))
        .routes(routes!(import_days))
        .routes(routes!(set_site_calendar))
        .routes(routes!(list_policies, create_policy))
        .routes(routes!(update_policy, delete_policy))
}

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct Calendar {
    pub id: Uuid,
    pub name: String,
    #[schema(value_type = Vec<DayHours>)]
    pub hours: Value,
    pub sites: Vec<String>,
}

#[derive(Deserialize, ToSchema)]
pub struct CalendarInput {
    pub name: String,
    pub hours: Vec<DayHours>,
}

fn check_calendar(b: &CalendarInput) -> ApiResult<()> {
    if b.name.trim().is_empty() {
        return Err(ApiError::bad_request("invalid_calendar", "Укажите название."));
    }
    schedule::Schedule::from_week(&b.hours).map_err(|m| ApiError::bad_request("invalid_calendar", m))?;
    Ok(())
}

async fn admin_audit(
    core: &Core,
    user: &CurrentUser,
    action: &'static str,
    kind: &'static str,
    id: Uuid,
    d: Value,
) -> ApiResult<()> {
    audit::record(&core.db, &user.actor, Entry::new(action, Source::Portal).object(kind, id).details(d)).await?;
    Ok(())
}

/// Рабочие календари.
#[utoipa::path(get, path = "/api/admin/helpdesk/calendars", tag = "helpdesk-admin", responses((status = 200, body = Vec<Calendar>)))]
async fn list_calendars(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<Vec<Calendar>>> {
    user.require(&perm::ADMIN)?;
    let rows = sqlx::query_as(
        "SELECT c.id, c.name, c.hours,
                coalesce(array_agg(s.name ORDER BY s.name) FILTER (WHERE s.id IS NOT NULL), '{}') AS sites
           FROM calendars c LEFT JOIN sites s ON s.calendar_id = c.id GROUP BY c.id ORDER BY c.name",
    )
    .fetch_all(&core.db)
    .await?;
    Ok(Json(rows))
}

#[utoipa::path(post, path = "/api/admin/helpdesk/calendars", tag = "helpdesk-admin", request_body = CalendarInput, responses((status = 201, body = Object)))]
async fn create_calendar(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<CalendarInput>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    user.require(&perm::ADMIN)?;
    check_calendar(&b)?;
    let id: Uuid = sqlx::query_scalar("INSERT INTO calendars (name, hours) VALUES ($1, $2) RETURNING id")
        .bind(b.name.trim())
        .bind(json!(b.hours))
        .fetch_one(&core.db)
        .await
        .map_err(dup)?;
    admin_audit(&core, &user, "helpdesk.calendar.create", "calendar", id, json!({"name": b.name.trim()})).await?;
    Ok((StatusCode::CREATED, Json(json!({"id": id}))))
}

fn dup(e: sqlx::Error) -> ApiError {
    match &e {
        sqlx::Error::Database(d) if d.is_unique_violation() => {
            ApiError::bad_request("duplicate", "Запись с таким названием уже есть.")
        }
        _ => e.into(),
    }
}

#[utoipa::path(put, path = "/api/admin/helpdesk/calendars/{id}", tag = "helpdesk-admin", params(("id" = Uuid, Path)), request_body = CalendarInput, responses((status = 204)))]
async fn update_calendar(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<CalendarInput>,
) -> ApiResult<StatusCode> {
    user.require(&perm::ADMIN)?;
    check_calendar(&b)?;
    let n = sqlx::query("UPDATE calendars SET name = $2, hours = $3, updated_at = now() WHERE id = $1")
        .bind(id)
        .bind(b.name.trim())
        .bind(json!(b.hours))
        .execute(&core.db)
        .await
        .map_err(dup)?
        .rows_affected();
    if n == 0 {
        return Err(ApiError::NotFound);
    }
    admin_audit(&core, &user, "helpdesk.calendar.update", "calendar", id, json!({"name": b.name.trim()})).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(delete, path = "/api/admin/helpdesk/calendars/{id}", tag = "helpdesk-admin", params(("id" = Uuid, Path)), responses((status = 204)))]
async fn delete_calendar(State(core): State<Core>, user: CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    user.require(&perm::ADMIN)?;
    let n = sqlx::query("DELETE FROM calendars WHERE id = $1").bind(id).execute(&core.db).await?.rows_affected();
    if n == 0 {
        return Err(ApiError::NotFound);
    }
    admin_audit(&core, &user, "helpdesk.calendar.delete", "calendar", id, json!({})).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Особый день: праздник (без часов) или перенос / сокращённый день.
#[derive(Serialize, Deserialize, ToSchema, Clone, Debug, PartialEq, Eq)]
pub struct SpecialDay {
    /// `2026-12-31`.
    pub day: String,
    /// Пусто — выходной.
    pub start: Option<String>,
    pub end: Option<String>,
    #[serde(default)]
    pub note: String,
}

#[utoipa::path(get, path = "/api/admin/helpdesk/calendars/{id}/days", tag = "helpdesk-admin", params(("id" = Uuid, Path)), responses((status = 200, body = Vec<SpecialDay>)))]
async fn list_days(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Vec<SpecialDay>>> {
    user.require(&perm::ADMIN)?;
    let rows: Vec<(Date, Option<Time>, Option<Time>, String)> = sqlx::query_as(
        "SELECT day, start_time, end_time, coalesce(note, '') FROM calendar_days WHERE calendar_id = $1 ORDER BY day",
    )
    .bind(id)
    .fetch_all(&core.db)
    .await?;
    let hm = |t: Time| format!("{:02}:{:02}", t.hour(), t.minute());
    Ok(Json(
        rows.into_iter()
            .map(|(d, s, e, note)| SpecialDay { day: d.to_string(), start: s.map(hm), end: e.map(hm), note })
            .collect(),
    ))
}

type DayRow = (Date, Option<Time>, Option<Time>, String);

fn parse_day(d: &SpecialDay) -> Result<DayRow, String> {
    let fmt = time::macros::format_description!("[year]-[month]-[day]");
    let day = Date::parse(d.day.trim(), fmt).map_err(|_| format!("Дата «{}» — нужен формат ГГГГ-ММ-ДД.", d.day))?;
    let (s, e) = match (d.start.as_deref().filter(|s| !s.is_empty()), d.end.as_deref().filter(|s| !s.is_empty())) {
        (None, None) => (None, None),
        (Some(s), Some(e)) => {
            let (s, e) = (schedule::parse_time(s)?, schedule::parse_time(e)?);
            if s >= e {
                return Err(format!("{}: начало дня позже конца.", d.day));
            }
            (Some(s), Some(e))
        }
        _ => return Err(format!("{}: укажите и начало, и конец дня или ничего.", d.day)),
    };
    Ok((day, s, e, d.note.trim().chars().take(200).collect()))
}

async fn save_days(core: &Core, id: Uuid, rows: &[DayRow], replace: bool) -> ApiResult<usize> {
    let mut tx = core.db.begin().await?;
    let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM calendars WHERE id = $1 FOR UPDATE)")
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    if !exists {
        return Err(ApiError::NotFound);
    }
    if replace {
        sqlx::query("DELETE FROM calendar_days WHERE calendar_id = $1").bind(id).execute(&mut *tx).await?;
    }
    for (d, s, e, note) in rows {
        sqlx::query(
            "INSERT INTO calendar_days (calendar_id, day, start_time, end_time, note) VALUES ($1, $2, $3, $4, nullif($5, ''))
             ON CONFLICT (calendar_id, day) DO UPDATE SET start_time = EXCLUDED.start_time, end_time = EXCLUDED.end_time, note = EXCLUDED.note",
        )
        .bind(id)
        .bind(d)
        .bind(s)
        .bind(e)
        .bind(note)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(rows.len())
}

/// Заменить список особых дней целиком.
#[utoipa::path(put, path = "/api/admin/helpdesk/calendars/{id}/days", tag = "helpdesk-admin", params(("id" = Uuid, Path)), request_body = Vec<SpecialDay>, responses((status = 204)))]
async fn put_days(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<Vec<SpecialDay>>,
) -> ApiResult<StatusCode> {
    user.require(&perm::ADMIN)?;
    if b.len() > 2000 {
        return Err(ApiError::bad_request("invalid_calendar", "Слишком много дней."));
    }
    let rows = b
        .iter()
        .map(parse_day)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|m| ApiError::bad_request("invalid_calendar", m))?;
    save_days(&core, id, &rows, true).await?;
    admin_audit(&core, &user, "helpdesk.calendar.days", "calendar", id, json!({"days": rows.len()})).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, ToSchema)]
pub struct ImportInput {
    /// Содержимое файла: CSV (`дата;начало;конец;примечание`) или iCalendar (ICS).
    pub content: String,
}

#[derive(Serialize, ToSchema)]
pub struct ImportResult {
    pub imported: usize,
}

/// Разобрать производственный календарь: CSV или ICS. В ICS событие на весь день — праздник.
pub fn parse_import(content: &str) -> Result<Vec<SpecialDay>, String> {
    let mut out = Vec::new();
    if content.contains("BEGIN:VCALENDAR") {
        let mut day: Option<String> = None;
        let mut note = String::new();
        // Строки ICS могут переноситься: продолжение начинается с пробела.
        let unfolded = content.replace("\r\n ", "").replace("\n ", "");
        for line in unfolded.lines().map(str::trim) {
            if line == "BEGIN:VEVENT" {
                day = None;
                note.clear();
            } else if let Some(v) = line.strip_prefix("DTSTART") {
                let v = v.rsplit(':').next().unwrap_or("");
                if v.len() >= 8 && v[..8].bytes().all(|b| b.is_ascii_digit()) {
                    day = Some(format!("{}-{}-{}", &v[..4], &v[4..6], &v[6..8]));
                }
            } else if let Some(v) = line.strip_prefix("SUMMARY") {
                note = v.split_once(':').map(|x| x.1).unwrap_or("").to_string();
            } else if line == "END:VEVENT"
                && let Some(d) = day.take()
            {
                out.push(SpecialDay { day: d, start: None, end: None, note: note.clone() });
            }
        }
    } else {
        for (i, line) in content.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let sep = if line.contains(';') { ';' } else { ',' };
            let cols: Vec<&str> = line.split(sep).map(str::trim).collect();
            let first = cols.first().copied().unwrap_or("");
            // Заголовок пропускаем.
            if i == 0 && !first.starts_with(|c: char| c.is_ascii_digit()) {
                continue;
            }
            // Поддерживаем и 31.12.2026.
            let day = match first.split('.').collect::<Vec<_>>().as_slice() {
                [d, m, y] if y.len() == 4 => format!("{y}-{m:0>2}-{d:0>2}"),
                _ => first.to_string(),
            };
            let opt = |k: usize| cols.get(k).map(|s| s.to_string()).filter(|s| !s.is_empty());
            out.push(SpecialDay { day, start: opt(1), end: opt(2), note: opt(3).unwrap_or_default() });
        }
    }
    if out.is_empty() {
        return Err("В файле не нашлось ни одного дня.".into());
    }
    Ok(out)
}

/// Импорт праздников и переносов (дополняет список).
#[utoipa::path(post, path = "/api/admin/helpdesk/calendars/{id}/import", tag = "helpdesk-admin", params(("id" = Uuid, Path)), request_body = ImportInput, responses((status = 200, body = ImportResult)))]
async fn import_days(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<ImportInput>,
) -> ApiResult<Json<ImportResult>> {
    user.require(&perm::ADMIN)?;
    if b.content.len() > 2 * 1024 * 1024 {
        return Err(ApiError::bad_request("invalid_calendar", "Файл слишком большой."));
    }
    let bad = |m: String| ApiError::bad_request("invalid_calendar", m);
    let days = parse_import(&b.content).map_err(bad)?;
    let rows = days.iter().map(parse_day).collect::<Result<Vec<_>, _>>().map_err(bad)?;
    let imported = save_days(&core, id, &rows, false).await?;
    admin_audit(&core, &user, "helpdesk.calendar.import", "calendar", id, json!({"days": imported})).await?;
    Ok(Json(ImportResult { imported }))
}

#[derive(Deserialize, ToSchema)]
pub struct SiteCalendarInput {
    /// Пусто — общий график из настроек.
    pub calendar_id: Option<Uuid>,
}

/// Календарь площадки.
#[utoipa::path(put, path = "/api/admin/sites/{id}/calendar", tag = "helpdesk-admin", params(("id" = Uuid, Path)), request_body = SiteCalendarInput, responses((status = 204)))]
async fn set_site_calendar(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<SiteCalendarInput>,
) -> ApiResult<StatusCode> {
    user.require(&perm::ADMIN)?;
    let n = sqlx::query("UPDATE sites SET calendar_id = $2 WHERE id = $1")
        .bind(id)
        .bind(b.calendar_id)
        .execute(&core.db)
        .await?
        .rows_affected();
    if n == 0 {
        return Err(ApiError::NotFound);
    }
    admin_audit(&core, &user, "site.calendar", "site", id, json!({"calendar": b.calendar_id})).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct SlaPolicy {
    pub id: Uuid,
    pub name: String,
    pub site_id: Option<Uuid>,
    pub category_id: Option<Uuid>,
    pub priority: Option<String>,
    pub response_minutes: Option<i32>,
    pub resolve_minutes: Option<i32>,
    pub sort: i32,
    pub is_active: bool,
}

#[derive(Deserialize, ToSchema)]
pub struct PolicyInput {
    pub name: String,
    pub site_id: Option<Uuid>,
    pub category_id: Option<Uuid>,
    pub priority: Option<String>,
    /// Первая реакция (до «В работе»), рабочих минут.
    pub response_minutes: Option<i32>,
    /// Решение, рабочих минут.
    pub resolve_minutes: Option<i32>,
    #[serde(default = "hundred")]
    pub sort: i32,
    #[serde(default = "yes")]
    pub is_active: bool,
}

fn hundred() -> i32 {
    100
}
fn yes() -> bool {
    true
}

fn check_policy(b: &PolicyInput) -> ApiResult<()> {
    let bad = |m: &str| Err(ApiError::bad_request("invalid_policy", m));
    if b.name.trim().is_empty() {
        return bad("Укажите название.");
    }
    if b.priority.as_deref().is_some_and(|p| !PRIORITIES.contains(&p)) {
        return bad("Неверный приоритет.");
    }
    let ok = |m: Option<i32>| m.is_none_or(|m| (1..=60 * 24 * 90).contains(&m));
    if !ok(b.response_minutes)
        || !ok(b.resolve_minutes)
        || (b.response_minutes.is_none() && b.resolve_minutes.is_none())
    {
        return bad("Сроки — в рабочих минутах, хотя бы один.");
    }
    Ok(())
}

#[utoipa::path(get, path = "/api/admin/helpdesk/sla", tag = "helpdesk-admin", responses((status = 200, body = Vec<SlaPolicy>)))]
async fn list_policies(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<Vec<SlaPolicy>>> {
    user.require(&perm::ADMIN)?;
    let rows = sqlx::query_as(
        "SELECT id, name, site_id, category_id, priority, response_minutes, resolve_minutes, sort, is_active
           FROM sla_policies ORDER BY sort, name",
    )
    .fetch_all(&core.db)
    .await?;
    Ok(Json(rows))
}

#[utoipa::path(post, path = "/api/admin/helpdesk/sla", tag = "helpdesk-admin", request_body = PolicyInput, responses((status = 201, body = Object)))]
async fn create_policy(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<PolicyInput>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    user.require(&perm::ADMIN)?;
    check_policy(&b)?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO sla_policies (name, site_id, category_id, priority, response_minutes, resolve_minutes, sort, is_active)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id",
    )
    .bind(b.name.trim())
    .bind(b.site_id)
    .bind(b.category_id)
    .bind(&b.priority)
    .bind(b.response_minutes)
    .bind(b.resolve_minutes)
    .bind(b.sort)
    .bind(b.is_active)
    .fetch_one(&core.db)
    .await?;
    admin_audit(&core, &user, "helpdesk.sla.create", "sla_policy", id, json!({"name": b.name.trim()})).await?;
    Ok((StatusCode::CREATED, Json(json!({"id": id}))))
}

#[utoipa::path(put, path = "/api/admin/helpdesk/sla/{id}", tag = "helpdesk-admin", params(("id" = Uuid, Path)), request_body = PolicyInput, responses((status = 204)))]
async fn update_policy(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<PolicyInput>,
) -> ApiResult<StatusCode> {
    user.require(&perm::ADMIN)?;
    check_policy(&b)?;
    let n = sqlx::query(
        "UPDATE sla_policies SET name = $2, site_id = $3, category_id = $4, priority = $5, response_minutes = $6,
                resolve_minutes = $7, sort = $8, is_active = $9, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(b.name.trim())
    .bind(b.site_id)
    .bind(b.category_id)
    .bind(&b.priority)
    .bind(b.response_minutes)
    .bind(b.resolve_minutes)
    .bind(b.sort)
    .bind(b.is_active)
    .execute(&core.db)
    .await?
    .rows_affected();
    if n == 0 {
        return Err(ApiError::NotFound);
    }
    admin_audit(&core, &user, "helpdesk.sla.update", "sla_policy", id, json!({"name": b.name.trim()})).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(delete, path = "/api/admin/helpdesk/sla/{id}", tag = "helpdesk-admin", params(("id" = Uuid, Path)), responses((status = 204)))]
async fn delete_policy(State(core): State<Core>, user: CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    user.require(&perm::ADMIN)?;
    let n = sqlx::query("DELETE FROM sla_policies WHERE id = $1").bind(id).execute(&core.db).await?.rows_affected();
    if n == 0 {
        return Err(ApiError::NotFound);
    }
    admin_audit(&core, &user, "helpdesk.sla.delete", "sla_policy", id, json!({})).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Пересчитать сроки открытых заявок после правки политик или календаря.
pub async fn recompute_open(core: &Core) -> ApiResult<usize> {
    let ids: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM tickets WHERE status = ANY($1) LIMIT 5000")
        .bind(OPEN_STATUSES)
        .fetch_all(&core.db)
        .await?;
    let mut n = 0;
    for id in &ids {
        let mut tx = core.db.begin().await?;
        match apply(&mut tx, *id).await {
            Ok(()) => {
                tx.commit().await?;
                n += 1;
            }
            Err(e) => tracing::warn!(ticket = %id, error = %e, "сроки SLA не пересчитаны"),
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrix_and_vip() {
        let cfg = HelpdeskSettings::default();
        assert_eq!(matrix_priority(&cfg, "person", "normal", false), "normal");
        assert_eq!(matrix_priority(&cfg, "organization", "blocked", false), "critical");
        assert_eq!(matrix_priority(&cfg, "person", "normal", true), "high", "VIP — на ступень выше");
        assert_eq!(bump("critical"), "critical");
    }

    #[test]
    fn calendar_import_formats() {
        let csv = "дата;начало;конец;примечание\n2026-12-31;;;Новый год\n27.12.2026;09:00;18:00;Перенос\n";
        let d = parse_import(csv).unwrap();
        assert_eq!(d.len(), 2);
        assert_eq!(d[0], SpecialDay { day: "2026-12-31".into(), start: None, end: None, note: "Новый год".into() });
        assert_eq!(d[1].day, "2026-12-27");
        assert_eq!(d[1].start.as_deref(), Some("09:00"));
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nDTSTART;VALUE=DATE:20270101\r\nSUMMARY:Новый год\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let d = parse_import(ics).unwrap();
        assert_eq!(d, vec![SpecialDay { day: "2027-01-01".into(), start: None, end: None, note: "Новый год".into() }]);
        assert!(parse_import("").is_err());
        assert!(
            parse_day(&SpecialDay { day: "31-12-2026".into(), start: None, end: None, note: String::new() }).is_err()
        );
    }
}

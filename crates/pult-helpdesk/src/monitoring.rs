//! Входящие оповещения мониторинга: Prometheus Alertmanager, вебхук Zabbix и любой источник, который
//! умеет POST с JSON. У каждого источника свой токен (`Authorization: Bearer …`), в базе — только хеш.
//!
//! Новое оповещение — уведомление суперадминистраторам и, если у источника включено, заявка. Повтор
//! того же оповещения (тот же отпечаток, пока оно не решено) ничего нового не создаёт. «Решено»
//! закрывает оповещение и пишет об этом в ленту заявки; саму заявку закрывает инженер.

use crate::{
    perm,
    tickets::{self, NewTicket, Who},
};
use axum::{
    Json,
    extract::{FromRef, Path, Query, State},
    http::{HeaderMap, StatusCode, header},
};
use pult_core::{
    ApiError, ApiResult, Core, CurrentUser,
    audit::{self, Entry, Source},
    crypto::{random_token, sha256},
    l10n::{self, tr_to},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::OffsetDateTime;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// Вид уведомления о новом оповещении.
pub const NOTIFY_KIND: &str = "monitoring.alert";
/// Не больше оповещений в одном запросе.
const MAX_ALERTS: usize = 200;
/// Не больше заявок из одного запроса: шквал оповещений не должен завалить очередь.
const MAX_TICKETS: usize = 20;
/// Открытых оповещений одного источника не больше: новые сверх — отбрасываются (утечка токена или
/// «шторм» не заполнят базу).
const MAX_OPEN: i64 = 2000;
/// Заявок от одного источника за час не больше.
const MAX_TICKETS_PER_HOUR: i64 = 100;
/// Новых оповещений с уведомлением суперадминистраторам за час от одного источника.
const MAX_NOTIFY_PER_HOUR: i64 = 100;
const SEVERITIES: [&str; 3] = ["info", "warning", "critical"];

pub fn routes<S>() -> OpenApiRouter<S>
where
    S: Clone + Send + Sync + 'static,
    Core: FromRef<S>,
{
    OpenApiRouter::new()
        .routes(routes!(receive, list_alerts))
        .routes(routes!(list_sources, create_source))
        .routes(routes!(update_source, delete_source))
}

// ------------------------------------------------------------------ Разбор оповещений

/// Оповещение после разбора тела запроса.
#[derive(Debug, PartialEq)]
pub struct Incoming {
    pub fingerprint: String,
    pub title: String,
    pub severity: &'static str,
    pub host: Option<String>,
    pub description: Option<String>,
    pub resolved: bool,
}

fn rank(s: &str) -> u8 {
    match s {
        "critical" => 2,
        "warning" => 1,
        _ => 0,
    }
}

/// Важность словами источника: Alertmanager — `labels.severity`, Zabbix — свои уровни (0–5 или названия).
fn severity(s: Option<&str>) -> &'static str {
    match s.map(|s| s.trim().to_lowercase()).as_deref() {
        Some("critical" | "disaster" | "high" | "emergency" | "alert" | "page" | "error" | "4" | "5") => "critical",
        Some("info" | "information" | "informational" | "notice" | "low" | "none" | "not classified" | "0" | "1") => {
            "info"
        }
        _ => "warning",
    }
}

/// Первое непустое значение по указателям JSON; числа — строкой (id события Zabbix бывает числом).
/// Управляющие символы (кроме перевода строки) убираются: NUL база не примет.
fn pick(v: &Value, ptrs: &[&str]) -> Option<String> {
    ptrs.iter().find_map(|p| match v.pointer(p)? {
        Value::String(s) => {
            let s: String = s.chars().filter(|c| !c.is_control() || *c == '\n').collect();
            Some(s.trim().to_string()).filter(|s| !s.is_empty())
        }
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    })
}

/// Одна строка: для названия, узла и отпечатка.
fn line(s: String) -> String {
    if s.contains('\n') { s.split_whitespace().collect::<Vec<_>>().join(" ") } else { s }
}

fn clip(s: String, n: usize) -> String {
    if s.chars().count() <= n { s } else { s.chars().take(n).collect() }
}

/// Тело запроса: `{"alerts": [...]}` (Alertmanager или пачка своих) либо одно оповещение
/// `{"id", "title", "status", "severity", "host", "description"}`. Без названия оповещение пропускается.
pub fn parse(body: &Value) -> Vec<Incoming> {
    let items: Vec<&Value> = match body.get("alerts").and_then(Value::as_array) {
        Some(list) => list.iter().collect(),
        None => vec![body],
    };
    let group_status = pick(body, &["/status"]);
    items
        .into_iter()
        .take(MAX_ALERTS)
        .filter_map(|a| {
            let title = line(pick(a, &["/annotations/summary", "/labels/alertname", "/title", "/name", "/subject"])?);
            let host = pick(a, &["/labels/instance", "/labels/host", "/host", "/hostname"]).map(line);
            let fingerprint = pick(a, &["/fingerprint", "/id", "/event_id", "/eventid"])
                .map(line)
                .unwrap_or_else(|| format!("{title}|{}", host.as_deref().unwrap_or("")));
            let status = pick(a, &["/status"]).or_else(|| group_status.clone()).unwrap_or_default().to_lowercase();
            Some(Incoming {
                fingerprint: clip(fingerprint, 200),
                title: clip(title, 300),
                severity: severity(pick(a, &["/labels/severity", "/severity", "/priority"]).as_deref()),
                host: host.map(|h| clip(h, 200)),
                description: pick(a, &["/annotations/description", "/description", "/message"]).map(|d| clip(d, 4000)),
                resolved: matches!(status.as_str(), "resolved" | "ok" | "recovered" | "recovery" | "closed" | "0"),
            })
        })
        .collect()
}

// ------------------------------------------------------------------ Приём

#[derive(sqlx::FromRow)]
struct SourceRow {
    id: Uuid,
    name: String,
    create_tickets: bool,
    min_severity: String,
    category_id: Option<Uuid>,
    site_id: Option<Uuid>,
}

#[derive(Serialize, ToSchema)]
pub struct Received {
    /// Новых оповещений.
    pub opened: usize,
    /// Повторов уже открытых.
    pub repeated: usize,
    /// Решённых.
    pub resolved: usize,
    /// Создано заявок.
    pub tickets: usize,
    /// Отброшено новых: у источника слишком много открытых оповещений.
    pub dropped: usize,
}

/// Принять оповещения источника. Токен — `Authorization: Bearer pult_mon_…` из настроек интеграций.
#[utoipa::path(post, path = "/api/helpdesk/monitoring/alerts", tag = "integrations", request_body = Object,
    responses((status = 200, body = Received), (status = 401, body = pult_core::error::ErrorBody)))]
async fn receive(State(core): State<Core>, headers: HeaderMap, Json(body): Json<Value>) -> ApiResult<Json<Received>> {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|t| t.starts_with("pult_mon_"))
        .ok_or(ApiError::Unauthorized)?;
    let src: SourceRow = sqlx::query_as(
        "UPDATE alert_sources SET last_alert_at = now() WHERE token_hash = $1 AND is_active
         RETURNING id, name, create_tickets, min_severity, category_id, site_id",
    )
    .bind(sha256(token.as_bytes()))
    .fetch_optional(&core.db)
    .await?
    .ok_or(ApiError::Unauthorized)?;
    let lang = l10n::default_lang(&core.db).await;
    let mut res = Received { opened: 0, repeated: 0, resolved: 0, tickets: 0, dropped: 0 };
    // Два запроса — каждый по своему индексу: открытые (alerts_firing) и за последний час (alerts_source_started).
    let open: i64 = sqlx::query_scalar("SELECT count(*) FROM alerts WHERE source_id = $1 AND status = 'firing'")
        .bind(src.id)
        .fetch_one(&core.db)
        .await?;
    let (recent_new, recent_tickets): (i64, i64) = sqlx::query_as(
        "SELECT count(*), count(*) FILTER (WHERE ticket_id IS NOT NULL)
           FROM alerts WHERE source_id = $1 AND started_at > now() - interval '1 hour'",
    )
    .bind(src.id)
    .fetch_one(&core.db)
    .await?;
    let mut room = MAX_OPEN - open;
    let ticket_room = usize::try_from(MAX_TICKETS_PER_HOUR - recent_tickets).unwrap_or(0).min(MAX_TICKETS);
    // Колокольчик суперадминистраторам — не чаще предела в час: «решено/снова горит» по кругу не завалит их.
    let mut notify_room = MAX_NOTIFY_PER_HOUR - recent_new;
    for a in parse(&body) {
        if a.resolved {
            resolve(&core, src.id, &a, &mut res).await?;
            continue;
        }
        // Повтор открытого — обновляется всегда; новое — пока у источника есть место.
        let row: Option<(Uuid, bool, Option<Uuid>)> = sqlx::query_as(
            "INSERT INTO alerts (source_id, fingerprint, title, severity, host, description)
             SELECT $1, $2, $3, $4, $5, $6
              WHERE $7 OR EXISTS (SELECT 1 FROM alerts WHERE source_id = $1 AND fingerprint = $2 AND status = 'firing')
             ON CONFLICT (source_id, fingerprint) WHERE status = 'firing'
             DO UPDATE SET title = EXCLUDED.title, severity = EXCLUDED.severity, host = EXCLUDED.host,
                           description = EXCLUDED.description, updated_at = now()
             RETURNING id, (xmax = 0), ticket_id",
        )
        .bind(src.id)
        .bind(&a.fingerprint)
        .bind(&a.title)
        .bind(a.severity)
        .bind(&a.host)
        .bind(&a.description)
        .bind(room > 0)
        .fetch_optional(&core.db)
        .await?;
        let Some((id, inserted, ticket)) = row else {
            res.dropped += 1;
            continue;
        };
        if inserted {
            res.opened += 1;
            room -= 1;
            if notify_room > 0 {
                notify_room -= 1;
                let payload = json!({"source": src.name, "title": a.title, "host": a.host, "severity": a.severity});
                pult_core::notify::notify_superadmins(&core.db, NOTIFY_KIND, payload).await?;
            }
        } else {
            res.repeated += 1;
        }
        // Заявка — и для повтора, если в прошлый раз её создать не удалось (сбой, предел за запрос).
        if ticket.is_none()
            && src.create_tickets
            && rank(a.severity) >= rank(&src.min_severity)
            && res.tickets < ticket_room
        {
            // Отметка «заявка создаётся»: параллельный повтор её пропустит. Соединение на время создания
            // заявки не держим — иначе шторм оповещений выберет весь пул. Сбой — отметка истечёт за 5 минут.
            let free: Option<Uuid> = sqlx::query_scalar(
                "UPDATE alerts SET ticket_claimed_at = now()
                  WHERE id = $1 AND ticket_id IS NULL AND status = 'firing'
                    AND (ticket_claimed_at IS NULL OR ticket_claimed_at < now() - interval '5 minutes')
                  RETURNING id",
            )
            .bind(id)
            .fetch_optional(&core.db)
            .await?;
            if free.is_none() {
                continue;
            }
            let mut text = Vec::new();
            for (label, value) in [
                ("Источник", Some(src.name.clone())),
                ("Узел", a.host.clone()),
                ("Важность", Some(tr_to(lang, severity_word(a.severity)).into_owned())),
            ] {
                if let Some(v) = value {
                    text.push(format!("{}: {v}", tr_to(lang, label)));
                }
            }
            if let Some(d) = &a.description {
                text.push(String::new());
                text.push(d.clone());
            }
            let created = tickets::create(
                &core,
                &Who { engineer: true, ..Who::system() },
                NewTicket {
                    title: clip(a.title.clone(), 200),
                    description: text.join("\n"),
                    category_id: src.category_id,
                    site_id: src.site_id,
                    channel: "monitoring".into(),
                    form_answers: json!({}),
                    system: true,
                    ..Default::default()
                },
            )
            .await?;
            sqlx::query(
                "UPDATE alerts SET ticket_id = (SELECT id FROM tickets WHERE number = $2), ticket_claimed_at = NULL
                  WHERE id = $1",
            )
            .bind(id)
            .bind(created.number)
            .execute(&core.db)
            .await?;
            res.tickets += 1;
        }
    }
    Ok(Json(res))
}

fn severity_word(s: &str) -> &'static str {
    match s {
        "critical" => "критично",
        "info" => "информация",
        _ => "предупреждение",
    }
}

/// «Решено»: оповещение закрывается, в ленте заявки — запись.
async fn resolve(core: &Core, source: Uuid, a: &Incoming, res: &mut Received) -> ApiResult<()> {
    let mut tx = core.db.begin().await?;
    let row: Option<(Option<Uuid>,)> = sqlx::query_as(
        "UPDATE alerts SET status = 'resolved', resolved_at = now(), updated_at = now()
          WHERE source_id = $1 AND fingerprint = $2 AND status = 'firing' RETURNING ticket_id",
    )
    .bind(source)
    .bind(&a.fingerprint)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some((ticket,)) = row {
        res.resolved += 1;
        if let Some(t) = ticket {
            tickets::event(&mut tx, t, None, "alert_resolved", Value::Null, json!({"title": a.title})).await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

// ------------------------------------------------------------------ Список оповещений

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct Alert {
    pub id: Uuid,
    pub source_name: String,
    pub title: String,
    /// `info`, `warning`, `critical`.
    pub severity: String,
    pub host: Option<String>,
    pub description: Option<String>,
    /// `firing` или `resolved`.
    pub status: String,
    pub ticket_number: Option<i64>,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub started_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub resolved_at: Option<OffsetDateTime>,
}

#[derive(Deserialize, IntoParams)]
pub struct AlertQuery {
    /// Только открытые.
    #[serde(default)]
    pub open: bool,
}

/// Оповещения мониторинга: открытые сверху, затем недавние решённые.
#[utoipa::path(get, path = "/api/helpdesk/monitoring/alerts", tag = "integrations", params(AlertQuery),
    responses((status = 200, body = Vec<Alert>)))]
async fn list_alerts(
    State(core): State<Core>,
    user: CurrentUser,
    Query(q): Query<AlertQuery>,
) -> ApiResult<Json<Vec<Alert>>> {
    user.require(&perm::WORK)?;
    // Сначала открытые, затем решённые; каждая часть — по своему частичному индексу.
    let rows = sqlx::query_as(
        "SELECT * FROM (
           (SELECT a.id, s.name AS source_name, a.title, a.severity, a.host, a.description, a.status,
                   t.number AS ticket_number, a.started_at, a.resolved_at
              FROM alerts a JOIN alert_sources s ON s.id = a.source_id
              LEFT JOIN tickets t ON t.id = a.ticket_id
             WHERE a.status = 'firing' ORDER BY a.started_at DESC LIMIT 200)
           UNION ALL
           (SELECT a.id, s.name, a.title, a.severity, a.host, a.description, a.status,
                   t.number, a.started_at, a.resolved_at
              FROM alerts a JOIN alert_sources s ON s.id = a.source_id
              LEFT JOIN tickets t ON t.id = a.ticket_id
             WHERE a.status = 'resolved' AND NOT $1 ORDER BY a.started_at DESC LIMIT 200)
         ) x ORDER BY status = 'resolved', started_at DESC LIMIT 200",
    )
    .bind(q.open)
    .fetch_all(&core.db)
    .await?;
    Ok(Json(rows))
}

// ------------------------------------------------------------------ Источники

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct AlertSource {
    pub id: Uuid,
    pub name: String,
    pub create_tickets: bool,
    pub min_severity: String,
    pub category_id: Option<Uuid>,
    pub site_id: Option<Uuid>,
    pub is_active: bool,
    pub open_alerts: i64,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub last_alert_at: Option<OffsetDateTime>,
}

#[derive(Deserialize, ToSchema)]
pub struct AlertSourceInput {
    pub name: String,
    #[serde(default)]
    pub create_tickets: bool,
    /// Заявка — на оповещения не ниже этой важности: `info`, `warning`, `critical`.
    pub min_severity: String,
    pub category_id: Option<Uuid>,
    pub site_id: Option<Uuid>,
    #[serde(default = "yes")]
    pub is_active: bool,
}

fn yes() -> bool {
    true
}

#[derive(Serialize, ToSchema)]
pub struct CreatedSource {
    pub id: Uuid,
    /// Показывается один раз: в базе хранится только хеш.
    pub token: String,
}

async fn check_source(core: &Core, b: &AlertSourceInput) -> ApiResult<()> {
    let bad = |m: &str| ApiError::bad_request("invalid_alert_source", m.to_string());
    if b.name.trim().is_empty() || b.name.chars().count() > 200 {
        return Err(bad("Укажите название."));
    }
    if !SEVERITIES.contains(&b.min_severity.as_str()) {
        return Err(bad("Важность — info, warning или critical."));
    }
    let (category_ok, site_ok): (bool, bool) = sqlx::query_as(
        "SELECT $1::uuid IS NULL OR EXISTS (SELECT 1 FROM ticket_categories WHERE id = $1 AND is_active),
                $2::uuid IS NULL OR EXISTS (SELECT 1 FROM sites WHERE id = $2 AND is_active)",
    )
    .bind(b.category_id)
    .bind(b.site_id)
    .fetch_one(&core.db)
    .await?;
    if !category_ok {
        return Err(bad("Категория не найдена."));
    }
    if !site_ok {
        return Err(bad("Площадка не найдена."));
    }
    Ok(())
}

#[utoipa::path(get, path = "/api/admin/monitoring/sources", tag = "integrations",
    responses((status = 200, body = Vec<AlertSource>)))]
async fn list_sources(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<Vec<AlertSource>>> {
    user.require(&perm::ADMIN)?;
    let rows = sqlx::query_as(
        "SELECT s.id, s.name, s.create_tickets, s.min_severity, s.category_id, s.site_id, s.is_active, s.last_alert_at,
                (SELECT count(*) FROM alerts a WHERE a.source_id = s.id AND a.status = 'firing') AS open_alerts
           FROM alert_sources s ORDER BY lower(s.name)",
    )
    .fetch_all(&core.db)
    .await?;
    Ok(Json(rows))
}

/// Новый источник: токен показывается один раз.
#[utoipa::path(post, path = "/api/admin/monitoring/sources", tag = "integrations", request_body = AlertSourceInput,
    responses((status = 201, body = CreatedSource)))]
async fn create_source(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<AlertSourceInput>,
) -> ApiResult<(StatusCode, Json<CreatedSource>)> {
    user.require(&perm::ADMIN)?;
    check_source(&core, &b).await?;
    let token = format!("pult_mon_{}", random_token(32).map_err(ApiError::internal)?);
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO alert_sources (name, token_hash, create_tickets, min_severity, category_id, site_id, is_active)
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
    )
    .bind(b.name.trim())
    .bind(sha256(token.as_bytes()))
    .bind(b.create_tickets)
    .bind(&b.min_severity)
    .bind(b.category_id)
    .bind(b.site_id)
    .bind(b.is_active)
    .fetch_one(&core.db)
    .await?;
    audit::record(
        &core.db,
        &user.actor,
        Entry::new("monitoring.source.create", Source::Portal).object("alert_source", id),
    )
    .await?;
    Ok((StatusCode::CREATED, Json(CreatedSource { id, token })))
}

#[utoipa::path(put, path = "/api/admin/monitoring/sources/{id}", tag = "integrations", params(("id" = Uuid, Path)),
    request_body = AlertSourceInput, responses((status = 204)))]
async fn update_source(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<AlertSourceInput>,
) -> ApiResult<StatusCode> {
    user.require(&perm::ADMIN)?;
    check_source(&core, &b).await?;
    let n = sqlx::query(
        "UPDATE alert_sources SET name = $2, create_tickets = $3, min_severity = $4, category_id = $5, site_id = $6,
                is_active = $7 WHERE id = $1",
    )
    .bind(id)
    .bind(b.name.trim())
    .bind(b.create_tickets)
    .bind(&b.min_severity)
    .bind(b.category_id)
    .bind(b.site_id)
    .bind(b.is_active)
    .execute(&core.db)
    .await?
    .rows_affected();
    if n == 0 {
        return Err(ApiError::NotFound);
    }
    let details = json!({"create_tickets": b.create_tickets, "min_severity": b.min_severity, "is_active": b.is_active});
    let entry = Entry::new("monitoring.source.update", Source::Portal).object("alert_source", id).details(details);
    audit::record(&core.db, &user.actor, entry).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Удалить источник вместе с его оповещениями (заявки остаются).
#[utoipa::path(delete, path = "/api/admin/monitoring/sources/{id}", tag = "integrations", params(("id" = Uuid, Path)),
    responses((status = 204)))]
async fn delete_source(State(core): State<Core>, user: CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    user.require(&perm::ADMIN)?;
    let n = sqlx::query("DELETE FROM alert_sources WHERE id = $1").bind(id).execute(&core.db).await?.rows_affected();
    if n == 0 {
        return Err(ApiError::NotFound);
    }
    audit::record(
        &core.db,
        &user.actor,
        Entry::new("monitoring.source.delete", Source::Portal).object("alert_source", id),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::{parse, severity};
    use serde_json::json;

    #[test]
    fn alertmanager_payload() {
        let body = json!({"version": "4", "status": "firing", "alerts": [
            {"status": "firing", "fingerprint": "abc", "labels": {"alertname": "DiskFull", "severity": "critical", "instance": "db1:9100"},
             "annotations": {"summary": "Disk almost full", "description": "95% used"}},
            {"status": "resolved", "fingerprint": "def", "labels": {"alertname": "HighLoad"}},
            {"status": "firing", "labels": {}}
        ]});
        let a = parse(&body);
        assert_eq!(a.len(), 2, "без названия — пропуск");
        assert_eq!(a[0].fingerprint, "abc");
        assert_eq!(a[0].title, "Disk almost full");
        assert_eq!((a[0].severity, a[0].host.as_deref(), a[0].resolved), ("critical", Some("db1:9100"), false));
        assert_eq!((a[1].title.as_str(), a[1].severity, a[1].resolved), ("HighLoad", "warning", true));
    }

    #[test]
    fn single_zabbix_style_alert() {
        let a = parse(
            &json!({"id": 12345, "title": "Host unreachable", "status": "PROBLEM", "severity": "4", "host": "sw-01"}),
        );
        assert_eq!(a.len(), 1);
        assert_eq!((a[0].fingerprint.as_str(), a[0].severity, a[0].resolved), ("12345", "critical", false));
        let a = parse(&json!({"id": "12345", "title": "Host unreachable", "status": "0"}));
        assert!(a[0].resolved, "значение события 0 — восстановление");
        let a = parse(&json!({"title": "Printer offline", "host": "prn-2"}));
        assert_eq!(a[0].fingerprint, "Printer offline|prn-2", "без id — по названию и узлу");
    }

    #[test]
    fn severity_words() {
        assert_eq!(severity(Some("Disaster")), "critical");
        assert_eq!(severity(Some("Information")), "info");
        assert_eq!(severity(Some("average")), "warning");
        assert_eq!(severity(None), "warning");
    }
}

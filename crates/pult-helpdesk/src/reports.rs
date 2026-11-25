//! Отчёты: заявки за период по площадкам, категориям, исполнителям, каналам,
//! подразделениям; время реакции и решения, выполнение SLA, очередь и её возраст, нагрузка,
//! показатели качества (решено с первого обращения, повторные открытия, оценки), трудозатраты.
//! Выгрузка в CSV и XLSX; еженедельная рассылка.

use crate::{
    HelpdeskState,
    notify::{self, Msg},
    settings::HelpdeskSettings,
    tickets::OPEN_STATUSES,
};
use axum::{
    Json,
    extract::{FromRef, Query, State},
    http::header,
    response::{IntoResponse, Response},
};
use pult_core::l10n::tr;
use pult_core::{
    ApiError, ApiResult, Core, CurrentUser, Permission,
    rbac::{ROLE_ENGINEER, ROLE_SUPERADMIN},
    settings,
};
use serde::{Deserialize, Serialize};
use time::{Date, Duration, OffsetDateTime};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub const VIEW: Permission = Permission {
    code: "helpdesk.reports",
    description: "Смотреть и выгружать отчёты Helpdesk",
    default_roles: &[ROLE_ENGINEER, ROLE_SUPERADMIN],
};

pub fn routes<S>() -> OpenApiRouter<S>
where
    S: Clone + Send + Sync + 'static,
    Core: FromRef<S>,
    HelpdeskState: FromRef<S>,
{
    OpenApiRouter::new().routes(routes!(get_report)).routes(routes!(export_report))
}

#[derive(Deserialize, IntoParams)]
pub struct ReportQuery {
    /// Начало периода, `2026-11-01` (включительно).
    pub from: String,
    /// Конец периода, `2026-11-30` (включительно).
    pub to: String,
    pub site_id: Option<Uuid>,
    /// Для выгрузки: `xlsx` или `csv`.
    pub format: Option<String>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow, ToSchema)]
pub struct Bucket {
    pub name: String,
    pub created: i64,
    pub resolved: i64,
    /// Среднее время решения, часов.
    pub avg_resolve_hours: Option<f64>,
    /// Трудозатраты, минут.
    pub minutes: i64,
}

#[derive(Debug, Clone, Default, Serialize, sqlx::FromRow, ToSchema)]
pub struct ReportSummary {
    pub created: i64,
    pub resolved: i64,
    pub reopened: i64,
    /// Доля повторных открытий среди решённых, %.
    pub reopened_share: Option<f64>,
    /// Решено с первого обращения (без «Ждёт ответа» и повторных открытий), %.
    pub first_contact_share: Option<f64>,
    pub avg_take_minutes: Option<f64>,
    pub avg_first_response_minutes: Option<f64>,
    pub avg_resolve_hours: Option<f64>,
    pub sla_total: i64,
    pub sla_met: i64,
    /// Выполнение SLA, %.
    pub sla_share: Option<f64>,
    pub rating_up: i64,
    pub rating_down: i64,
    /// Доля «хорошо», %.
    pub rating_share: Option<f64>,
    /// Средние баллы подробного опроса (1–5) и число ответивших.
    pub survey_count: i64,
    pub avg_speed: Option<f64>,
    pub avg_quality: Option<f64>,
    pub avg_courtesy: Option<f64>,
    pub work_minutes: i64,
    pub open_now: i64,
    pub unassigned_now: i64,
    pub overdue_now: i64,
    /// Возраст очереди: открытые старше 1, 3, 7 дней.
    pub open_older_1d: i64,
    pub open_older_3d: i64,
    pub open_older_7d: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Report {
    pub from: String,
    pub to: String,
    pub summary: ReportSummary,
    pub by_site: Vec<Bucket>,
    pub by_category: Vec<Bucket>,
    pub by_assignee: Vec<Bucket>,
    pub by_channel: Vec<Bucket>,
    pub by_department: Vec<Bucket>,
    /// Кто чаще всего обращается — подсказка для базы знаний.
    pub top_requesters: Vec<Bucket>,
    /// Текущая нагрузка: открытые заявки по исполнителям.
    pub load: Vec<Bucket>,
}

fn parse_date(s: &str) -> ApiResult<Date> {
    Date::parse(s.trim(), time::macros::format_description!("[year]-[month]-[day]"))
        .map_err(|_| ApiError::bad_request("invalid_period", "Дата — в формате ГГГГ-ММ-ДД."))
}

/// Период в UTC: `[from 00:00, to + 1 день)` по поясу настроек.
async fn period(core: &Core, q: &ReportQuery) -> ApiResult<(OffsetDateTime, OffsetDateTime)> {
    let (from, to) = (parse_date(&q.from)?, parse_date(&q.to)?);
    if to < from || to - from > Duration::days(800) {
        return Err(ApiError::bad_request("invalid_period", "Конец периода раньше начала или период больше двух лет."));
    }
    let cfg: HelpdeskSettings = settings::load(&core.db).await?;
    let tz = crate::schedule::known_tz(&core.db, &cfg.timezone).await?;
    let a = crate::schedule::to_utc(&core.db, from.midnight(), &tz).await?;
    let b = crate::schedule::to_utc(&core.db, to.next_day().unwrap_or(to).midnight(), &tz).await?;
    Ok((a, b))
}

/// Разрез по выражению группировки (константы ниже).
async fn buckets(
    core: &Core,
    group: &str,
    a: OffsetDateTime,
    b: OffsetDateTime,
    site: Option<Uuid>,
) -> ApiResult<Vec<Bucket>> {
    let sql = format!(
        "SELECT coalesce({group}, '—') AS name,
                count(*) FILTER (WHERE t.created_at >= $1 AND t.created_at < $2) AS created,
                count(*) FILTER (WHERE t.resolved_at >= $1 AND t.resolved_at < $2) AS resolved,
                round((avg(extract(epoch FROM t.resolved_at - t.created_at) / 3600)
                       FILTER (WHERE t.resolved_at >= $1 AND t.resolved_at < $2))::numeric, 1)::float8 AS avg_resolve_hours,
                coalesce(sum((SELECT sum(w.minutes) FROM ticket_worklogs w WHERE w.ticket_id = t.id AND w.created_at >= $1 AND w.created_at < $2)), 0)::int8 AS minutes
           FROM tickets t
           LEFT JOIN sites s ON s.id = t.site_id
           LEFT JOIN ticket_categories c ON c.id = t.category_id
           LEFT JOIN users au ON au.id = t.assignee_id
           LEFT JOIN ad_accounts aa ON aa.id = au.ad_account_id
           LEFT JOIN persons ap ON ap.id = au.person_id
           LEFT JOIN persons rp ON rp.id = t.requester_id
           LEFT JOIN departments d ON d.id = rp.department_id
          WHERE ($3::uuid IS NULL OR t.site_id = $3)
            AND ((t.created_at >= $1 AND t.created_at < $2) OR (t.resolved_at >= $1 AND t.resolved_at < $2))
          GROUP BY 1 ORDER BY created DESC, name LIMIT 50" // SAFE: group — константа из этого модуля
    );
    Ok(sqlx::query_as(&sql).bind(a).bind(b).bind(site).fetch_all(&core.db).await?)
}

// Имя исполнителя: ФИО из справочника, иначе отображаемое имя учётки AD, иначе логин.
const ASSIGNEE: &str = "coalesce(nullif(trim(concat_ws(' ', ap.last_name, ap.first_name)), ''), nullif(aa.display_name, ''), aa.sam_account_name)";
const REQUESTER: &str = "nullif(trim(concat_ws(' ', rp.last_name, rp.first_name)), '')";

pub async fn build(core: &Core, q: &ReportQuery) -> ApiResult<Report> {
    let (a, b) = period(core, q).await?;
    let site = q.site_id;
    let mut summary: ReportSummary = sqlx::query_as(
        "WITH r AS (SELECT * FROM tickets t WHERE t.resolved_at >= $1 AND t.resolved_at < $2 AND ($3::uuid IS NULL OR t.site_id = $3)),
              c AS (SELECT * FROM tickets t WHERE t.created_at >= $1 AND t.created_at < $2 AND ($3::uuid IS NULL OR t.site_id = $3)),
              o AS (SELECT * FROM tickets t WHERE t.status = ANY($4) AND ($3::uuid IS NULL OR t.site_id = $3))
         SELECT (SELECT count(*) FROM c) AS created,
                (SELECT count(*) FROM r) AS resolved,
                (SELECT count(*) FROM r WHERE reopened_count > 0) AS reopened,
                NULL::float8 AS reopened_share,
                (SELECT round(100.0 * count(*) FILTER (WHERE reopened_count = 0 AND NOT EXISTS (
                            SELECT 1 FROM ticket_events e WHERE e.ticket_id = r.id AND e.kind = 'status' AND e.to_val = '\"waiting\"'))
                        / nullif(count(*), 0), 1)::float8 FROM r) AS first_contact_share,
                (SELECT round(avg(extract(epoch FROM taken_at - created_at) / 60)::numeric, 1)::float8 FROM c WHERE taken_at IS NOT NULL) AS avg_take_minutes,
                (SELECT round(avg(extract(epoch FROM first_response_at - created_at) / 60)::numeric, 1)::float8 FROM c WHERE first_response_at IS NOT NULL) AS avg_first_response_minutes,
                (SELECT round(avg(extract(epoch FROM resolved_at - created_at) / 3600)::numeric, 1)::float8 FROM r) AS avg_resolve_hours,
                (SELECT count(*) FROM r WHERE resolve_due_at IS NOT NULL) AS sla_total,
                (SELECT count(*) FROM r WHERE resolve_due_at IS NOT NULL AND resolved_at <= resolve_due_at
                        AND (response_due_at IS NULL OR coalesce(taken_at, resolved_at) <= response_due_at)) AS sla_met,
                NULL::float8 AS sla_share,
                (SELECT count(*) FROM ticket_ratings g JOIN tickets t ON t.id = g.ticket_id
                  WHERE g.value = 'up' AND g.updated_at >= $1 AND g.updated_at < $2 AND ($3::uuid IS NULL OR t.site_id = $3)) AS rating_up,
                (SELECT count(*) FROM ticket_ratings g JOIN tickets t ON t.id = g.ticket_id
                  WHERE g.value = 'down' AND g.updated_at >= $1 AND g.updated_at < $2 AND ($3::uuid IS NULL OR t.site_id = $3)) AS rating_down,
                NULL::float8 AS rating_share,
                (SELECT count(*) FROM ticket_ratings g JOIN tickets t ON t.id = g.ticket_id
                  WHERE num_nonnulls(g.speed, g.quality, g.courtesy) > 0
                    AND g.updated_at >= $1 AND g.updated_at < $2 AND ($3::uuid IS NULL OR t.site_id = $3)) AS survey_count,
                (SELECT round(avg(g.speed), 2)::float8 FROM ticket_ratings g JOIN tickets t ON t.id = g.ticket_id
                  WHERE g.updated_at >= $1 AND g.updated_at < $2 AND ($3::uuid IS NULL OR t.site_id = $3)) AS avg_speed,
                (SELECT round(avg(g.quality), 2)::float8 FROM ticket_ratings g JOIN tickets t ON t.id = g.ticket_id
                  WHERE g.updated_at >= $1 AND g.updated_at < $2 AND ($3::uuid IS NULL OR t.site_id = $3)) AS avg_quality,
                (SELECT round(avg(g.courtesy), 2)::float8 FROM ticket_ratings g JOIN tickets t ON t.id = g.ticket_id
                  WHERE g.updated_at >= $1 AND g.updated_at < $2 AND ($3::uuid IS NULL OR t.site_id = $3)) AS avg_courtesy,
                (SELECT coalesce(sum(w.minutes), 0) FROM ticket_worklogs w JOIN tickets t ON t.id = w.ticket_id
                  WHERE w.created_at >= $1 AND w.created_at < $2 AND ($3::uuid IS NULL OR t.site_id = $3))::int8 AS work_minutes,
                (SELECT count(*) FROM o) AS open_now,
                (SELECT count(*) FROM o WHERE assignee_id IS NULL) AS unassigned_now,
                (SELECT count(*) FROM o WHERE resolve_due_at <= now() AND sla_paused_at IS NULL) AS overdue_now,
                (SELECT count(*) FROM o WHERE created_at < now() - interval '1 day') AS open_older_1d,
                (SELECT count(*) FROM o WHERE created_at < now() - interval '3 days') AS open_older_3d,
                (SELECT count(*) FROM o WHERE created_at < now() - interval '7 days') AS open_older_7d",
    )
    .bind(a)
    .bind(b)
    .bind(site)
    .bind(OPEN_STATUSES)
    .fetch_one(&core.db)
    .await?;
    let share = |x: i64, total: i64| (total > 0).then(|| (1000.0 * x as f64 / total as f64).round() / 10.0);
    summary.reopened_share = share(summary.reopened, summary.resolved);
    summary.sla_share = share(summary.sla_met, summary.sla_total);
    summary.rating_share = share(summary.rating_up, summary.rating_up + summary.rating_down);
    let load: Vec<Bucket> = sqlx::query_as(&format!(
        "SELECT coalesce({ASSIGNEE}, 'Без исполнителя') AS name, count(*) AS created, 0::int8 AS resolved,
                NULL::float8 AS avg_resolve_hours, 0::int8 AS minutes
           FROM tickets t LEFT JOIN users au ON au.id = t.assignee_id LEFT JOIN ad_accounts aa ON aa.id = au.ad_account_id
           LEFT JOIN persons ap ON ap.id = au.person_id
          WHERE t.status = ANY($1) AND ($2::uuid IS NULL OR t.site_id = $2)
          GROUP BY 1 ORDER BY 2 DESC LIMIT 50" // SAFE: константа
    ))
    .bind(OPEN_STATUSES)
    .bind(site)
    .fetch_all(&core.db)
    .await?;
    Ok(Report {
        from: q.from.clone(),
        to: q.to.clone(),
        summary,
        by_site: buckets(core, "s.name", a, b, site).await?,
        by_category: buckets(core, "c.name", a, b, site).await?,
        by_assignee: buckets(core, ASSIGNEE, a, b, site).await?,
        by_channel: buckets(core, "t.channel", a, b, site).await?,
        by_department: buckets(core, "d.name", a, b, site).await?,
        top_requesters: buckets(core, REQUESTER, a, b, site).await?.into_iter().take(10).collect(),
        load,
    })
}

#[utoipa::path(get, path = "/api/helpdesk/reports", tag = "reports", params(ReportQuery), responses((status = 200, body = Report)))]
async fn get_report(
    State(core): State<Core>,
    user: CurrentUser,
    Query(q): Query<ReportQuery>,
) -> ApiResult<Json<Report>> {
    user.require(&VIEW)?;
    let mut r = build(&core, &q).await?;
    for b in r.by_assignee.iter_mut().chain(r.load.iter_mut()) {
        b.name = tr(&b.name).into_owned();
    }
    Ok(Json(r))
}

/// Ячейка таблицы выгрузки.
#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    Text(String),
    Num(f64),
}

fn t(s: impl Into<String>) -> Cell {
    Cell::Text(s.into())
}

fn n(v: impl Into<f64>) -> Cell {
    Cell::Num(v.into())
}

fn opt(v: Option<f64>) -> Cell {
    v.map(Cell::Num).unwrap_or_else(|| t(""))
}

/// Листы выгрузки: `(название, строки)`.
pub fn sheets(r: &Report) -> Vec<(String, Vec<Vec<Cell>>)> {
    let s = &r.summary;
    #[allow(clippy::cast_precision_loss)]
    let i = |v: i64| n(v as f64);
    let summary = vec![
        vec![t("Показатель"), t("Значение")],
        vec![t("Период"), t(format!("{} — {}", r.from, r.to))],
        vec![t("Создано заявок"), i(s.created)],
        vec![t("Решено заявок"), i(s.resolved)],
        vec![t("Повторно открыто"), i(s.reopened)],
        vec![t("Доля повторных открытий, %"), opt(s.reopened_share)],
        vec![t("Решено с первого обращения, %"), opt(s.first_contact_share)],
        vec![t("Среднее время до «В работе», мин"), opt(s.avg_take_minutes)],
        vec![t("Среднее время первого ответа, мин"), opt(s.avg_first_response_minutes)],
        vec![t("Среднее время решения, ч"), opt(s.avg_resolve_hours)],
        vec![t("Решено со сроком SLA"), i(s.sla_total)],
        vec![t("В срок"), i(s.sla_met)],
        vec![t("Выполнение SLA, %"), opt(s.sla_share)],
        vec![t("Оценок «хорошо»"), i(s.rating_up)],
        vec![t("Оценок «плохо»"), i(s.rating_down)],
        vec![t("Доля «хорошо», %"), opt(s.rating_share)],
        vec![t("Ответили на подробный опрос"), i(s.survey_count)],
        vec![t("Скорость, средний балл (1–5)"), opt(s.avg_speed)],
        vec![t("Качество решения, средний балл (1–5)"), opt(s.avg_quality)],
        vec![t("Вежливость, средний балл (1–5)"), opt(s.avg_courtesy)],
        vec![t("Трудозатраты, мин"), i(s.work_minutes)],
        vec![t("Открыто сейчас"), i(s.open_now)],
        vec![t("Без исполнителя сейчас"), i(s.unassigned_now)],
        vec![t("Просрочено сейчас"), i(s.overdue_now)],
        vec![t("Открыты дольше 1 дня"), i(s.open_older_1d)],
        vec![t("Открыты дольше 3 дней"), i(s.open_older_3d)],
        vec![t("Открыты дольше 7 дней"), i(s.open_older_7d)],
    ];
    let table = |head: &str, rows: &[Bucket]| {
        let mut out = vec![vec![t(head), t("Создано"), t("Решено"), t("Среднее решение, ч"), t("Трудозатраты, мин")]];
        out.extend(
            rows.iter().map(|b| vec![t(&b.name), i(b.created), i(b.resolved), opt(b.avg_resolve_hours), i(b.minutes)]),
        );
        out
    };
    let channels: Vec<Bucket> =
        r.by_channel.iter().map(|b| Bucket { name: channel_label(&b.name).to_string(), ..b.clone() }).collect();
    let load = {
        let mut out = vec![vec![t("Исполнитель"), t("Открытых заявок")]];
        out.extend(r.load.iter().map(|b| vec![t(&b.name), i(b.created)]));
        out
    };
    vec![
        ("Сводка".into(), summary),
        ("Площадки".into(), table("Площадка", &r.by_site)),
        ("Категории".into(), table("Категория", &r.by_category)),
        ("Исполнители".into(), table("Исполнитель", &r.by_assignee)),
        ("Каналы".into(), table("Канал", &channels)),
        ("Подразделения".into(), table("Подразделение", &r.by_department)),
        ("Частые заявители".into(), table("Заявитель", &r.top_requesters)),
        ("Нагрузка".into(), load),
    ]
}

/// CSV (разделитель `;`, UTF-8 с BOM — Excel открывает кириллицу правильно).
pub fn to_csv(sheets: &[(String, Vec<Vec<Cell>>)]) -> String {
    let esc = |s: &str| {
        // Формулы в ячейках не исполняем: «=», «+», «-», «@», таб и CR в начале — экранируем (CSV injection).
        let s = if s.starts_with(['=', '+', '-', '@', '\t', '\r']) { format!("'{s}") } else { s.to_string() };
        if s.contains([';', '"', '\n', '\r']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s }
    };
    let mut out = String::from("\u{feff}");
    for (name, rows) in sheets {
        out.push_str(&esc(name));
        out.push_str("\r\n");
        for row in rows {
            let line: Vec<String> = row
                .iter()
                .map(|c| match c {
                    Cell::Text(s) => esc(s),
                    Cell::Num(v) => v.to_string().replace('.', ","),
                })
                .collect();
            out.push_str(&line.join(";"));
            out.push_str("\r\n");
        }
        out.push_str("\r\n");
    }
    out
}

// ------------------------------------------------------------------ XLSX без зависимостей

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

/// ZIP без сжатия (метод STORE): XLSX этого достаточно.
fn zip(files: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in files {
        let offset = out.len() as u32;
        let crc = crc32(data);
        let size = data.len() as u32;
        let header = |sig: u32, central: bool| {
            let mut h = Vec::new();
            h.extend_from_slice(&sig.to_le_bytes());
            if central {
                h.extend_from_slice(&20u16.to_le_bytes());
            }
            h.extend_from_slice(&20u16.to_le_bytes()); // версия
            h.extend_from_slice(&0x0800u16.to_le_bytes()); // имена в UTF-8
            h.extend_from_slice(&0u16.to_le_bytes()); // STORE
            h.extend_from_slice(&0u16.to_le_bytes()); // время
            h.extend_from_slice(&0x5b21u16.to_le_bytes()); // дата 2025-09-01
            h.extend_from_slice(&crc.to_le_bytes());
            h.extend_from_slice(&size.to_le_bytes());
            h.extend_from_slice(&size.to_le_bytes());
            h.extend_from_slice(&(name.len() as u16).to_le_bytes());
            h.extend_from_slice(&0u16.to_le_bytes());
            if central {
                h.extend_from_slice(&[0u8; 8]); // комментарий, диск, атрибуты
                h.extend_from_slice(&0u32.to_le_bytes());
                h.extend_from_slice(&offset.to_le_bytes());
            }
            h.extend_from_slice(name.as_bytes());
            h
        };
        out.extend(header(0x0403_4b50, false));
        out.extend_from_slice(data);
        central.extend(header(0x0201_4b50, true));
    }
    let start = out.len() as u32;
    let count = files.len() as u16;
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&[0u8; 4]);
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&start.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

fn xml_esc(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect::<String>()
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn col(mut i: usize) -> String {
    let mut s = String::new();
    i += 1;
    while i > 0 {
        let r = (i - 1) % 26;
        s.insert(0, char::from(b'A' + r as u8));
        i = (i - 1) / 26;
    }
    s
}

pub fn to_xlsx(sheets: &[(String, Vec<Vec<Cell>>)]) -> Vec<u8> {
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    let overrides: String = (1..=sheets.len())
        .map(|i| format!("<Override PartName=\"/xl/worksheets/sheet{i}.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/>"))
        .collect();
    files.push((
        "[Content_Types].xml".into(),
        format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/><Override PartName=\"/xl/workbook.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\"/>{overrides}</Types>").into_bytes(),
    ));
    files.push((
        "_rels/.rels".into(),
        b"<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"xl/workbook.xml\"/></Relationships>".to_vec(),
    ));
    let mut names = std::collections::HashSet::new();
    let sheet_tags: String = sheets
        .iter()
        .enumerate()
        .map(|(i, (name, _))| {
            // Имя листа: до 31 символа, без []:*?/\ и уникальное.
            let mut clean: String = name.chars().filter(|c| !"[]:*?/\\".contains(*c)).take(28).collect();
            if !names.insert(clean.clone()) {
                clean = format!("{clean}{}", i + 1);
            }
            format!("<sheet name=\"{}\" sheetId=\"{}\" r:id=\"rId{}\"/>", xml_esc(&clean), i + 1, i + 1)
        })
        .collect();
    files.push((
        "xl/workbook.xml".into(),
        format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><sheets>{sheet_tags}</sheets></workbook>").into_bytes(),
    ));
    let rels: String = (1..=sheets.len())
        .map(|i| format!("<Relationship Id=\"rId{i}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet{i}.xml\"/>"))
        .collect();
    files.push((
        "xl/_rels/workbook.xml.rels".into(),
        format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">{rels}</Relationships>").into_bytes(),
    ));
    for (i, (_, rows)) in sheets.iter().enumerate() {
        let mut x = String::from(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><cols><col min=\"1\" max=\"1\" width=\"42\" customWidth=\"1\"/><col min=\"2\" max=\"8\" width=\"16\" customWidth=\"1\"/></cols><sheetData>",
        );
        for (r, row) in rows.iter().enumerate() {
            x.push_str(&format!("<row r=\"{}\">", r + 1));
            for (c, cell) in row.iter().enumerate() {
                let at = format!("{}{}", col(c), r + 1);
                match cell {
                    Cell::Num(v) if v.is_finite() => x.push_str(&format!("<c r=\"{at}\"><v>{v}</v></c>")),
                    Cell::Num(_) => {}
                    Cell::Text(s) => x.push_str(&format!(
                        "<c r=\"{at}\" t=\"inlineStr\"><is><t xml:space=\"preserve\">{}</t></is></c>",
                        xml_esc(s)
                    )),
                }
            }
            x.push_str("</row>");
        }
        x.push_str("</sheetData></worksheet>");
        files.push((format!("xl/worksheets/sheet{}.xml", i + 1), x.into_bytes()));
    }
    zip(&files)
}

/// Выгрузка отчёта.
#[utoipa::path(get, path = "/api/helpdesk/reports/export", tag = "reports", params(ReportQuery),
    responses((status = 200, description = "Файл XLSX или CSV")))]
async fn export_report(
    State(core): State<Core>,
    user: CurrentUser,
    Query(q): Query<ReportQuery>,
) -> Result<Response, ApiError> {
    user.require(&VIEW)?;
    let r = build(&core, &q).await?;
    let s = localized(sheets(&r));
    let name = format!("pult-report-{}-{}", r.from, r.to);
    let (ctype, ext, body) = match q.format.as_deref() {
        Some("csv") => ("text/csv; charset=utf-8", "csv", to_csv(&s).into_bytes()),
        _ => ("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet", "xlsx", to_xlsx(&s)),
    };
    let disposition = format!(
        "attachment; filename=\"{}.{ext}\"",
        name.replace(|c: char| !c.is_ascii_alphanumeric() && c != '-', "")
    );
    Ok((
        [
            (header::CONTENT_TYPE, ctype.to_string()),
            (header::CONTENT_DISPOSITION, disposition),
            (header::CACHE_CONTROL, "no-store".into()),
        ],
        body,
    )
        .into_response())
}

/// Листы выгрузки на языке запроса: названия листов, подписи и «Без исполнителя» из SQL.
/// Данные (имена, категории) остаются как есть — каталог их не знает.
fn localized(sheets: Vec<(String, Vec<Vec<Cell>>)>) -> Vec<(String, Vec<Vec<Cell>>)> {
    sheets
        .into_iter()
        .map(|(name, rows)| {
            let rows = rows
                .into_iter()
                .map(|row| {
                    row.into_iter()
                        .map(|c| match c {
                            Cell::Text(s) => Cell::Text(tr(&s).into_owned()),
                            n => n,
                        })
                        .collect()
                })
                .collect();
            (tr(&name).into_owned(), rows)
        })
        .collect()
}

/// Еженедельная рассылка: сводка за прошлую неделю письмом — на языке установки
/// (получатели — адреса из настроек, не пользователи портала).
pub async fn weekly(core: &Core) -> ApiResult<usize> {
    let lang = pult_core::l10n::default_lang(&core.db).await;
    pult_core::l10n::with_lang(lang, weekly_mail(core)).await
}

async fn weekly_mail(core: &Core) -> ApiResult<usize> {
    let cfg: HelpdeskSettings = settings::load(&core.db).await?;
    if cfg.weekly_report_to.is_empty() {
        return Ok(0);
    }
    let today = OffsetDateTime::now_utc().date();
    let q = ReportQuery {
        from: (today - Duration::days(7)).to_string(),
        to: (today - Duration::days(1)).to_string(),
        site_id: None,
        format: None,
    };
    let r = build(core, &q).await?;
    let mut text = String::new();
    for row in &localized(sheets(&r))[0].1[1..] {
        let v: Vec<String> = row
            .iter()
            .map(|c| match c {
                Cell::Text(s) => s.clone(),
                Cell::Num(n) => n.to_string(),
            })
            .collect();
        text.push_str(&format!("{}: {}\n", v[0], v.get(1).cloned().unwrap_or_default()));
    }
    let c = notify::ctx(core).await?;
    if !c.portal.is_empty() {
        text.push_str(&tr(&format!(
            "\nПодробно и выгрузка в XLSX: {}/reports?from={}&to={}\n",
            c.portal, q.from, q.to
        )));
    }
    let subject = tr(&format!("Отчёт Helpdesk за {} — {}", q.from, q.to)).into_owned();
    let mut tx = core.db.begin().await?;
    for to in &cfg.weekly_report_to {
        notify::to_address(&mut tx, to, &subject, &Msg::Report { body: &text }, &c).await?;
    }
    tx.commit().await?;
    Ok(cfg.weekly_report_to.len())
}

/// Подпись канала для выгрузки (в интерфейсе — из переводов).
fn channel_label(code: &str) -> &str {
    match code {
        "portal" => "Портал",
        "email" => "Почта",
        "phone" => "Телефон",
        "walk_in" => "Лично",
        "login_help" => "«Не могу войти»",
        "api" => "API",
        "monitoring" => "Мониторинг",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_matches_reference() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
    }

    #[test]
    fn xlsx_is_a_zip_with_sheets() {
        let x = to_xlsx(&[("Сводка".into(), vec![vec![t("Показатель <1>"), n(2.5)]])]);
        assert_eq!(&x[..4], b"PK\x03\x04");
        let s = String::from_utf8_lossy(&x);
        assert!(
            s.contains("xl/worksheets/sheet1.xml") && s.contains("Показатель &lt;1&gt;") && s.contains("<v>2.5</v>")
        );
    }

    #[test]
    fn csv_escapes_formulas_and_separators() {
        let c = to_csv(&[("Лист".into(), vec![vec![t("=HYPERLINK(1)"), t("a;b"), n(1.5)]])]);
        assert!(c.contains("'=HYPERLINK(1);\"a;b\";1,5"));
        let c = to_csv(&[("Лист".into(), vec![vec![t("\t=1"), t("\r=2")]])]);
        assert!(c.contains("'\t=1;\"'\r=2\""), "таб и CR в начале тоже экранируются");
    }

    #[test]
    fn column_names() {
        assert_eq!(col(0), "A");
        assert_eq!(col(25), "Z");
        assert_eq!(col(26), "AA");
    }
}

//! HTTP-интерфейс заявок. Логика — в `tickets.rs`, права проверяются и там.

use crate::sla::SLA_STATE;
use crate::{
    HelpdeskState, admin, card,
    card::WatcherView,
    checklist::{self, ChecklistItem, ItemUpdate, NewItem},
    files, perm, public,
    settings::{CloseCode, HelpdeskSettings},
    tickets::{self, CommentResult, Created, MajorInput, NewComment, NewTicket, Resolution, TicketUpdate, Who},
};
use axum::{
    Json,
    extract::{DefaultBodyLimit, FromRef, Multipart, Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use pult_core::{
    ApiError, ApiResult, Core, CurrentUser, Module, Permission,
    audit::{self, Entry, Source},
    settings,
};
use pult_people::computers::ComputerRow;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::OffsetDateTime;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub struct HelpdeskModule;

/// Запас над настройкой размера вложения; точный лимит проверяет `files::store`.
const UPLOAD_BODY_LIMIT: usize = 105 * 1024 * 1024;
/// Одновременных загрузок файлов на сервер (каждая держит файл в памяти до записи на диск).
const UPLOADS_AT_ONCE: usize = 8;
static UPLOADS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(UPLOADS_AT_ONCE);
/// Одновременных загрузок одного пользователя: медленные загрузки одного не занимают места всех.
const UPLOADS_PER_USER: usize = 2;
static UPLOADING: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<Uuid, usize>>> =
    std::sync::LazyLock::new(Default::default);

/// Место загрузки пользователя; освобождается при завершении запроса (в том числе по ошибке).
struct UserUpload(Uuid);

impl UserUpload {
    fn start(user: Uuid) -> Option<Self> {
        let mut busy = UPLOADING.lock().unwrap_or_else(|e| e.into_inner());
        let n = busy.entry(user).or_default();
        if *n >= UPLOADS_PER_USER {
            return None;
        }
        *n += 1;
        Some(Self(user))
    }
}

impl Drop for UserUpload {
    fn drop(&mut self) {
        let mut busy = UPLOADING.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = busy.get_mut(&self.0) {
            *n -= 1;
            if *n == 0 {
                busy.remove(&self.0);
            }
        }
    }
}
/// Квота неприкреплённых файлов пользователя за сутки.
const UNATTACHED_MAX_FILES: i64 = 50;
const UNATTACHED_MAX_MB: i64 = 500;

impl<S> Module<S> for HelpdeskModule
where
    S: Clone + Send + Sync + 'static,
    Core: FromRef<S>,
    HelpdeskState: FromRef<S>,
{
    fn name(&self) -> &'static str {
        "helpdesk"
    }

    fn routes(&self) -> OpenApiRouter<S> {
        OpenApiRouter::new()
            .routes(routes!(list_tickets, create_ticket))
            .routes(routes!(get_ticket, update_ticket))
            .routes(routes!(add_comment))
            .routes(routes!(take_ticket))
            .routes(routes!(set_waiting))
            .routes(routes!(resolve_ticket))
            .routes(routes!(confirm_ticket))
            .routes(routes!(not_resolved))
            .routes(routes!(cancel_ticket))
            .routes(routes!(reopen_ticket))
            .routes(routes!(transfer_ticket))
            .routes(routes!(verify_identity))
            .routes(routes!(make_major))
            .routes(routes!(link_ticket))
            .routes(routes!(assign_ticket))
            .routes(routes!(hold_ticket))
            .routes(routes!(add_watcher))
            .routes(routes!(remove_watcher))
            .routes(routes!(add_checklist))
            .routes(routes!(update_checklist_item, delete_checklist_item))
            .routes(routes!(list_close_codes))
            .routes(routes!(context))
            .routes(routes!(list_computers))
            .routes(routes!(list_engineers))
            .routes(routes!(list_announcements))
            .routes(routes!(end_announcement))
            .routes(routes!(download_file))
            .merge(OpenApiRouter::new().routes(routes!(upload_file)).layer(DefaultBodyLimit::max(UPLOAD_BODY_LIMIT)))
            .merge(admin::routes())
            .merge(public::routes())
            .merge(crate::sla::routes())
            .merge(crate::rules::routes())
            .merge(crate::templates::routes())
            .merge(crate::objects::routes())
            .merge(crate::import::routes())
            .merge(crate::kb::routes())
            .merge(crate::service::routes())
            .merge(crate::reports::routes())
            .merge(crate::access::routes())
            .merge(crate::vault::routes())
            .merge(crate::assist::routes())
            .merge(crate::cmdb::routes())
            .merge(crate::inventory::routes())
            .merge(crate::extensions::routes())
            .merge(crate::integrations::routes())
            .merge(crate::monitoring::routes())
            .merge(crate::itil::routes())
    }

    fn permissions(&self) -> Vec<Permission> {
        crate::permissions()
    }

    fn jobs(&self) -> Vec<pult_core::JobDefinition> {
        crate::jobs::definitions()
    }
}

const NAMES: &str = "coalesce(nullif(trim(concat_ws(' ', p.last_name, p.first_name, p.middle_name)), ''), t.unverified_contact->>'name', ra.display_name, ra.sam_account_name) AS requester_name,
    s.name AS site_name, c.name AS category_name,
    coalesce(nullif(trim(concat_ws(' ', ap.last_name, ap.first_name)), ''), aa.display_name, aa.sam_account_name) AS assignee_name";

const JOINS: &str = "FROM tickets t
    LEFT JOIN persons p ON p.id = t.requester_id
    LEFT JOIN users ru ON ru.id = t.requester_user_id
    LEFT JOIN ad_accounts ra ON ra.id = ru.ad_account_id
    LEFT JOIN sites s ON s.id = t.site_id
    LEFT JOIN ticket_categories c ON c.id = t.category_id
    LEFT JOIN users au ON au.id = t.assignee_id
    LEFT JOIN ad_accounts aa ON aa.id = au.ad_account_id
    LEFT JOIN persons ap ON ap.id = au.person_id";

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct TicketRow {
    pub id: Uuid,
    pub number: i64,
    pub title: String,
    /// `incident`, `request`, `problem`, `change`.
    pub kind: String,
    pub status: String,
    pub priority: String,
    pub line: String,
    pub channel: String,
    pub is_major: bool,
    pub linked: bool,
    pub unverified: bool,
    pub requester_name: Option<String>,
    pub site_name: Option<String>,
    pub category_name: Option<String>,
    pub assignee_id: Option<Uuid>,
    pub assignee_name: Option<String>,
    pub group_name: Option<String>,
    /// `ok`, `warning`, `breached`, `paused`, `met`; без SLA — пусто.
    pub sla_state: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub resolve_due_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub updated_at: OffsetDateTime,
    pub unread: bool,
}

#[derive(Deserialize, IntoParams)]
pub struct ListQuery {
    /// `mine` — мои обращения; для инженера ещё `assigned`, `unassigned`, `group` (мои группы), `watching`, `all`.
    pub scope: Option<String>,
    /// `open` (по умолчанию), `active` (открытые и решённые), `all` или код статуса.
    pub status: Option<String>,
    pub site_id: Option<Uuid>,
    pub category_id: Option<Uuid>,
    pub group_id: Option<Uuid>,
    pub assignee_id: Option<Uuid>,
    /// `warning` — скоро срок, `breached` — просрочены.
    pub sla: Option<String>,
    /// Создана не раньше (`2026-11-01`) и не позже (включительно).
    pub created_from: Option<String>,
    pub created_to: Option<String>,
    pub q: Option<String>,
    /// `problem` или `change` — только они; без параметра — обращения (инциденты и запросы).
    pub kind: Option<String>,
    /// `updated`, `created`, `number`, `priority`, `status`, `due`.
    pub sort: Option<String>,
    #[serde(default)]
    pub asc: bool,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

fn sort_column(s: Option<&str>) -> &'static str {
    match s {
        Some("created") => "t.created_at",
        Some("number") => "t.number",
        Some("status") => "t.status",
        Some("priority") => "array_position(ARRAY['critical','high','normal','low'], t.priority)",
        Some("due") => "t.resolve_due_at",
        _ => "t.updated_at",
    }
}

fn date_param(s: Option<&str>) -> ApiResult<Option<time::Date>> {
    s.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            time::Date::parse(s, time::macros::format_description!("[year]-[month]-[day]"))
                .map_err(|_| ApiError::bad_request("invalid_query", "Дата — в формате ГГГГ-ММ-ДД."))
        })
        .transpose()
}

/// Список. Видимость — в самом запросе: сотрудник не получит чужого ни одним фильтром.
#[utoipa::path(get, path = "/api/helpdesk/tickets", tag = "helpdesk", params(ListQuery),
    responses((status = 200, body = Vec<TicketRow>)))]
async fn list_tickets(
    State(core): State<Core>,
    user: CurrentUser,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Vec<TicketRow>>> {
    let who = Who::from_user(&user);
    let scope = if who.engineer { q.scope.unwrap_or_else(|| "all".into()) } else { "mine".into() };
    let status = q.status.unwrap_or_else(|| "open".into());
    let statuses: Option<Vec<String>> = match status.as_str() {
        "all" => None,
        "open" => Some(tickets::OPEN_STATUSES.iter().map(|s| (*s).to_string()).collect()),
        // Заявителю «открытые» — и решённые: их он подтверждает или открывает заново.
        "active" => Some(tickets::OPEN_STATUSES.iter().chain(&["resolved"]).map(|s| (*s).to_string()).collect()),
        s => Some(vec![s.to_string()]),
    };
    let needle =
        q.q.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(|s| s.chars().take(200).collect::<String>());
    let number: Option<i64> = needle.as_deref().and_then(|s| s.trim_start_matches('#').parse().ok());
    let dir = if q.asc { "ASC" } else { "DESC" };
    let col = sort_column(q.sort.as_deref());
    let sql = format!(
        "SELECT t.id, t.number, t.title, t.kind, t.status, t.priority, t.line, t.channel, t.is_major,
                t.parent_ticket_id IS NOT NULL AS linked,
                (t.requester_id IS NULL AND t.requester_user_id IS NULL) AS unverified,
                {NAMES}, t.assignee_id, t.created_at, t.updated_at,
                (SELECT g.name FROM engineer_groups g WHERE g.id = t.group_id) AS group_name,
                {SLA_STATE} AS sla_state, t.resolve_due_at,
                (r.read_at IS NULL OR r.read_at < t.updated_at) AS unread
           {JOINS}
           LEFT JOIN ticket_reads r ON r.ticket_id = t.id AND r.user_id = $1
          WHERE ($2 OR t.requester_user_id = $1 OR t.requester_id = $3
                 OR EXISTS (SELECT 1 FROM ticket_watchers w WHERE w.ticket_id = t.id AND w.person_id = $3))
            AND CASE $4
                  WHEN 'mine' THEN t.requester_user_id = $1 OR t.requester_id = $3
                       OR EXISTS (SELECT 1 FROM ticket_watchers w WHERE w.ticket_id = t.id AND w.person_id = $3)
                  WHEN 'watching' THEN EXISTS (SELECT 1 FROM ticket_watchers w WHERE w.ticket_id = t.id AND w.person_id = $3)
                  WHEN 'assigned' THEN t.assignee_id = $1
                       OR EXISTS (SELECT 1 FROM ticket_checklist_items i WHERE i.ticket_id = t.id AND i.assignee_id = $1 AND i.done_at IS NULL)
                  WHEN 'unassigned' THEN t.assignee_id IS NULL
                  WHEN 'group' THEN t.group_id IN (SELECT m.group_id FROM engineer_group_members m WHERE m.user_id = $1)
                  ELSE true END
            AND ($12::uuid IS NULL OR t.group_id = $12)
            AND ($13::uuid IS NULL OR t.assignee_id = $13)
            AND ($14::text IS NULL OR ({SLA_STATE}) = $14)
            AND ($15::date IS NULL OR t.created_at >= $15::date)
            AND ($16::date IS NULL OR t.created_at < $16::date + 1)
            AND (($17::text IS NULL AND t.kind IN ('incident', 'request')) OR t.kind = $17)
            AND ($5::text[] IS NULL OR t.status = ANY($5))
            AND ($6::uuid IS NULL OR t.site_id = $6)
            AND ($7::uuid IS NULL OR t.category_id = $7)
            AND ($8::text IS NULL OR t.number = $9
                 OR t.search_tsv @@ websearch_to_tsquery('russian', $8)
                 OR pult_norm(concat_ws(' ', p.last_name, p.first_name)) LIKE '%' || pult_norm($8) || '%')
          ORDER BY {col} {dir}, t.number DESC
          LIMIT $10 OFFSET $11" // SAFE: col и dir — из белого списка выше, SLA_STATE — константа
    );
    let rows = sqlx::query_as(&sql)
        .bind(user.user_id)
        .bind(who.engineer)
        .bind(user.person_id)
        .bind(scope)
        .bind(statuses)
        .bind(q.site_id)
        .bind(q.category_id)
        .bind(&needle)
        .bind(number)
        .bind(q.limit.unwrap_or(100).clamp(1, 500))
        .bind(q.offset.unwrap_or(0).max(0))
        .bind(q.group_id)
        .bind(q.assignee_id)
        .bind(q.sla.as_deref().filter(|s| matches!(*s, "ok" | "warning" | "breached" | "paused" | "met")))
        .bind(date_param(q.created_from.as_deref())?)
        .bind(date_param(q.created_to.as_deref())?)
        // Проблемы и изменения — только ИТ; сотрудник видит лишь свои обращения.
        .bind(q.kind.as_deref().filter(|k| who.engineer && matches!(*k, "problem" | "change")))
        .fetch_all(&core.db)
        .await?;
    Ok(Json(rows))
}

#[derive(Deserialize, ToSchema)]
pub struct CreateTicket {
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub category_id: Uuid,
    pub site_id: Uuid,
    pub contact_phone: Option<String>,
    pub contact_room: Option<String>,
    pub computer_name: Option<String>,
    #[serde(default)]
    #[schema(value_type = Object)]
    pub form_answers: Value,
    #[serde(default)]
    pub files: Vec<Uuid>,
    /// «У меня то же самое»: номер главной заявки.
    pub same_as: Option<i64>,
    /// Заявка за сотрудника: кого регистрируем.
    pub on_behalf_of: Option<Uuid>,
    /// `phone` или `walk_in` для заявки за сотрудника.
    pub channel: Option<String>,
    /// «Запрос доступа»: ресурс из каталога.
    pub resource_id: Option<Uuid>,
    /// Шаблон заявки: тема, описание, категория, группа и чек-лист из него.
    pub template_id: Option<Uuid>,
    /// Оборудование из реестра (до 10). Сотрудник — только то, за которое отвечает заявитель.
    #[serde(default)]
    pub asset_ids: Vec<Uuid>,
}

/// Создать заявку; заявитель — сам пользователь или, для инженера, выбранный сотрудник.
#[utoipa::path(post, path = "/api/helpdesk/tickets", tag = "helpdesk", request_body = CreateTicket,
    responses((status = 201, body = Created), (status = 400, body = pult_core::error::ErrorBody)))]
async fn create_ticket(
    State(core): State<Core>,
    user: CurrentUser,
    headers: HeaderMap,
    uri: axum::http::Uri,
    Json(b): Json<CreateTicket>,
) -> ApiResult<(StatusCode, Json<Created>)> {
    // Адрес из запроса (заголовок Host подделывается) — только от администратора портала.
    if user.grants.has(&perm::ADMIN) {
        remember_portal_url(&core, &headers, &uri).await;
    }
    let who = Who::from_user(&user);
    let (requester_id, requester_user_id, channel) = match b.on_behalf_of {
        Some(person) => {
            user.require(&perm::ON_BEHALF)?;
            let channel = b.channel.clone().unwrap_or_else(|| "phone".into());
            if !matches!(channel.as_str(), "phone" | "walk_in") {
                return Err(ApiError::bad_request("invalid_ticket", "Канал — телефон или лично."));
            }
            let row: Option<Option<Uuid>> = sqlx::query_scalar(
                "SELECT (SELECT id FROM users u WHERE u.person_id = p.id LIMIT 1) FROM persons p WHERE p.id = $1 AND NOT p.is_archived",
            )
            .bind(person)
            .fetch_optional(&core.db)
            .await?;
            let linked = row.ok_or_else(|| ApiError::bad_request("invalid_ticket", "Сотрудник не найден."))?;
            (Some(person), linked, channel)
        }
        None => (user.person_id, Some(user.user_id), "portal".to_string()),
    };
    let parent_ticket_id = match b.same_as {
        Some(n) => Some(
            sqlx::query_scalar("SELECT id FROM tickets WHERE number = $1 AND is_major")
                .bind(n)
                .fetch_optional(&core.db)
                .await?
                .ok_or_else(|| ApiError::bad_request("invalid_ticket", "Массовый сбой не найден."))?,
        ),
        None => None,
    };
    let created = tickets::create(
        &core,
        &who,
        NewTicket {
            title: b.title,
            description: b.description,
            category_id: Some(b.category_id),
            site_id: Some(b.site_id),
            requester_id,
            requester_user_id,
            channel,
            contact_phone: b.contact_phone,
            contact_room: b.contact_room,
            computer_name: b.computer_name,
            form_answers: b.form_answers,
            parent_ticket_id,
            files: b.files,
            files_owner: Some(user.user_id),
            template_id: b.template_id,
            resource_id: b.resource_id,
            asset_ids: b.asset_ids,
            ..Default::default()
        },
    )
    .await?;
    Ok((StatusCode::CREATED, Json(created)))
}

/// Адрес портала для ссылок в письмах запоминается при первом обращении администратора из браузера
/// (обычно его задаёт мастер первого запуска или «Настройки заявок»).
async fn remember_portal_url(core: &Core, headers: &HeaderMap, uri: &axum::http::Uri) {
    let Ok(mut cfg) = settings::load::<HelpdeskSettings>(&core.db).await else { return };
    if !cfg.portal_url.is_empty() {
        return;
    }
    if let Some(host) = pult_core::session::request_host(headers, uri) {
        cfg.portal_url = format!("https://{host}");
        if let Err(e) = settings::save(&core.db, &cfg, None).await {
            tracing::warn!(error = %e, "адрес портала не сохранён");
        }
    }
}

#[derive(Serialize, ToSchema, Clone)]
pub struct AttachmentView {
    pub id: Uuid,
    pub name: String,
    pub mime: String,
    pub size: i64,
    pub is_image: bool,
}

#[derive(sqlx::FromRow)]
struct AttachmentRow {
    id: Uuid,
    comment_id: Option<Uuid>,
    original_name: String,
    mime: String,
    size: i64,
}

#[derive(Serialize, ToSchema)]
pub struct FeedItem {
    pub id: Uuid,
    /// `comment` или `event`.
    #[serde(rename = "type")]
    pub item_type: String,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub at: OffsetDateTime,
    pub author: Option<String>,
    pub internal: bool,
    pub body: Option<String>,
    pub source: Option<String>,
    pub kind: Option<String>,
    #[schema(value_type = Object)]
    pub from: Option<Value>,
    #[schema(value_type = Object)]
    pub to: Option<Value>,
    pub attachments: Vec<AttachmentView>,
    /// Затраченное время (видят инженеры).
    pub minutes: Option<i32>,
}

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct TicketHead {
    pub id: Uuid,
    pub number: i64,
    pub title: String,
    pub description: String,
    pub status: String,
    pub priority: String,
    pub kind: String,
    pub line: String,
    pub channel: String,
    pub category_id: Option<Uuid>,
    pub site_id: Option<Uuid>,
    #[schema(value_type = Object)]
    pub form_answers: Value,
    pub contact_phone: Option<String>,
    pub contact_room: Option<String>,
    pub computer_name: Option<String>,
    pub is_major: bool,
    pub reopened_count: i32,
    pub version: i32,
    pub requester_name: Option<String>,
    pub requester_email: Option<String>,
    pub requester_person_id: Option<Uuid>,
    pub unverified: bool,
    #[schema(value_type = Object)]
    pub unverified_contact: Option<Value>,
    pub site_name: Option<String>,
    pub category_name: Option<String>,
    pub assignee_id: Option<Uuid>,
    pub assignee_name: Option<String>,
    pub created_by_name: Option<String>,
    pub parent_number: Option<i64>,
    pub follow_up_number: Option<i64>,
    /// Инцидент — следствие этой проблемы.
    pub problem_number: Option<i64>,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub taken_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub first_response_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub resolved_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub auto_close_at: Option<OffsetDateTime>,
    pub closure_code: Option<String>,
    /// Сумма трудозатрат, минут.
    pub work_minutes: i64,
    pub impact: String,
    pub urgency: String,
    pub group_id: Option<Uuid>,
    pub group_name: Option<String>,
    pub sla_state: Option<String>,
    pub sla_policy_name: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub response_due_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub resolve_due_at: Option<OffsetDateTime>,
    pub sla_paused: bool,
    pub requester_vip: bool,
    pub rating: Option<String>,
    pub rating_comment: Option<String>,
    /// Баллы подробного опроса 1–5: скорость, качество, вежливость.
    pub rating_speed: Option<i16>,
    pub rating_quality: Option<i16>,
    pub rating_courtesy: Option<i16>,
    #[serde(skip)]
    pub requester_user_id: Option<Uuid>,
}

#[derive(Serialize, ToSchema)]
pub struct ChildTicket {
    pub number: i64,
    pub title: String,
    pub status: String,
}

#[derive(Serialize, ToSchema)]
pub struct TicketDetail {
    pub ticket: TicketHead,
    pub feed: Vec<FeedItem>,
    pub attachments: Vec<AttachmentView>,
    pub children: Vec<ChildTicket>,
    pub watchers: Vec<WatcherView>,
    pub checklist: Vec<ChecklistItem>,
    pub objects: Vec<crate::objects::LinkedObject>,
    pub links: Vec<crate::objects::TicketLink>,
    pub is_requester: bool,
    pub is_watcher: bool,
    pub can_work: bool,
}

fn view(a: &AttachmentRow) -> AttachmentView {
    AttachmentView {
        id: a.id,
        name: a.original_name.clone(),
        mime: a.mime.clone(),
        size: a.size,
        is_image: files::is_inline_image(&a.mime),
    }
}

/// Карточка. Сотруднику — только его заявка и только публичная часть ленты.
#[utoipa::path(get, path = "/api/helpdesk/tickets/{number}", tag = "helpdesk", params(("number" = i64, Path)),
    responses((status = 200, body = TicketDetail), (status = 404, body = pult_core::error::ErrorBody)))]
async fn get_ticket(
    State(core): State<Core>,
    user: CurrentUser,
    Path(number): Path<i64>,
) -> ApiResult<Json<TicketDetail>> {
    let who = Who::from_user(&user);
    let sql = format!(
        "SELECT t.id, t.number, t.title, t.description, t.status, t.priority, t.kind, t.line, t.channel, t.category_id,
                t.site_id, t.form_answers, t.contact_phone, t.contact_room, t.computer_name, t.is_major,
                t.reopened_count, t.version, {NAMES}, t.assignee_id,
                coalesce(nullif(p.email, ''), ra.mail) AS requester_email, t.requester_id AS requester_person_id,
                t.requester_user_id,
                (t.requester_id IS NULL AND t.requester_user_id IS NULL) AS unverified,
                t.unverified_contact,
                (SELECT coalesce(ca.display_name, ca.sam_account_name) FROM users cu JOIN ad_accounts ca ON ca.id = cu.ad_account_id WHERE cu.id = t.created_by) AS created_by_name,
                (SELECT number FROM tickets x WHERE x.id = t.parent_ticket_id) AS parent_number,
                (SELECT number FROM tickets x WHERE x.id = t.follow_up_of) AS follow_up_number,
                (SELECT number FROM tickets x WHERE x.id = t.problem_id) AS problem_number,
                t.created_at, t.taken_at, t.first_response_at, t.resolved_at, t.auto_close_at, t.closure_code,
                (SELECT coalesce(sum(w.minutes), 0) FROM ticket_worklogs w WHERE w.ticket_id = t.id) AS work_minutes,
                t.impact, t.urgency, t.group_id, (SELECT g.name FROM engineer_groups g WHERE g.id = t.group_id) AS group_name,
                {SLA_STATE} AS sla_state, (SELECT sp.name FROM sla_policies sp WHERE sp.id = t.sla_policy_id) AS sla_policy_name,
                t.response_due_at, t.resolve_due_at, t.sla_paused_at IS NOT NULL AS sla_paused,
                coalesce(p.is_vip, false) AS requester_vip,
                (SELECT value FROM ticket_ratings g WHERE g.ticket_id = t.id) AS rating,
                (SELECT comment FROM ticket_ratings g WHERE g.ticket_id = t.id) AS rating_comment,
                (SELECT speed FROM ticket_ratings g WHERE g.ticket_id = t.id) AS rating_speed,
                (SELECT quality FROM ticket_ratings g WHERE g.ticket_id = t.id) AS rating_quality,
                (SELECT courtesy FROM ticket_ratings g WHERE g.ticket_id = t.id) AS rating_courtesy
           {JOINS}
          WHERE t.number = $1 AND ($2 OR t.requester_user_id = $3 OR t.requester_id = $4
                OR EXISTS (SELECT 1 FROM ticket_watchers w WHERE w.ticket_id = t.id AND w.person_id = $4))" // SAFE: константы
    );
    let mut ticket: TicketHead = sqlx::query_as(&sql)
        .bind(number)
        .bind(who.engineer)
        .bind(user.user_id)
        .bind(user.person_id)
        .fetch_optional(&core.db)
        .await?
        .ok_or(ApiError::NotFound)?;
    let is_requester = ticket.requester_user_id == Some(user.user_id)
        || (user.person_id.is_some() && ticket.requester_person_id == user.person_id);
    let watchers = card::watchers(&core.db, ticket.id).await?;
    let is_watcher = user.person_id.is_some_and(|p| watchers.iter().any(|w| w.person_id == p));

    #[derive(sqlx::FromRow)]
    struct Cm {
        id: Uuid,
        created_at: OffsetDateTime,
        author: Option<String>,
        visibility: String,
        body: String,
        source: String,
        minutes: Option<i32>,
    }
    let comments: Vec<Cm> = sqlx::query_as(
        "SELECT c.id, c.created_at, c.visibility, c.body, c.source,
                CASE WHEN $2 THEN (SELECT sum(w.minutes)::int FROM ticket_worklogs w WHERE w.comment_id = c.id) END AS minutes,
                coalesce(nullif(trim(concat_ws(' ', p.last_name, p.first_name)), ''), a.display_name, a.sam_account_name, c.author_name) AS author
           FROM ticket_comments c
           LEFT JOIN users u ON u.id = c.author_id
           LEFT JOIN ad_accounts a ON a.id = u.ad_account_id
           LEFT JOIN persons p ON p.id = u.person_id
          WHERE c.ticket_id = $1 AND ($2 OR c.visibility = 'public')
          ORDER BY c.created_at",
    )
    .bind(ticket.id)
    .bind(who.engineer)
    .fetch_all(&core.db)
    .await?;
    let attachments: Vec<AttachmentRow> = sqlx::query_as(
        "SELECT f.id, a.comment_id, f.original_name, f.mime, f.size
           FROM ticket_attachments a JOIN files f ON f.id = a.file_id
           LEFT JOIN ticket_comments c ON c.id = a.comment_id
          WHERE a.ticket_id = $1 AND ($2 OR c.id IS NULL OR c.visibility = 'public')
          ORDER BY a.created_at",
    )
    .bind(ticket.id)
    .bind(who.engineer)
    .fetch_all(&core.db)
    .await?;
    #[derive(sqlx::FromRow)]
    struct Ev {
        id: Uuid,
        ts: OffsetDateTime,
        kind: String,
        from_val: Option<Value>,
        to_val: Option<Value>,
        author: Option<String>,
    }
    // Сотруднику — только создание и смена статуса.
    let events: Vec<Ev> = sqlx::query_as(
        "SELECT e.id, e.ts, e.kind, e.from_val, e.to_val,
                coalesce(nullif(trim(concat_ws(' ', p.last_name, p.first_name)), ''), a.display_name, a.sam_account_name) AS author
           FROM ticket_events e
           LEFT JOIN users u ON u.id = e.actor_id
           LEFT JOIN ad_accounts a ON a.id = u.ad_account_id
           LEFT JOIN persons p ON p.id = u.person_id
          WHERE e.ticket_id = $1 AND ($2 OR e.kind IN ('created', 'status'))
          ORDER BY e.ts",
    )
    .bind(ticket.id)
    .bind(who.engineer)
    .fetch_all(&core.db)
    .await?;

    let mut feed: Vec<FeedItem> = comments
        .into_iter()
        .map(|c| FeedItem {
            attachments: attachments.iter().filter(|a| a.comment_id == Some(c.id)).map(view).collect(),
            id: c.id,
            item_type: "comment".into(),
            at: c.created_at,
            author: c.author,
            internal: c.visibility == "internal",
            body: Some(c.body),
            source: Some(c.source),
            kind: None,
            from: None,
            to: None,
            minutes: c.minutes,
        })
        .chain(events.into_iter().map(|e| FeedItem {
            id: e.id,
            item_type: "event".into(),
            at: e.ts,
            author: e.author,
            internal: false,
            body: None,
            source: None,
            kind: Some(e.kind),
            from: e.from_val,
            to: e.to_val,
            attachments: Vec::new(),
            minutes: None,
        }))
        .collect();
    feed.sort_by_key(|f| f.at);
    let top: Vec<AttachmentView> = attachments.iter().filter(|a| a.comment_id.is_none()).map(view).collect();
    let children: Vec<(i64, String, String)> = if ticket.is_major && who.engineer {
        sqlx::query_as("SELECT number, title, status FROM tickets WHERE parent_ticket_id = $1 ORDER BY number")
            .bind(ticket.id)
            .fetch_all(&core.db)
            .await?
    } else {
        Vec::new()
    };
    sqlx::query("INSERT INTO ticket_reads (user_id, ticket_id) VALUES ($1, $2) ON CONFLICT (user_id, ticket_id) DO UPDATE SET read_at = now()")
        .bind(user.user_id)
        .bind(ticket.id)
        .execute(&core.db)
        .await?;
    // Заявителю и наблюдателю — без внутренней кухни: задачи инженеров (с действиями AD), трудозатраты,
    // политика SLA и пометка VIP — служебное.
    let checklist = if who.engineer { checklist::items(&core.db, ticket.id).await? } else { Vec::new() };
    if !who.engineer {
        ticket.work_minutes = 0;
        ticket.sla_policy_name = None;
        ticket.requester_vip = false;
    }
    // Заявителю и наблюдателю — только оборудование (его выбирает и сам сотрудник), без учёток,
    // компьютеров домена и связей заявок.
    let (objects, links) = if who.engineer {
        (crate::objects::of_ticket(&core.db, ticket.id).await?, crate::objects::links_of(&core.db, ticket.id).await?)
    } else {
        let mut o = crate::objects::of_ticket(&core.db, ticket.id).await?;
        o.retain(|x| x.kind == "asset");
        (o, Vec::new())
    };
    Ok(Json(TicketDetail {
        objects,
        links,
        ticket,
        feed,
        attachments: top,
        children: children.into_iter().map(|(number, title, status)| ChildTicket { number, title, status }).collect(),
        watchers,
        checklist,
        is_requester,
        is_watcher,
        can_work: who.engineer,
    }))
}

/// Изменить приоритет, категорию, площадку, тему.
#[utoipa::path(put, path = "/api/helpdesk/tickets/{number}", tag = "helpdesk", params(("number" = i64, Path)),
    request_body = TicketUpdate, responses((status = 204), (status = 409, body = pult_core::error::ErrorBody)))]
async fn update_ticket(
    State(core): State<Core>,
    user: CurrentUser,
    Path(number): Path<i64>,
    Json(b): Json<TicketUpdate>,
) -> ApiResult<StatusCode> {
    tickets::update(&core, &Who::from_user(&user), number, b).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, ToSchema)]
pub struct CommentInput {
    pub body: String,
    #[serde(default)]
    pub internal: bool,
    #[serde(default)]
    pub files: Vec<Uuid>,
    /// В главной заявке — разослать во все привязанные.
    #[serde(default)]
    pub broadcast: bool,
    /// Затраченное время, минут (инженер).
    pub minutes: Option<i32>,
}

/// Ответ или внутренняя заметка.
#[utoipa::path(post, path = "/api/helpdesk/tickets/{number}/comments", tag = "helpdesk", params(("number" = i64, Path)),
    request_body = CommentInput, responses((status = 200, body = CommentResult)))]
async fn add_comment(
    State(core): State<Core>,
    user: CurrentUser,
    Path(number): Path<i64>,
    Json(b): Json<CommentInput>,
) -> ApiResult<Json<CommentResult>> {
    let input = NewComment {
        body: b.body,
        internal: b.internal,
        files: b.files,
        files_owner: Some(user.user_id),
        broadcast: b.broadcast,
        minutes: b.minutes,
        ..Default::default()
    };
    Ok(Json(tickets::comment(&core, &Who::from_user(&user), number, input).await?))
}

/// Взять себе.
#[utoipa::path(post, path = "/api/helpdesk/tickets/{number}/take", tag = "helpdesk", params(("number" = i64, Path)), responses((status = 204)))]
async fn take_ticket(State(core): State<Core>, user: CurrentUser, Path(number): Path<i64>) -> ApiResult<StatusCode> {
    tickets::take(&core, &Who::from_user(&user), number).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, ToSchema)]
pub struct OptionalNote {
    pub comment: Option<String>,
}

/// Перевести в «Ждёт ответа».
#[utoipa::path(post, path = "/api/helpdesk/tickets/{number}/waiting", tag = "helpdesk", params(("number" = i64, Path)), request_body = OptionalNote, responses((status = 204)))]
async fn set_waiting(
    State(core): State<Core>,
    user: CurrentUser,
    Path(number): Path<i64>,
    Json(b): Json<OptionalNote>,
) -> ApiResult<StatusCode> {
    tickets::waiting(&core, &Who::from_user(&user), number, b.comment).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, ToSchema)]
pub struct ResolveInput {
    pub comment: String,
    #[serde(default)]
    pub files: Vec<Uuid>,
    /// Код закрытия из списка `/api/helpdesk/close-codes`.
    #[serde(default)]
    pub closure_code: String,
    pub minutes: Option<i32>,
}

/// Решить. Обязателен публичный комментарий решения.
#[utoipa::path(post, path = "/api/helpdesk/tickets/{number}/resolve", tag = "helpdesk", params(("number" = i64, Path)), request_body = ResolveInput, responses((status = 204)))]
async fn resolve_ticket(
    State(core): State<Core>,
    user: CurrentUser,
    Path(number): Path<i64>,
    Json(b): Json<ResolveInput>,
) -> ApiResult<StatusCode> {
    let r = Resolution { body: &b.comment, files: &b.files, closure_code: &b.closure_code, minutes: b.minutes };
    tickets::resolve(&core, &Who::from_user(&user), number, r).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Заявитель: «Да, решена».
#[utoipa::path(post, path = "/api/helpdesk/tickets/{number}/confirm", tag = "helpdesk", params(("number" = i64, Path)), responses((status = 204)))]
async fn confirm_ticket(State(core): State<Core>, user: CurrentUser, Path(number): Path<i64>) -> ApiResult<StatusCode> {
    tickets::confirm(&core, &Who::from_user(&user), number).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, ToSchema)]
pub struct RequiredComment {
    pub comment: String,
}

/// Заявитель: «Нет, не решена».
#[utoipa::path(post, path = "/api/helpdesk/tickets/{number}/not-resolved", tag = "helpdesk", params(("number" = i64, Path)), request_body = RequiredComment, responses((status = 204)))]
async fn not_resolved(
    State(core): State<Core>,
    user: CurrentUser,
    Path(number): Path<i64>,
    Json(b): Json<RequiredComment>,
) -> ApiResult<StatusCode> {
    tickets::not_resolved(&core, &Who::from_user(&user), number, &b.comment).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Отменить заявку.
#[utoipa::path(post, path = "/api/helpdesk/tickets/{number}/cancel", tag = "helpdesk", params(("number" = i64, Path)), request_body = OptionalNote, responses((status = 204)))]
async fn cancel_ticket(
    State(core): State<Core>,
    user: CurrentUser,
    Path(number): Path<i64>,
    Json(b): Json<OptionalNote>,
) -> ApiResult<StatusCode> {
    tickets::cancel(&core, &Who::from_user(&user), number, b.comment).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Открыть закрытую заявку заново (суперадминистратор).
#[utoipa::path(post, path = "/api/helpdesk/tickets/{number}/reopen", tag = "helpdesk", params(("number" = i64, Path)), responses((status = 204)))]
async fn reopen_ticket(State(core): State<Core>, user: CurrentUser, Path(number): Path<i64>) -> ApiResult<StatusCode> {
    tickets::reopen_closed(&core, &Who::from_user(&user), number).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, ToSchema)]
pub struct TransferInput {
    pub assignee_id: Uuid,
    /// `l1` или `l2`.
    pub line: String,
    pub comment: String,
}

/// Передать другому инженеру или на вторую линию.
#[utoipa::path(post, path = "/api/helpdesk/tickets/{number}/transfer", tag = "helpdesk", params(("number" = i64, Path)), request_body = TransferInput, responses((status = 204)))]
async fn transfer_ticket(
    State(core): State<Core>,
    user: CurrentUser,
    Path(number): Path<i64>,
    Json(b): Json<TransferInput>,
) -> ApiResult<StatusCode> {
    tickets::transfer(&core, &Who::from_user(&user), number, b.assignee_id, &b.line, &b.comment).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, ToSchema)]
pub struct VerifyInput {
    pub person_id: Uuid,
}

/// Личность подтверждена: связать заявку «Не могу войти» с сотрудником.
#[utoipa::path(post, path = "/api/helpdesk/tickets/{number}/verify-identity", tag = "helpdesk", params(("number" = i64, Path)), request_body = VerifyInput, responses((status = 204)))]
async fn verify_identity(
    State(core): State<Core>,
    user: CurrentUser,
    Path(number): Path<i64>,
    Json(b): Json<VerifyInput>,
) -> ApiResult<StatusCode> {
    tickets::verify_identity(&core, &Who::from_user(&user), number, b.person_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Сделать главной и объявить о массовом сбое.
#[utoipa::path(post, path = "/api/helpdesk/tickets/{number}/major", tag = "helpdesk", params(("number" = i64, Path)), request_body = MajorInput, responses((status = 204)))]
async fn make_major(
    State(core): State<Core>,
    user: CurrentUser,
    Path(number): Path<i64>,
    Json(b): Json<MajorInput>,
) -> ApiResult<StatusCode> {
    tickets::make_major(&core, &Who::from_user(&user), number, b).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, ToSchema)]
pub struct LinkInput {
    /// Номер главной; пусто — отвязать.
    pub parent: Option<i64>,
}

/// Привязать к главной заявке.
#[utoipa::path(post, path = "/api/helpdesk/tickets/{number}/link", tag = "helpdesk", params(("number" = i64, Path)), request_body = LinkInput, responses((status = 204)))]
async fn link_ticket(
    State(core): State<Core>,
    user: CurrentUser,
    Path(number): Path<i64>,
    Json(b): Json<LinkInput>,
) -> ApiResult<StatusCode> {
    tickets::link(&core, &Who::from_user(&user), number, b.parent).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, ToSchema)]
pub struct AssignInput {
    /// Пусто — вернуть в очередь.
    pub assignee_id: Option<Uuid>,
    pub comment: Option<String>,
}

/// Назначить исполнителя или вернуть заявку в очередь.
#[utoipa::path(post, path = "/api/helpdesk/tickets/{number}/assign", tag = "helpdesk", params(("number" = i64, Path)), request_body = AssignInput, responses((status = 204)))]
async fn assign_ticket(
    State(core): State<Core>,
    user: CurrentUser,
    Path(number): Path<i64>,
    Json(b): Json<AssignInput>,
) -> ApiResult<StatusCode> {
    card::assign(&core, &Who::from_user(&user), number, b.assignee_id, b.comment.as_deref()).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, ToSchema)]
pub struct HoldInput {
    pub comment: String,
    pub minutes: Option<i32>,
}

/// «Отложена»: ждём поставки или подрядчика.
#[utoipa::path(post, path = "/api/helpdesk/tickets/{number}/hold", tag = "helpdesk", params(("number" = i64, Path)), request_body = HoldInput, responses((status = 204)))]
async fn hold_ticket(
    State(core): State<Core>,
    user: CurrentUser,
    Path(number): Path<i64>,
    Json(b): Json<HoldInput>,
) -> ApiResult<StatusCode> {
    card::hold(&core, &Who::from_user(&user), number, &b.comment, b.minutes).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, ToSchema)]
pub struct WatcherInput {
    pub person_id: Uuid,
}

/// Добавить наблюдателя.
#[utoipa::path(post, path = "/api/helpdesk/tickets/{number}/watchers", tag = "helpdesk", params(("number" = i64, Path)), request_body = WatcherInput, responses((status = 204)))]
async fn add_watcher(
    State(core): State<Core>,
    user: CurrentUser,
    Path(number): Path<i64>,
    Json(b): Json<WatcherInput>,
) -> ApiResult<StatusCode> {
    card::add_watcher(&core, &Who::from_user(&user), number, b.person_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Убрать наблюдателя (себя — может сам наблюдатель).
#[utoipa::path(delete, path = "/api/helpdesk/tickets/{number}/watchers/{person_id}", tag = "helpdesk",
    params(("number" = i64, Path), ("person_id" = Uuid, Path)), responses((status = 204)))]
async fn remove_watcher(
    State(core): State<Core>,
    user: CurrentUser,
    Path((number, person)): Path<(i64, Uuid)>,
) -> ApiResult<StatusCode> {
    card::remove_watcher(&core, &Who::from_user(&user), number, person).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, ToSchema)]
pub struct ChecklistInput {
    pub items: Vec<NewItem>,
}

/// Добавить пункты чек-листа.
#[utoipa::path(post, path = "/api/helpdesk/tickets/{number}/checklist", tag = "helpdesk", params(("number" = i64, Path)), request_body = ChecklistInput, responses((status = 204)))]
async fn add_checklist(
    State(core): State<Core>,
    user: CurrentUser,
    Path(number): Path<i64>,
    Json(b): Json<ChecklistInput>,
) -> ApiResult<StatusCode> {
    checklist::add(&core, &Who::from_user(&user), number, &b.items).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Отметить или изменить пункт чек-листа.
#[utoipa::path(put, path = "/api/helpdesk/tickets/{number}/checklist/{id}", tag = "helpdesk",
    params(("number" = i64, Path), ("id" = Uuid, Path)), request_body = ItemUpdate, responses((status = 204)))]
async fn update_checklist_item(
    State(core): State<Core>,
    user: CurrentUser,
    Path((number, id)): Path<(i64, Uuid)>,
    Json(b): Json<ItemUpdate>,
) -> ApiResult<StatusCode> {
    checklist::update(&core, &Who::from_user(&user), number, id, b).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Удалить пункт чек-листа.
#[utoipa::path(delete, path = "/api/helpdesk/tickets/{number}/checklist/{id}", tag = "helpdesk",
    params(("number" = i64, Path), ("id" = Uuid, Path)), responses((status = 204)))]
async fn delete_checklist_item(
    State(core): State<Core>,
    user: CurrentUser,
    Path((number, id)): Path<(i64, Uuid)>,
) -> ApiResult<StatusCode> {
    checklist::remove(&core, &Who::from_user(&user), number, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Коды закрытия для «Решить».
#[utoipa::path(get, path = "/api/helpdesk/close-codes", tag = "helpdesk", responses((status = 200, body = Vec<CloseCode>)))]
async fn list_close_codes(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<Vec<CloseCode>>> {
    user.require(&perm::WORK)?;
    let cfg: HelpdeskSettings = settings::load(&core.db).await?;
    Ok(Json(crate::settings::localized(cfg.close_codes)))
}

// ------------------------------------------------------------------ Форма создания

#[derive(Serialize, ToSchema)]
pub struct TicketContext {
    pub site_id: Option<Uuid>,
    pub computer_name: Option<String>,
    /// Оборудование, за которое отвечает сотрудник: выбрать в форме.
    pub assets: Vec<crate::objects::FoundObject>,
    pub after_hours: Option<crate::schedule::AfterHours>,
    pub max_file_mb: u32,
}

/// Что подставить в форму заявки: площадка сотрудника, ПК из активного сеанса, его оборудование для
/// выбора, рабочее ли время. Кабинет и телефон не подставляются — их указывает человек.
#[utoipa::path(get, path = "/api/helpdesk/context", tag = "helpdesk", responses((status = 200, body = TicketContext)))]
async fn context(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<TicketContext>> {
    let site_id: Option<Uuid> = match user.person_id {
        // Выключенная площадка в форму не подставляется: с ней заявку не создать.
        Some(p) => sqlx::query_scalar(
            "SELECT p.site_id FROM persons p JOIN sites s ON s.id = p.site_id AND s.is_active WHERE p.id = $1",
        )
        .bind(p)
        .fetch_optional(&core.db)
        .await?
        .flatten(),
        None => None,
    };
    let assets = match user.person_id {
        Some(p) => crate::objects::assets_of(&core.db, p).await?,
        None => Vec::new(),
    };
    let cfg: HelpdeskSettings = settings::load(&core.db).await?;
    Ok(Json(TicketContext {
        site_id,
        assets,
        // Сначала данные агента (ПК, где у сотрудника активная сессия), затем — адрес запроса.
        computer_name: match pult_people::computers::of_session(&core.db, Some(user.ad_account_id), user.actor.ip)
            .await?
        {
            Some((_, name)) => Some(name),
            None => pult_people::computers::of_client(&core.db, user.actor.ip).await?,
        },
        after_hours: crate::schedule::after_hours(&core.db, site_id, &cfg).await?,
        max_file_mb: cfg.max_file_mb,
    }))
}

#[derive(Deserialize, IntoParams)]
pub struct ComputersQuery {
    /// Часть имени, DNS-имени, описания или точный IP.
    pub q: Option<String>,
    pub limit: Option<i64>,
}

/// Компьютеры домена: подсказки инженеру. Сотруднику список машин не показываем.
#[utoipa::path(get, path = "/api/helpdesk/computers", tag = "helpdesk", params(ComputersQuery),
    responses((status = 200, body = Vec<ComputerRow>)))]
async fn list_computers(
    State(core): State<Core>,
    user: CurrentUser,
    Query(q): Query<ComputersQuery>,
) -> ApiResult<Json<Vec<ComputerRow>>> {
    if user.require(&perm::WORK).is_err() {
        user.require(&perm::ON_BEHALF)?;
    }
    let rows = pult_people::computers::search(&core.db, q.q.as_deref().unwrap_or(""), q.limit.unwrap_or(10)).await?;
    Ok(Json(rows))
}

#[derive(Deserialize, IntoParams)]
pub struct EngineersQuery {
    pub site_id: Option<Uuid>,
}

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct EngineerView {
    pub user_id: Uuid,
    pub name: String,
    pub lines: Vec<String>,
    /// Ответственный за площадку заявки — показать первым.
    pub at_site: bool,
    pub load: i64,
    pub absent: bool,
}

/// Инженеры для передачи заявки.
#[utoipa::path(get, path = "/api/helpdesk/engineers", tag = "helpdesk", params(EngineersQuery), responses((status = 200, body = Vec<EngineerView>)))]
async fn list_engineers(
    State(core): State<Core>,
    user: CurrentUser,
    Query(q): Query<EngineersQuery>,
) -> ApiResult<Json<Vec<EngineerView>>> {
    user.require(&perm::WORK)?;
    let rows = sqlx::query_as(
        "SELECT u.id AS user_id,
                coalesce(nullif(trim(concat_ws(' ', p.last_name, p.first_name)), ''), a.display_name, a.sam_account_name) AS name,
                array_agg(DISTINCT se.line) AS lines,
                coalesce(bool_or(se.site_id = $1), false) AS at_site,
                (SELECT count(*) FROM tickets t WHERE t.assignee_id = u.id AND t.status = ANY($2)) AS load,
                EXISTS (SELECT 1 FROM engineer_absences ab WHERE ab.user_id = u.id AND ab.starts_at <= now() AND ab.ends_at > now()) AS absent
           FROM site_engineers se
           JOIN users u ON u.id = se.user_id
           JOIN ad_accounts a ON a.id = u.ad_account_id
           LEFT JOIN persons p ON p.id = u.person_id
          WHERE u.is_active AND a.is_enabled AND NOT a.is_missing
          GROUP BY u.id, a.id, p.id
          ORDER BY at_site DESC, name",
    )
    .bind(q.site_id)
    .bind(crate::assignment::LOAD_STATUSES)
    .fetch_all(&core.db)
    .await?;
    Ok(Json(rows))
}

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct AnnouncementView {
    pub id: Uuid,
    pub title: String,
    pub body: String,
    pub ticket_number: Option<i64>,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub starts_at: OffsetDateTime,
}

/// Действующие объявления: своей площадки, инженерам — все.
#[utoipa::path(get, path = "/api/helpdesk/announcements", tag = "helpdesk", responses((status = 200, body = Vec<AnnouncementView>)))]
async fn list_announcements(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<Vec<AnnouncementView>>> {
    let site: Option<Uuid> = match user.person_id {
        Some(p) => sqlx::query_scalar("SELECT site_id FROM persons WHERE id = $1").bind(p).fetch_one(&core.db).await?,
        None => None,
    };
    let rows = sqlx::query_as(
        "SELECT a.id, a.title, a.body, a.starts_at, t.number AS ticket_number
           FROM announcements a LEFT JOIN tickets t ON t.id = a.ticket_id
          WHERE a.starts_at <= now() AND (a.ends_at IS NULL OR a.ends_at > now())
            AND ($1 OR cardinality(a.site_ids) = 0 OR $2 = ANY(a.site_ids))
          ORDER BY a.starts_at DESC",
    )
    .bind(user.grants.has(&perm::WORK))
    .bind(site)
    .fetch_all(&core.db)
    .await?;
    Ok(Json(rows))
}

/// Снять объявление вручную.
#[utoipa::path(delete, path = "/api/helpdesk/announcements/{id}", tag = "helpdesk", params(("id" = Uuid, Path)), responses((status = 204)))]
async fn end_announcement(State(core): State<Core>, user: CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    user.require(&perm::WORK)?;
    let n =
        sqlx::query("UPDATE announcements SET ends_at = now() WHERE id = $1 AND (ends_at IS NULL OR ends_at > now())")
            .bind(id)
            .execute(&core.db)
            .await?
            .rows_affected();
    if n == 0 {
        return Err(ApiError::NotFound);
    }
    audit::record(&core.db, &user.actor, Entry::new("announcement.end", Source::Portal).object("announcement", id))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ------------------------------------------------------------------ Файлы

#[derive(Serialize, ToSchema)]
pub struct Uploaded {
    pub id: Uuid,
    pub name: String,
    pub is_image: bool,
}

#[derive(ToSchema)]
#[allow(dead_code)]
pub struct UploadForm {
    #[schema(format = Binary, value_type = String)]
    file: Vec<u8>,
}

/// Загрузить файл; прикрепляется к заявке при её создании или в ответе.
#[utoipa::path(post, path = "/api/helpdesk/files", tag = "helpdesk",
    request_body(content_type = "multipart/form-data", content = inline(UploadForm)),
    responses((status = 200, body = Uploaded), (status = 400, body = pult_core::error::ErrorBody)))]
async fn upload_file(
    State(core): State<Core>,
    State(st): State<HelpdeskState>,
    user: CurrentUser,
    mut form: Multipart,
) -> ApiResult<Json<Uploaded>> {
    let bad = |m: &str| ApiError::bad_request("invalid_file", m);
    let _mine = UserUpload::start(user.user_id).ok_or_else(|| ApiError::TooManyRequests {
        message: "Дождитесь окончания предыдущих загрузок.".into(),
        retry_after_secs: 10,
    })?;
    // Файл держится в памяти, пока не записан: одновременных загрузок — не больше UPLOADS_AT_ONCE.
    let _slot = UPLOADS.try_acquire().map_err(|_| ApiError::TooManyRequests {
        message: "Сервер сейчас принимает много файлов. Повторите через минуту.".into(),
        retry_after_secs: 60,
    })?;
    // Загруженные, но не прикреплённые файлы лежат сутки: без квоты один пользователь заполнил бы диск.
    let (count, bytes): (i64, i64) = sqlx::query_as(
        "SELECT count(*), coalesce(sum(size), 0)::bigint FROM files f
          WHERE f.uploaded_by = $1 AND f.created_at > now() - interval '1 day'
            AND NOT EXISTS (SELECT 1 FROM ticket_attachments a WHERE a.file_id = f.id)",
    )
    .bind(user.user_id)
    .fetch_one(&core.db)
    .await?;
    if count >= UNATTACHED_MAX_FILES || bytes >= UNATTACHED_MAX_MB * 1024 * 1024 {
        return Err(ApiError::TooManyRequests {
            message: "Слишком много файлов загружено и не прикреплено к заявкам. Прикрепите их или повторите завтра."
                .into(),
            retry_after_secs: 3600,
        });
    }
    let cfg: HelpdeskSettings = settings::load(&core.db).await?;
    let limit = usize::try_from(cfg.max_file_mb).unwrap_or(usize::MAX).saturating_mul(1024 * 1024);
    let too_large = || ApiError::bad_request("file_too_large", format!("Файл больше {} МБ.", cfg.max_file_mb));
    let mut field =
        form.next_field().await.map_err(|_| bad("Файл не получен."))?.ok_or_else(|| bad("Файл не получен."))?;
    let name = field.file_name().unwrap_or("file").to_string();
    // По частям и с обрывом на пределе: не буферизовать сначала все 100 МБ тела.
    let mut data = Vec::new();
    while let Some(chunk) = field.chunk().await.map_err(|_| bad("Загрузка оборвалась."))? {
        if data.len() + chunk.len() > limit {
            return Err(too_large());
        }
        data.extend_from_slice(&chunk);
    }
    let id = files::store(&core, &st, &name, &data, Some(user.user_id)).await?;
    let (name, mime): (String, String) =
        sqlx::query_as("SELECT original_name, mime FROM files WHERE id = $1").bind(id).fetch_one(&core.db).await?;
    Ok(Json(Uploaded { id, is_image: files::is_inline_image(&mime), name }))
}

#[derive(sqlx::FromRow)]
struct FileAccess {
    sha256: String,
    mime: String,
    original_name: String,
    uploaded_by: Option<Uuid>,
    ticket_id: Option<Uuid>,
    internal: bool,
    requester_id: Option<Uuid>,
    requester_user_id: Option<Uuid>,
    assignee_id: Option<Uuid>,
    watcher: bool,
}

fn content_disposition(kind: &str, name: &str) -> String {
    let encoded: String = name
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"._-".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }
        })
        .collect();
    format!("{kind}; filename*=UTF-8''{encoded}")
}

/// Скачать вложение. Права — как на заявку; вложение внутренней заметки сотруднику недоступно.
#[utoipa::path(get, path = "/api/helpdesk/files/{id}", tag = "helpdesk", params(("id" = Uuid, Path)),
    responses((status = 200, description = "Содержимое файла"), (status = 404, body = pult_core::error::ErrorBody)))]
async fn download_file(
    State(core): State<Core>,
    State(st): State<HelpdeskState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let f: FileAccess = sqlx::query_as(
        "SELECT f.sha256, f.mime, f.original_name, f.uploaded_by, a.ticket_id,
                coalesce(c.visibility = 'internal', false) AS internal,
                t.requester_id, t.requester_user_id, t.assignee_id,
                coalesce(EXISTS (SELECT 1 FROM ticket_watchers w WHERE w.ticket_id = a.ticket_id AND w.person_id = $2), false) AS watcher
           FROM files f
           LEFT JOIN ticket_attachments a ON a.file_id = f.id
           LEFT JOIN ticket_comments c ON c.id = a.comment_id
           LEFT JOIN tickets t ON t.id = a.ticket_id
          WHERE f.id = $1",
    )
    .bind(id)
    .bind(user.person_id)
    .fetch_optional(&core.db)
    .await?
    .ok_or(ApiError::NotFound)?;
    let engineer = user.grants.has(&perm::WORK);
    let requester = f.requester_user_id == Some(user.user_id)
        || (user.person_id.is_some() && f.requester_id == user.person_id)
        || f.watcher;
    let allowed = match f.ticket_id {
        None => f.uploaded_by == Some(user.user_id),
        Some(_) => engineer || (requester && !f.internal),
    };
    if !allowed {
        return Err(ApiError::NotFound);
    }
    if engineer && !requester && f.ticket_id.is_some() && f.assignee_id != Some(user.user_id) {
        audit::record(
            &core.db,
            &user.actor,
            Entry::new("ticket.file_download", Source::Portal)
                .object("file", id)
                .details(json!({"ticket": f.ticket_id})),
        )
        .await?;
    }
    let data = tokio::fs::read(files::path_of(&st, &f.sha256)).await.map_err(|_| ApiError::NotFound)?;
    let kind = if files::is_inline_image(&f.mime) { "inline" } else { "attachment" };
    Ok((
        [
            (header::CONTENT_TYPE, f.mime),
            (header::CONTENT_DISPOSITION, content_disposition(kind, &f.original_name)),
            // Браузер не хранит вложение: на общем компьютере следующий пользователь открыл бы его из кэша.
            (header::CACHE_CONTROL, "no-store".to_string()),
        ],
        data,
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sort_is_whitelisted() {
        assert_eq!(sort_column(Some("1; DROP TABLE tickets")), "t.updated_at");
        assert_eq!(sort_column(Some("number")), "t.number");
    }

    #[test]
    fn one_user_cannot_take_all_upload_slots() {
        let (me, other) = (Uuid::now_v7(), Uuid::now_v7());
        let a = UserUpload::start(me).unwrap();
        let _b = UserUpload::start(me).unwrap();
        assert!(UserUpload::start(me).is_none(), "третья одновременная загрузка одного пользователя");
        assert!(UserUpload::start(other).is_some(), "другим место есть");
        drop(a);
        assert!(UserUpload::start(me).is_some(), "место освободилось");
    }

    #[test]
    fn file_names_cannot_break_headers() {
        let h = content_disposition("attachment", "скрин\r\nX: y.png");
        assert!(!h.contains('\r') && !h.contains('\n') && h.starts_with("attachment; filename*=UTF-8''"));
    }
}

//! Запросы доступа и действия каталога AD из заявки.
//!
//! - Каталог ресурсов: группа AD, общая папка, приложение, прочее; у каждого ровно один владелец.
//! - Заявка категории «Запрос доступа» — один ресурс; назначается по обычным правилам.
//! - Исполнитель отправляет на согласование; владелец решает в «Согласованиях»;
//!   владелец недоступен — заявка ждёт; смена владельца переносит ожидающие.
//! - Согласованный доступ к группе AD — кнопка «Выполнить»: операция каталога, повторное чтение, итог в ленте.
//! - Пункты чек-листа с действием каталога и быстрые действия карточки идут тем же путём (`pult-directory`).

use crate::{
    checklist,
    notify::{self, Msg},
    tickets::{self, NewComment, T, Who},
};
use axum::{
    Json,
    extract::{FromRef, Path, State},
    http::StatusCode,
};
use pult_core::{
    ApiError, ApiResult, Core, CurrentUser, Permission,
    audit::{self, Entry, Source},
    l10n::{self, tr_to},
    rbac::ROLE_SUPERADMIN,
};
use pult_directory::ops::{self, Kind, NewOperation, OperationView, Target};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Postgres, Transaction};
use time::OffsetDateTime;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// Код служебной категории «Запрос доступа».
pub const CATEGORY: &str = "access";

pub const RESOURCES: Permission = Permission {
    code: "helpdesk.access.resources",
    description: "Вести каталог ресурсов для запросов доступа и назначать их владельцев",
    default_roles: &[ROLE_SUPERADMIN],
};

const KINDS: &[&str] = &["ad_group", "share", "application", "other"];

pub fn routes<S>() -> OpenApiRouter<S>
where
    S: Clone + Send + Sync + 'static,
    Core: FromRef<S>,
{
    OpenApiRouter::new()
        .routes(routes!(list_resources))
        .routes(routes!(admin_resources, create_resource))
        .routes(routes!(update_resource))
        .routes(routes!(ticket_access))
        .routes(routes!(send_for_approval))
        .routes(routes!(execute_access))
        .routes(routes!(list_approvals))
        .routes(routes!(decide))
        .routes(routes!(my_access))
        .routes(routes!(run_checklist_item))
}

// ------------------------------------------------------------------ Ресурсы

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct Resource {
    id: Uuid,
    name: String,
    owner_id: Uuid,
}

#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
pub struct ResourceView {
    pub id: Uuid,
    /// `ad_group`, `share`, `application`, `other`.
    pub kind: String,
    pub name: String,
    pub description: String,
    /// Группа AD, в которую добавляет «Выполнить».
    pub group_dn: Option<String>,
    pub owner_id: Uuid,
    pub owner_name: Option<String>,
    pub is_active: bool,
}

const RES_VIEW: &str = "SELECT r.id, r.kind, r.name, r.description, r.group_dn, r.owner_id,
        nullif(trim(concat_ws(' ', p.last_name, p.first_name)), '') AS owner_name, r.is_active
   FROM access_resources r JOIN persons p ON p.id = r.owner_id";

#[derive(Debug, Deserialize, ToSchema)]
pub struct ResourceInput {
    pub kind: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub group_dn: Option<String>,
    pub owner_id: Uuid,
    #[serde(default = "yes")]
    pub is_active: bool,
}

fn yes() -> bool {
    true
}

/// Ресурс выбран и активен (форма заявки).
pub(crate) async fn check_resource(tx: &mut Transaction<'_, Postgres>, id: Option<Uuid>) -> ApiResult<Resource> {
    let Some(id) = id else {
        return Err(ApiError::bad_request("invalid_ticket", "Выберите ресурс, к которому нужен доступ."));
    };
    sqlx::query_as("SELECT id, name, owner_id FROM access_resources WHERE id = $1 AND is_active")
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| ApiError::bad_request("invalid_ticket", "Ресурс не найден."))
}

/// Новая заявка «Запрос доступа»: запрос в статусе «новый», ресурс — в заявке.
pub(crate) async fn open(tx: &mut Transaction<'_, Postgres>, t: &T, who: &Who, r: Resource) -> ApiResult<()> {
    sqlx::query("UPDATE tickets SET resource_id = $2 WHERE id = $1").bind(t.id).bind(r.id).execute(&mut **tx).await?;
    sqlx::query("INSERT INTO access_requests (ticket_id, resource_id, person_id) VALUES ($1, $2, $3)")
        .bind(t.id)
        .bind(r.id)
        .bind(t.requester_id)
        .execute(&mut **tx)
        .await?;
    tickets::audit_rec(tx, who, "access.request", t, json!({"resource": r.name, "owner": r.owner_id})).await?;
    Ok(())
}

/// Ресурсы для формы заявки — активные.
#[utoipa::path(get, path = "/api/helpdesk/access/resources", tag = "helpdesk", responses((status = 200, body = Vec<ResourceView>)))]
async fn list_resources(State(core): State<Core>, _user: CurrentUser) -> ApiResult<Json<Vec<ResourceView>>> {
    Ok(Json(
        sqlx::query_as(&format!("{RES_VIEW} WHERE r.is_active ORDER BY r.name")).fetch_all(&core.db).await?, // SAFE: константа
    ))
}

/// Все ресурсы, с неактивными — для админки.
#[utoipa::path(get, path = "/api/admin/helpdesk/access/resources", tag = "helpdesk", responses((status = 200, body = Vec<ResourceView>)))]
async fn admin_resources(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<Vec<ResourceView>>> {
    user.require(&RESOURCES)?;
    Ok(Json(sqlx::query_as(&format!("{RES_VIEW} ORDER BY r.is_active DESC, r.name")).fetch_all(&core.db).await?)) // SAFE: константа
}

fn validated(b: ResourceInput) -> ApiResult<ResourceInput> {
    if !KINDS.contains(&b.kind.as_str()) {
        return Err(ApiError::bad_request(
            "invalid_resource",
            "Тип ресурса — группа AD, папка, приложение или прочее.",
        ));
    }
    let name = b.name.trim().to_string();
    if name.is_empty() || name.chars().count() > 200 {
        return Err(ApiError::bad_request("invalid_resource", "Название — от 1 до 200 символов."));
    }
    let group_dn = b.group_dn.map(|g| g.trim().to_string()).filter(|g| !g.is_empty());
    if let Some(g) = &group_dn
        && !pult_directory::zones::is_dn(g)
    {
        return Err(ApiError::bad_request("invalid_resource", "Группа AD — DN вида CN=…,OU=…,DC=…"));
    }
    if b.kind == "ad_group" && group_dn.is_none() {
        return Err(ApiError::bad_request("invalid_resource", "Для группы AD укажите её DN."));
    }
    let description = b.description.trim().chars().take(2000).collect();
    Ok(ResourceInput { name, group_dn, description, ..b })
}

async fn check_owner(tx: &mut Transaction<'_, Postgres>, owner: Uuid) -> ApiResult<()> {
    let ok: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM persons WHERE id = $1 AND NOT is_archived)")
        .bind(owner)
        .fetch_one(&mut **tx)
        .await?;
    if ok { Ok(()) } else { Err(ApiError::bad_request("invalid_resource", "Владелец не найден.")) }
}

fn unique_name(e: sqlx::Error) -> ApiError {
    match &e {
        sqlx::Error::Database(d) if d.code().as_deref() == Some("23505") => {
            ApiError::bad_request("invalid_resource", "Ресурс с таким названием уже есть.")
        }
        _ => e.into(),
    }
}

async fn resource_view(db: &sqlx::PgPool, id: Uuid) -> ApiResult<ResourceView> {
    Ok(sqlx::query_as(&format!("{RES_VIEW} WHERE r.id = $1")).bind(id).fetch_one(db).await?) // SAFE: константа
}

#[utoipa::path(post, path = "/api/admin/helpdesk/access/resources", tag = "helpdesk", request_body = ResourceInput,
    responses((status = 201, body = ResourceView)))]
async fn create_resource(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<ResourceInput>,
) -> ApiResult<(StatusCode, Json<ResourceView>)> {
    user.require(&RESOURCES)?;
    let b = validated(b)?;
    let mut tx = core.db.begin().await?;
    check_owner(&mut tx, b.owner_id).await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO access_resources (kind, name, description, group_dn, owner_id, is_active)
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
    )
    .bind(&b.kind)
    .bind(&b.name)
    .bind(&b.description)
    .bind(&b.group_dn)
    .bind(b.owner_id)
    .bind(b.is_active)
    .fetch_one(&mut *tx)
    .await
    .map_err(unique_name)?;
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("access.resource.create", Source::Portal)
            .object("access_resource", id)
            .details(json!({"name": b.name, "kind": b.kind, "group_dn": b.group_dn, "owner": b.owner_id})),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(resource_view(&core.db, id).await?)))
}

/// Изменить ресурс. Новый владелец получает ожидающие согласования прежнего.
#[utoipa::path(put, path = "/api/admin/helpdesk/access/resources/{id}", tag = "helpdesk", params(("id" = Uuid, Path)),
    request_body = ResourceInput, responses((status = 200, body = ResourceView)))]
async fn update_resource(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<ResourceInput>,
) -> ApiResult<Json<ResourceView>> {
    user.require(&RESOURCES)?;
    let b = validated(b)?;
    let c = notify::ctx(&core).await?;
    let lang = l10n::default_lang(&core.db).await;
    let mut tx = core.db.begin().await?;
    // Под блокировкой: две одновременные правки не разойдутся в том, кто владелец ожидающих согласований.
    let before: ResourceView = sqlx::query_as(&format!("{RES_VIEW} WHERE r.id = $1 FOR UPDATE OF r")) // SAFE: константа
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ApiError::NotFound)?;
    check_owner(&mut tx, b.owner_id).await?;
    sqlx::query(
        "UPDATE access_resources SET kind = $2, name = $3, description = $4, group_dn = $5, owner_id = $6, is_active = $7
          WHERE id = $1",
    )
    .bind(id)
    .bind(&b.kind)
    .bind(&b.name)
    .bind(&b.description)
    .bind(&b.group_dn)
    .bind(b.owner_id)
    .bind(b.is_active)
    .execute(&mut *tx)
    .await
    .map_err(unique_name)?;
    // Владелец согласовывал конкретную группу: сменилась группа или тип — прежнее согласование не действует.
    let mut reset = 0;
    if before.group_dn != b.group_dn || before.kind != b.kind {
        let tickets: Vec<Uuid> = sqlx::query_scalar(
            "UPDATE access_requests SET status = 'new', approver_id = NULL, decided_by = NULL, decided_at = NULL,
                    decision_comment = NULL
              WHERE resource_id = $1 AND status IN ('pending', 'approved') AND operation_id IS NULL RETURNING ticket_id",
        )
        .bind(id)
        .fetch_all(&mut *tx)
        .await?;
        reset = tickets.len();
        let body = tr_to(
            lang,
            "Ресурс изменён администратором (группа или тип): согласование снято, отправьте запрос владельцу заново.",
        )
        .into_owned();
        for t in tickets {
            let note = NewComment { body: body.clone(), internal: true, ..Default::default() };
            tickets::insert_comment(&mut tx, t, &Who::from_user(&user), &note).await?;
        }
    }
    let mut moved = 0;
    if before.owner_id != b.owner_id {
        let tickets: Vec<Uuid> = sqlx::query_scalar(
            "UPDATE access_requests SET approver_id = $2 WHERE resource_id = $1 AND status = 'pending' RETURNING ticket_id",
        )
        .bind(id)
        .bind(b.owner_id)
        .fetch_all(&mut *tx)
        .await?;
        moved = tickets.len();
        for t in tickets {
            let t: T = sqlx::query_as(&tickets::select_t("WHERE id = $1")).bind(t).fetch_one(&mut *tx).await?;
            ask_owner(&mut tx, &t, b.owner_id, &b.name, &c).await?;
        }
    }
    let d = audit::diff(&json!(before), &json!(resource_view_input(&b, id)));
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("access.resource.update", Source::Portal)
            .object("access_resource", id)
            .details(json!({"changes": d, "moved_approvals": moved, "reset_approvals": reset})),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(resource_view(&core.db, id).await?))
}

fn resource_view_input(b: &ResourceInput, id: Uuid) -> Value {
    json!({"id": id, "kind": b.kind, "name": b.name, "description": b.description, "group_dn": b.group_dn,
           "owner_id": b.owner_id, "is_active": b.is_active})
}

// ------------------------------------------------------------------ Запросы и согласования

#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
pub struct AccessRequestView {
    pub id: Uuid,
    pub ticket_number: i64,
    pub ticket_title: String,
    pub resource_id: Uuid,
    pub resource_name: String,
    pub resource_kind: String,
    /// Кому нужен доступ.
    pub person_id: Option<Uuid>,
    pub person_name: Option<String>,
    /// `new`, `pending`, `approved`, `rejected`, `done`.
    pub status: String,
    pub approver_id: Option<Uuid>,
    pub approver_name: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub sent_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub decided_at: Option<OffsetDateTime>,
    pub decided_by_name: Option<String>,
    pub decision_comment: Option<String>,
    pub operation_id: Option<Uuid>,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub granted_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: OffsetDateTime,
}

const REQ_VIEW: &str = "SELECT q.id, t.number AS ticket_number, t.title AS ticket_title, q.resource_id,
        r.name AS resource_name, r.kind AS resource_kind, q.person_id,
        nullif(trim(concat_ws(' ', pp.last_name, pp.first_name)), '') AS person_name, q.status, q.approver_id,
        nullif(trim(concat_ws(' ', ap.last_name, ap.first_name)), '') AS approver_name, q.sent_at, q.decided_at,
        (SELECT coalesce(nullif(trim(concat_ws(' ', dp.last_name, dp.first_name)), ''), da.display_name, da.sam_account_name)
           FROM users du JOIN ad_accounts da ON da.id = du.ad_account_id
           LEFT JOIN persons dp ON dp.id = du.person_id WHERE du.id = q.decided_by) AS decided_by_name,
        q.decision_comment, q.operation_id, q.granted_at, q.created_at
   FROM access_requests q
   JOIN tickets t ON t.id = q.ticket_id
   JOIN access_resources r ON r.id = q.resource_id
   LEFT JOIN persons pp ON pp.id = q.person_id
   LEFT JOIN persons ap ON ap.id = q.approver_id";

async fn request_view(ex: impl sqlx::PgExecutor<'_>, id: Uuid) -> ApiResult<AccessRequestView> {
    Ok(sqlx::query_as(&format!("{REQ_VIEW} WHERE q.id = $1")).bind(id).fetch_one(ex).await?) // SAFE: константа
}

/// Заявка, которую пользователь видит (без блокировки: операции каталога ссылаются на неё своими транзакциями).
async fn visible(core: &Core, who: &Who, number: i64) -> ApiResult<T> {
    let mut tx = core.db.begin().await?;
    let t = tickets::lock(&mut tx, number, who).await?;
    tx.rollback().await?;
    Ok(t)
}

/// Запрос доступа заявки (`null` — заявка не про доступ).
#[utoipa::path(get, path = "/api/helpdesk/tickets/{number}/access", tag = "helpdesk", params(("number" = i64, Path)),
    responses((status = 200, body = Option<AccessRequestView>)))]
async fn ticket_access(
    State(core): State<Core>,
    user: CurrentUser,
    Path(number): Path<i64>,
) -> ApiResult<Json<Option<AccessRequestView>>> {
    let t = visible(&core, &Who::from_user(&user), number).await?;
    let id: Option<Uuid> = sqlx::query_scalar("SELECT id FROM access_requests WHERE ticket_id = $1")
        .bind(t.id)
        .fetch_optional(&core.db)
        .await?;
    Ok(Json(match id {
        Some(id) => Some(request_view(&core.db, id).await?),
        None => None,
    }))
}

/// Владельцу: колокольчик и письмо «нужно согласование».
async fn ask_owner(
    tx: &mut Transaction<'_, Postgres>,
    t: &T,
    owner: Uuid,
    resource: &str,
    c: &notify::Ctx,
) -> Result<(), sqlx::Error> {
    let person: Option<String> =
        sqlx::query_scalar("SELECT nullif(trim(concat_ws(' ', last_name, first_name)), '') FROM persons WHERE id = $1")
            .bind(t.requester_id)
            .fetch_optional(&mut **tx)
            .await?
            .flatten();
    let users: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM users WHERE person_id = $1 AND is_active")
        .bind(owner)
        .fetch_all(&mut **tx)
        .await?;
    let person = person.unwrap_or_default();
    for u in users {
        notify::bell(tx, u, "helpdesk.approval", t).await?;
        notify::to_user(tx, u, "helpdesk.approval", t, &Msg::ApprovalRequested { resource, person: &person }, c)
            .await?;
    }
    Ok(())
}

/// Исполнитель отправляет запрос владельцу ресурса.
#[utoipa::path(post, path = "/api/helpdesk/tickets/{number}/access/send", tag = "helpdesk", params(("number" = i64, Path)),
    responses((status = 200, body = AccessRequestView)))]
async fn send_for_approval(
    State(core): State<Core>,
    user: CurrentUser,
    Path(number): Path<i64>,
) -> ApiResult<Json<AccessRequestView>> {
    let who = Who::from_user(&user);
    if !who.engineer {
        return Err(ApiError::Forbidden);
    }
    let c = notify::ctx(&core).await?;
    // Заметки в заявке хранятся на языке установки: их читают все, у кого бы какой интерфейс ни был.
    let lang = l10n::default_lang(&core.db).await;
    let mut tx = core.db.begin().await?;
    let t = tickets::lock(&mut tx, number, &who).await?;
    let row: Option<(Uuid, String, Uuid, String, bool)> = sqlx::query_as(
        "SELECT q.id, q.status, r.owner_id, r.name, r.is_active FROM access_requests q
           JOIN access_resources r ON r.id = q.resource_id WHERE q.ticket_id = $1 FOR UPDATE OF q",
    )
    .bind(t.id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((id, status, owner, resource, active)) = row else {
        return Err(ApiError::bad_request("not_access", "Это не запрос доступа."));
    };
    still_valid(&t, active)?;
    if !matches!(status.as_str(), "new" | "rejected") {
        return Err(ApiError::bad_request("invalid_state", "Запрос уже отправлен на согласование."));
    }
    sqlx::query(
        "UPDATE access_requests SET status = 'pending', approver_id = $2, sent_by = $3, sent_at = now(),
                decided_by = NULL, decided_at = NULL, decision_comment = NULL WHERE id = $1",
    )
    .bind(id)
    .bind(owner)
    .bind(user.user_id)
    .execute(&mut *tx)
    .await?;
    tickets::event(&mut tx, t.id, who.user, "access.sent", Value::Null, json!({"owner": owner})).await?;
    let owner_name: Option<String> =
        sqlx::query_scalar("SELECT nullif(trim(concat_ws(' ', last_name, first_name)), '') FROM persons WHERE id = $1")
            .bind(owner)
            .fetch_one(&mut *tx)
            .await?;
    let body = tr_to(
        lang,
        &format!(
            "Запрос отправлен на согласование владельцу ресурса «{resource}»: {}.",
            owner_name.unwrap_or_default()
        ),
    )
    .into_owned();
    let note = NewComment { body, internal: true, ..Default::default() };
    tickets::insert_comment(&mut tx, t.id, &who, &note).await?;
    ask_owner(&mut tx, &t, owner, &resource, &c).await?;
    tickets::audit_rec(&mut tx, &who, "access.send", &t, json!({"request": id, "owner": owner})).await?;
    tickets::ping(&mut tx, &t).await?;
    tx.commit().await?;
    Ok(Json(request_view(&core.db, id).await?))
}

/// Мои согласования: ожидающие и решённые мной.
#[utoipa::path(get, path = "/api/helpdesk/approvals", tag = "helpdesk", responses((status = 200, body = Vec<AccessRequestView>)))]
async fn list_approvals(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<Vec<AccessRequestView>>> {
    let Some(person) = user.person_id else { return Ok(Json(Vec::new())) };
    Ok(Json(
        sqlx::query_as(&format!(
            "{REQ_VIEW} WHERE (q.approver_id = $1 AND q.status = 'pending' AND t.status = ANY($3)) OR q.decided_by = $2 \
             ORDER BY (q.status = 'pending') DESC, q.created_at DESC LIMIT 200" // SAFE: константа
        ))
        .bind(person)
        .bind(user.user_id)
        .bind(tickets::OPEN_STATUSES)
        .fetch_all(&core.db)
        .await?,
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct Decision {
    pub approve: bool,
    pub comment: Option<String>,
}

/// Решение владельца ресурса. Принимается один раз.
#[utoipa::path(post, path = "/api/helpdesk/approvals/{id}", tag = "helpdesk", params(("id" = Uuid, Path)),
    request_body = Decision, responses((status = 200, body = AccessRequestView), (status = 409, body = pult_core::error::ErrorBody)))]
async fn decide(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<Decision>,
) -> ApiResult<Json<AccessRequestView>> {
    let who = Who::from_user(&user);
    let comment = b.comment.map(|c| c.trim().chars().take(2000).collect::<String>()).filter(|c| !c.is_empty());
    let c = notify::ctx(&core).await?;
    let lang = l10n::default_lang(&core.db).await;
    let mut tx = core.db.begin().await?;
    let row: Option<(Uuid, String, Option<Uuid>, String, bool)> = sqlx::query_as(
        "SELECT q.ticket_id, q.status, q.approver_id, r.name, r.is_active FROM access_requests q
           JOIN access_resources r ON r.id = q.resource_id WHERE q.id = $1 FOR UPDATE OF q",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((ticket, status, approver, resource, active)) = row else { return Err(ApiError::NotFound) };
    if approver.is_none() || approver != user.person_id {
        return Err(ApiError::Forbidden);
    }
    if status != "pending" {
        return Err(ApiError::Conflict { current: json!(request_view(&mut *tx, id).await?) });
    }
    let t: T = sqlx::query_as(&tickets::select_t("WHERE id = $1")).bind(ticket).fetch_one(&mut *tx).await?;
    still_valid(&t, active)?;
    let new = if b.approve { "approved" } else { "rejected" };
    sqlx::query(
        "UPDATE access_requests SET status = $2, decided_by = $3, decided_at = now(), decision_comment = $4 WHERE id = $1",
    )
    .bind(id)
    .bind(new)
    .bind(user.user_id)
    .bind(&comment)
    .execute(&mut *tx)
    .await?;
    let head = if b.approve {
        format!("Владелец ресурса «{resource}» согласовал доступ.")
    } else {
        format!("Владелец ресурса «{resource}» отказал в доступе.")
    };
    let head = tr_to(lang, &head).into_owned();
    let body = match &comment {
        Some(c) => format!("{head}\n\n{c}"),
        None => head,
    };
    tickets::insert_comment(&mut tx, t.id, &who, &NewComment { body, internal: false, ..Default::default() }).await?;
    tickets::event(&mut tx, t.id, who.user, "access.decided", json!(status), json!(new)).await?;
    if let Some(u) = t.assignee_id {
        notify::bell(&mut tx, u, "helpdesk.approval_decided", &t).await?;
        let m = Msg::ApprovalDecided { approved: b.approve, comment: comment.as_deref() };
        notify::to_user(&mut tx, u, "helpdesk.approval_decided", &t, &m, &c).await?;
    }
    tickets::audit_rec(&mut tx, &who, "access.decide", &t, json!({"request": id, "decision": new})).await?;
    tickets::ping(&mut tx, &t).await?;
    tx.commit().await?;
    Ok(Json(request_view(&core.db, id).await?))
}

/// «Мои доступы»: что запрошено, в каком состоянии, что выдано.
#[utoipa::path(get, path = "/api/helpdesk/access/mine", tag = "helpdesk", responses((status = 200, body = Vec<AccessRequestView>)))]
async fn my_access(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<Vec<AccessRequestView>>> {
    let Some(person) = user.person_id else { return Ok(Json(Vec::new())) };
    Ok(Json(
        sqlx::query_as(&format!("{REQ_VIEW} WHERE q.person_id = $1 ORDER BY q.created_at DESC LIMIT 200")) // SAFE: константа
            .bind(person)
            .fetch_all(&core.db)
            .await?,
    ))
}

#[derive(sqlx::FromRow)]
struct ExecRow {
    id: Uuid,
    status: String,
    person_id: Option<Uuid>,
    kind: String,
    name: String,
    group_dn: Option<String>,
    is_active: bool,
    operation_id: Option<Uuid>,
    operation_status: Option<String>,
}

/// Запрос ещё в силе: заявка открыта, ресурс не выключен.
fn still_valid(t: &T, resource_active: bool) -> ApiResult<()> {
    if !tickets::is_open(&t.status) {
        return Err(ApiError::bad_request("ticket_closed", "Заявка закрыта: по ней доступ не выдаётся."));
    }
    if !resource_active {
        return Err(ApiError::bad_request("resource_inactive", "Ресурс выключен: доступ к нему не выдаётся."));
    }
    Ok(())
}

/// «Выполнить»: согласованный доступ к группе AD — операция каталога. Повтор — та же операция.
#[utoipa::path(post, path = "/api/helpdesk/tickets/{number}/access/execute", tag = "helpdesk", params(("number" = i64, Path)),
    responses((status = 202, body = OperationView)))]
async fn execute_access(
    State(core): State<Core>,
    user: CurrentUser,
    Path(number): Path<i64>,
) -> ApiResult<(StatusCode, Json<OperationView>)> {
    let who = Who::from_user(&user);
    if !who.engineer {
        return Err(ApiError::Forbidden);
    }
    let t = visible(&core, &who, number).await?;
    let row: Option<ExecRow> = sqlx::query_as(
        "SELECT q.id, q.status, q.person_id, r.kind, r.name, r.group_dn, r.is_active, q.operation_id,
                (SELECT o.status FROM directory_operations o WHERE o.id = q.operation_id) AS operation_status
           FROM access_requests q JOIN access_resources r ON r.id = q.resource_id WHERE q.ticket_id = $1",
    )
    .bind(t.id)
    .fetch_optional(&core.db)
    .await?;
    let Some(ExecRow {
        id,
        status,
        person_id: person,
        kind,
        name: resource,
        group_dn: group,
        is_active,
        operation_id,
        operation_status,
    }) = row
    else {
        return Err(ApiError::bad_request("not_access", "Это не запрос доступа."));
    };
    // Операция уже есть и не завершилась ошибкой — та же, кто бы из инженеров ни нажал.
    if let (Some(op), Some(st)) = (operation_id, operation_status.as_deref())
        && st != "failed"
    {
        return Ok((StatusCode::ACCEPTED, Json(ops::view(&core.db, op).await?.localized())));
    }
    if !matches!(status.as_str(), "approved" | "done") {
        return Err(ApiError::bad_request("not_approved", "Сначала нужно согласование владельца ресурса."));
    }
    still_valid(&t, is_active)?;
    let (Some(group), "ad_group") = (group, kind.as_str()) else {
        return Err(ApiError::bad_request(
            "manual_resource",
            "Этот ресурс выдаётся вручную: выполните и закройте заявку.",
        ));
    };
    let Some(person) = person else {
        return Err(ApiError::bad_request("no_account", "У сотрудника нет учётки AD."));
    };
    let op = ops::request(
        &core,
        &user,
        NewOperation {
            kind: Kind::AddMember,
            target: Target::Person(person),
            group_dn: Some(group),
            basis: format!("Заявка #{number}: доступ к «{resource}», согласован владельцем ресурса."),
            ticket_id: Some(t.id),
            checklist_item_id: None,
            // После неудачной операции — новый ключ: выполнение можно повторить.
            idempotency_key: match operation_id {
                Some(failed) => format!("access:{id}:{failed}"),
                None => format!("access:{id}"),
            },
            own_allowed: true,
        },
    )
    .await?;
    sqlx::query("UPDATE access_requests SET operation_id = $2 WHERE id = $1 AND operation_id IS NOT DISTINCT FROM $3")
        .bind(id)
        .bind(op.id)
        .bind(operation_id)
        .execute(&core.db)
        .await?;
    Ok((StatusCode::ACCEPTED, Json(op.localized())))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ChecklistRun {
    /// Логин учётки AD: нового сотрудника (его ещё нет в справочнике), уволенного и т. п.
    pub login: String,
    /// Для `directory.add_member`.
    pub group_dn: Option<String>,
}

/// Пункт чек-листа с действием каталога: «Включить учётку», «Отключить учётку», «Добавить в группу».
#[utoipa::path(post, path = "/api/helpdesk/tickets/{number}/checklist/{item}/run", tag = "helpdesk",
    params(("number" = i64, Path), ("item" = Uuid, Path)), request_body = ChecklistRun,
    responses((status = 202, body = OperationView)))]
async fn run_checklist_item(
    State(core): State<Core>,
    user: CurrentUser,
    Path((number, item)): Path<(i64, Uuid)>,
    Json(b): Json<ChecklistRun>,
) -> ApiResult<(StatusCode, Json<OperationView>)> {
    let who = Who::from_user(&user);
    if !who.engineer {
        return Err(ApiError::Forbidden);
    }
    let t = visible(&core, &who, number).await?;
    let row: Option<(String, Option<String>)> =
        sqlx::query_as("SELECT title, action FROM ticket_checklist_items WHERE id = $1 AND ticket_id = $2")
            .bind(item)
            .bind(t.id)
            .fetch_optional(&core.db)
            .await?;
    let Some((title, Some(action))) = row else {
        return Err(ApiError::bad_request("invalid_item", "У пункта нет действия каталога."));
    };
    let kind = match action.as_str() {
        "directory.enable" => Kind::Enable,
        "directory.disable" => Kind::Disable,
        "directory.add_member" => Kind::AddMember,
        _ => return Err(ApiError::bad_request("invalid_item", "Неизвестное действие пункта.")),
    };
    debug_assert!(checklist::ACTIONS.contains(&action.as_str()));
    let account: Uuid = sqlx::query_scalar(
        "SELECT id FROM ad_accounts WHERE lower(sam_account_name) = lower($1) AND NOT is_missing ORDER BY synced_at DESC LIMIT 1",
    )
    .bind(b.login.trim())
    .fetch_optional(&core.db)
    .await?
    .ok_or_else(|| ApiError::bad_request("no_account", "Учётка с таким логином не найдена."))?;
    let group = b.group_dn.map(|g| g.trim().to_string()).filter(|g| !g.is_empty());
    // Ключ — пункт, учётка и группа: повторное нажатие не создаёт вторую операцию.
    let seed = format!("checklist:{item}:{account}:{}", group.as_deref().unwrap_or("-").to_lowercase());
    let key: String = pult_core::crypto::sha256(seed.as_bytes()).iter().take(20).map(|b| format!("{b:02x}")).collect();
    let op = ops::request(
        &core,
        &user,
        NewOperation {
            kind,
            target: Target::Account(account),
            group_dn: group,
            basis: format!("Заявка #{number}, пункт чек-листа «{title}»."),
            ticket_id: Some(t.id),
            checklist_item_id: Some(item),
            idempotency_key: format!("checklist:{key}"),
            own_allowed: false,
        },
    )
    .await?;
    Ok((StatusCode::ACCEPTED, Json(op.localized())))
}

// ------------------------------------------------------------------ Итог операции в заявке

/// Фоновая задача `directory.operation`: выполнить операцию каталога и отразить итог в заявке —
/// комментарий в ленте, запрос доступа «выдан», пункт чек-листа выполнен. Итог пишется ровно один раз
/// (`reported_at`): если задача упала после записи в AD, повтор допишет итог, но не продублирует его.
pub async fn run_operation(core: &Core, op: Uuid) -> ApiResult<Value> {
    let f = ops::execute(core, op).await?;
    follow_up(core, &f.view).await?;
    serde_json::to_value(&f.view).map_err(ApiError::internal)
}

fn outcome(v: &OperationView) -> String {
    let login = &v.login;
    if v.status != "done" {
        let kind = match v.kind.as_str() {
            "group.add_member" => "добавление в группу",
            "user.unlock" => "разблокировка",
            "user.enable" => "включение учётки",
            "user.disable" => "отключение учётки",
            _ => "сброс пароля",
        };
        let error = v.error.as_deref().unwrap_or("");
        return format!("Операция каталога AD «{kind}» для {login} не выполнена: {error}");
    }
    let group = v.group_dn.as_deref().unwrap_or("");
    let changed = v.changed.unwrap_or(true);
    let text = match (v.kind.as_str(), changed) {
        ("group.add_member", true) => format!("Учётка {login} добавлена в группу {group}."),
        ("group.add_member", false) => format!("Учётка {login} уже была в группе {group}, AD не меняли."),
        ("user.unlock", true) => format!("Учётка {login} разблокирована."),
        ("user.unlock", false) => format!("Учётка {login} не была заблокирована, AD не меняли."),
        ("user.enable", true) => format!("Учётка {login} включена."),
        ("user.enable", false) => format!("Учётка {login} уже была включена, AD не меняли."),
        ("user.disable", true) => format!("Учётка {login} отключена."),
        ("user.disable", false) => format!("Учётка {login} уже была отключена, AD не меняли."),
        _ => format!("Пароль учётки {login} сброшен, при входе нужно сменить. Временный пароль — у запросившего."),
    };
    format!("{text} Проверено повторным чтением из AD.")
}

async fn follow_up(core: &Core, v: &OperationView) -> ApiResult<()> {
    if !matches!(v.status.as_str(), "done" | "failed") {
        return Ok(());
    }
    let lang = l10n::default_lang(&core.db).await;
    let mut tx = core.db.begin().await?;
    let row: Option<(Option<Uuid>, Option<Uuid>, Uuid)> = sqlx::query_as(
        "UPDATE directory_operations SET reported_at = now() WHERE id = $1 AND reported_at IS NULL
         RETURNING ticket_id, checklist_item_id, requested_by",
    )
    .bind(v.id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((ticket, item, by)) = row else {
        // Итог уже в заявке.
        return Ok(());
    };
    if let Some(ticket) = ticket {
        let t: T = sqlx::query_as(&tickets::select_t("WHERE id = $1")).bind(ticket).fetch_one(&mut *tx).await?;
        let system = Who::system();
        let note = NewComment {
            body: tr_to(lang, &outcome(v)).into_owned(),
            internal: true,
            author_name: Some(tr_to(lang, "Каталог AD").into_owned()),
            source: "system".into(),
            ..Default::default()
        };
        tickets::insert_comment(&mut tx, t.id, &system, &note).await?;
        tickets::event(
            &mut tx,
            t.id,
            Some(by),
            "directory.operation",
            Value::Null,
            json!({"kind": v.kind, "status": v.status}),
        )
        .await?;
        tickets::ping(&mut tx, &t).await?;
    }
    if v.status == "done" {
        sqlx::query("UPDATE access_requests SET status = 'done', granted_at = now() WHERE operation_id = $1 AND status = 'approved'")
            .bind(v.id)
            .execute(&mut *tx)
            .await?;
        if let Some(item) = item {
            sqlx::query(
                "UPDATE ticket_checklist_items SET done_at = now(), done_by = $2 WHERE id = $1 AND done_at IS NULL",
            )
            .bind(item)
            .bind(by)
            .execute(&mut *tx)
            .await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(kind: &str, status: &str, changed: bool) -> OperationView {
        OperationView {
            id: Uuid::nil(),
            kind: kind.into(),
            status: status.into(),
            person_id: None,
            login: "ivanov".into(),
            target_dn: String::new(),
            group_dn: Some("CN=Buh,DC=x".into()),
            ticket_number: None,
            basis: String::new(),
            requested_by: Uuid::nil(),
            requested_by_name: None,
            before: None,
            after: None,
            changed: Some(changed),
            verified: Some(status == "done"),
            error: (status != "done").then(|| "нет прав".into()),
            has_password: false,
            created_at: OffsetDateTime::UNIX_EPOCH,
            finished_at: None,
        }
    }

    #[test]
    fn outcome_texts() {
        assert!(outcome(&op("group.add_member", "done", true)).contains("добавлена в группу CN=Buh,DC=x"));
        assert!(outcome(&op("group.add_member", "done", false)).contains("уже была в группе"));
        assert!(outcome(&op("user.unlock", "done", true)).contains("разблокирована"));
        assert!(outcome(&op("user.unlock", "failed", false)).contains("не выполнена: нет прав"));
    }

    #[test]
    fn resource_validation() {
        let base = || ResourceInput {
            kind: "ad_group".into(),
            name: " Бухгалтерия ".into(),
            description: String::new(),
            group_dn: Some("CN=Buh,OU=Groups,DC=x".into()),
            owner_id: Uuid::nil(),
            is_active: true,
        };
        assert_eq!(validated(base()).unwrap().name, "Бухгалтерия");
        assert!(validated(ResourceInput { group_dn: None, ..base() }).is_err(), "группе нужен DN");
        assert!(validated(ResourceInput { group_dn: Some("Бухгалтерия".into()), ..base() }).is_err());
        assert!(validated(ResourceInput { kind: "share".into(), group_dn: None, ..base() }).is_ok());
        assert!(validated(ResourceInput { kind: "printer".into(), ..base() }).is_err());
    }
}

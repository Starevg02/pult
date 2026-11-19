//! Связанные объекты заявки (как «элементы» GLPI): компьютер домена, сотрудник, учётка AD,
//! оборудование из каталога. Каталог: типы задаёт администратор, у объекта — площадка,
//! кабинет, инвентарный номер, ответственный. В карточке объекта — история заявок.
//! Здесь же связи заявок («связана», «продолжение», «дубль») и объединение дублей.

use crate::{
    HelpdeskState, perm,
    tickets::{
        self, NewComment, T, Who, audit_rec, bad_state, event, insert_comment, lock, need_engineer, ping, set_status,
    },
};
use axum::{
    Json,
    extract::{FromRef, Path, Query, State},
    http::StatusCode,
};
use pult_core::l10n::tr;
use pult_core::{
    ApiError, ApiResult, Core, CurrentUser, Permission,
    audit::{self, Entry, Source},
    rbac::{ROLE_ENGINEER, ROLE_SUPERADMIN},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Postgres, Transaction};
use time::OffsetDateTime;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub const MANAGE_ASSETS: Permission = Permission {
    code: "helpdesk.assets.manage",
    description: "Вести каталог оборудования: добавлять, менять, списывать",
    default_roles: &[ROLE_ENGINEER, ROLE_SUPERADMIN],
};

pub const KINDS: &[&str] = &["computer", "person", "ad_account", "asset"];

#[derive(Debug, Clone, Serialize, sqlx::FromRow, ToSchema)]
pub struct LinkedObject {
    pub kind: String,
    pub object_id: Uuid,
    pub label: String,
    /// Подробности: тип оборудования, инвентарный номер, ОС…
    pub detail: Option<String>,
}

/// Название и подробности объекта по его виду. `None` — объекта нет.
async fn describe(
    tx: &mut Transaction<'_, Postgres>,
    kind: &str,
    id: Uuid,
) -> Result<Option<(String, Option<String>)>, sqlx::Error> {
    let sql = match kind {
        "computer" => "SELECT name, concat_ws(' · ', operating_system, description) FROM ad_computers WHERE id = $1",
        "person" => {
            "SELECT trim(concat_ws(' ', last_name, first_name, middle_name)), (SELECT name FROM departments d WHERE d.id = department_id)
               FROM persons WHERE id = $1"
        }
        "ad_account" => "SELECT sam_account_name, display_name FROM ad_accounts WHERE id = $1",
        "asset" => {
            "SELECT a.name, concat_ws(' · ', t.name, nullif(a.inventory_number, ''), nullif(a.serial_number, ''))
               FROM assets a JOIN asset_types t ON t.id = a.type_id WHERE a.id = $1"
        }
        _ => return Ok(None),
    };
    sqlx::query_as(sql).bind(id).fetch_optional(&mut **tx).await
}

pub async fn of_ticket(db: &sqlx::PgPool, ticket: Uuid) -> Result<Vec<LinkedObject>, sqlx::Error> {
    sqlx::query_as("SELECT kind, object_id, label, detail FROM ticket_objects WHERE ticket_id = $1 ORDER BY created_at")
        .bind(ticket)
        .fetch_all(db)
        .await
}

async fn link_object(tx: &mut Transaction<'_, Postgres>, t: &T, who: &Who, kind: &str, id: Uuid) -> ApiResult<bool> {
    if !KINDS.contains(&kind) {
        return Err(ApiError::bad_request("invalid_object", "Неизвестный вид объекта."));
    }
    let (label, detail) =
        describe(tx, kind, id).await?.ok_or_else(|| ApiError::bad_request("invalid_object", "Объект не найден."))?;
    let n = sqlx::query(
        "INSERT INTO ticket_objects (ticket_id, kind, object_id, label, detail, added_by) VALUES ($1, $2, $3, $4, nullif($5, ''), $6)
         ON CONFLICT (ticket_id, kind, object_id) DO NOTHING",
    )
    .bind(t.id)
    .bind(kind)
    .bind(id)
    .bind(&label)
    .bind(detail.unwrap_or_default())
    .bind(who.user)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    if n > 0 {
        event(tx, t.id, who.user, "object_added", Value::Null, json!({"kind": kind, "label": label})).await?;
    }
    Ok(n > 0)
}

/// Имя ПК из формы — привязать компьютер домена с таким именем.
pub async fn attach_computer_by_name(tx: &mut Transaction<'_, Postgres>, t: &T, who: &Who) -> ApiResult<()> {
    let id: Option<Uuid> = sqlx::query_scalar(
        "SELECT c.id FROM tickets t JOIN ad_computers c ON lower(c.name) = lower(t.computer_name) WHERE t.id = $1 LIMIT 1",
    )
    .bind(t.id)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(id) = id {
        link_object(tx, t, who, "computer", id).await?;
    }
    Ok(())
}

/// Сколько единиц оборудования можно выбрать при создании заявки.
pub const MAX_TICKET_ASSETS: usize = 10;

/// Действующее оборудование, за которое отвечает сотрудник: выбор в форме заявки.
pub async fn assets_of(db: &sqlx::PgPool, person: Uuid) -> Result<Vec<FoundObject>, sqlx::Error> {
    sqlx::query_as(
        "SELECT 'asset' AS kind, a.id, a.name AS label,
                concat_ws(' · ', t.name, nullif(a.inventory_number, ''), nullif(a.serial_number, '')) AS detail
           FROM assets a JOIN asset_types t ON t.id = a.type_id
          WHERE a.responsible_person_id = $1 AND a.status <> 'retired'
          ORDER BY t.name, a.name LIMIT 50",
    )
    .bind(person)
    .fetch_all(db)
    .await
}

/// Оборудование, выбранное в форме новой заявки. Инженер выбирает любое действующее из реестра,
/// остальные — только то, за которое отвечает заявитель. Чужое и списанное — ошибка без
/// подробностей (не подсказывает, что такое оборудование есть).
pub(crate) async fn attach_assets(
    tx: &mut Transaction<'_, Postgres>,
    t: &T,
    who: &Who,
    ids: &[Uuid],
    requester: Option<Uuid>,
) -> ApiResult<()> {
    for &id in ids {
        let allowed: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM assets WHERE id = $1 AND status <> 'retired'
                              AND ($2 OR responsible_person_id = $3))",
        )
        .bind(id)
        .bind(who.engineer)
        .bind(requester)
        .fetch_one(&mut **tx)
        .await?;
        if !allowed {
            return Err(ApiError::bad_request("invalid_ticket", "Оборудование не найдено."));
        }
        if link_object(tx, t, who, "asset", id).await? {
            audit_rec(tx, who, "ticket.object.add", t, json!({"kind": "asset", "object": id})).await?;
        }
    }
    Ok(())
}

pub fn routes<S>() -> OpenApiRouter<S>
where
    S: Clone + Send + Sync + 'static,
    Core: FromRef<S>,
    HelpdeskState: FromRef<S>,
{
    OpenApiRouter::new()
        .routes(routes!(search_objects))
        .routes(routes!(person_assets))
        .routes(routes!(add_object))
        .routes(routes!(remove_object))
        .routes(routes!(object_card))
        .routes(routes!(list_asset_types, create_asset_type))
        .routes(routes!(update_asset_type))
        .routes(routes!(list_assets, create_asset))
        .routes(routes!(get_asset, update_asset))
        .routes(routes!(add_link))
        .routes(routes!(remove_link))
        .routes(routes!(merge_ticket))
}

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct FoundObject {
    pub kind: String,
    pub id: Uuid,
    pub label: String,
    pub detail: Option<String>,
}

#[derive(Deserialize, IntoParams)]
pub struct ObjectQuery {
    pub q: String,
    /// Один вид объектов; пусто — все.
    pub kind: Option<String>,
}

/// Поиск объектов для привязки: компьютеры, сотрудники, учётки, оборудование.
#[utoipa::path(get, path = "/api/helpdesk/objects", tag = "helpdesk", params(ObjectQuery), responses((status = 200, body = Vec<FoundObject>)))]
async fn search_objects(
    State(core): State<Core>,
    user: CurrentUser,
    Query(q): Query<ObjectQuery>,
) -> ApiResult<Json<Vec<FoundObject>>> {
    user.require(&perm::WORK)?;
    let needle: String = q.q.trim().chars().take(100).collect();
    if needle.chars().count() < 2 {
        return Ok(Json(Vec::new()));
    }
    let rows = sqlx::query_as(
        "SELECT * FROM (
            (SELECT 'computer' AS kind, id, name AS label, concat_ws(' · ', operating_system, description) AS detail
               FROM ad_computers WHERE is_enabled AND (strpos(lower(name), lower($1)) > 0 OR strpos(lower(coalesce(description, '')), lower($1)) > 0)
              ORDER BY starts_with(lower(name), lower($1)) DESC, name LIMIT 8)
            UNION ALL
            (SELECT 'person', p.id, trim(concat_ws(' ', p.last_name, p.first_name, p.middle_name)), d.name
               FROM persons p LEFT JOIN departments d ON d.id = p.department_id
              WHERE NOT p.is_archived AND p.search_text LIKE '%' || pult_norm($1) || '%'
              ORDER BY p.last_name, p.first_name LIMIT 8)
            UNION ALL
            (SELECT 'ad_account', id, sam_account_name, display_name FROM ad_accounts
              WHERE NOT is_missing AND (strpos(lower(sam_account_name), lower($1)) > 0 OR strpos(lower(coalesce(display_name, '')), lower($1)) > 0)
              ORDER BY sam_account_name LIMIT 8)
            UNION ALL
            (SELECT 'asset', a.id, a.name, concat_ws(' · ', t.name, nullif(a.inventory_number, ''), nullif(a.serial_number, ''))
               FROM assets a JOIN asset_types t ON t.id = a.type_id
              WHERE a.status <> 'retired' AND (strpos(lower(a.name), lower($1)) > 0 OR lower(a.inventory_number) = lower($1)
                    OR lower(a.serial_number) = lower($1) OR strpos(lower(t.name), lower($1)) > 0)
              ORDER BY a.name LIMIT 8)
         ) x WHERE $2::text IS NULL OR x.kind = $2",
    )
    .bind(&needle)
    .bind(&q.kind)
    .fetch_all(&core.db)
    .await?;
    Ok(Json(rows))
}

/// Оборудование сотрудника — подсказка инженеру в заявке за сотрудника и в карточке заявки.
#[utoipa::path(get, path = "/api/helpdesk/persons/{id}/assets", tag = "helpdesk", params(("id" = Uuid, Path)),
    responses((status = 200, body = Vec<FoundObject>)))]
async fn person_assets(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Vec<FoundObject>>> {
    user.require(&perm::WORK).or_else(|_| user.require(&perm::ON_BEHALF))?;
    Ok(Json(assets_of(&core.db, id).await?))
}

#[derive(Deserialize, ToSchema)]
pub struct ObjectInput {
    /// `computer`, `person`, `ad_account`, `asset`.
    pub kind: String,
    pub object_id: Uuid,
}

/// Привязать объект к заявке.
#[utoipa::path(post, path = "/api/helpdesk/tickets/{number}/objects", tag = "helpdesk", params(("number" = i64, Path)), request_body = ObjectInput, responses((status = 204)))]
async fn add_object(
    State(core): State<Core>,
    user: CurrentUser,
    Path(number): Path<i64>,
    Json(b): Json<ObjectInput>,
) -> ApiResult<StatusCode> {
    let who = Who::from_user(&user);
    need_engineer(&who)?;
    let mut tx = core.db.begin().await?;
    let t = lock(&mut tx, number, &who).await?;
    if link_object(&mut tx, &t, &who, &b.kind, b.object_id).await? {
        audit_rec(&mut tx, &who, "ticket.object.add", &t, json!({"kind": b.kind, "object": b.object_id})).await?;
    }
    ping(&mut tx, &t).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(delete, path = "/api/helpdesk/tickets/{number}/objects/{kind}/{id}", tag = "helpdesk",
    params(("number" = i64, Path), ("kind" = String, Path), ("id" = Uuid, Path)), responses((status = 204)))]
async fn remove_object(
    State(core): State<Core>,
    user: CurrentUser,
    Path((number, kind, id)): Path<(i64, String, Uuid)>,
) -> ApiResult<StatusCode> {
    let who = Who::from_user(&user);
    need_engineer(&who)?;
    let mut tx = core.db.begin().await?;
    let t = lock(&mut tx, number, &who).await?;
    let label: Option<String> = sqlx::query_scalar(
        "DELETE FROM ticket_objects WHERE ticket_id = $1 AND kind = $2 AND object_id = $3 RETURNING label",
    )
    .bind(t.id)
    .bind(&kind)
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    let label = label.ok_or(ApiError::NotFound)?;
    event(&mut tx, t.id, who.user, "object_removed", json!({"kind": kind, "label": label}), Value::Null).await?;
    audit_rec(&mut tx, &who, "ticket.object.remove", &t, json!({"kind": kind, "object": id})).await?;
    ping(&mut tx, &t).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct ObjectTicket {
    pub number: i64,
    pub title: String,
    pub status: String,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: OffsetDateTime,
}

#[derive(Serialize, ToSchema)]
pub struct ObjectCard {
    pub kind: String,
    pub id: Uuid,
    pub label: String,
    pub detail: Option<String>,
    /// Заявки с этим объектом, новые сверху. Для сотрудника — и его обращения.
    pub tickets: Vec<ObjectTicket>,
    pub asset: Option<Asset>,
}

/// Карточка объекта с историей заявок.
#[utoipa::path(get, path = "/api/helpdesk/objects/{kind}/{id}", tag = "helpdesk",
    params(("kind" = String, Path), ("id" = Uuid, Path)), responses((status = 200, body = ObjectCard)))]
async fn object_card(
    State(core): State<Core>,
    user: CurrentUser,
    Path((kind, id)): Path<(String, Uuid)>,
) -> ApiResult<Json<ObjectCard>> {
    user.require(&perm::WORK)?;
    let mut tx = core.db.begin().await?;
    let (label, detail) = describe(&mut tx, &kind, id).await?.ok_or(ApiError::NotFound)?;
    tx.rollback().await?;
    let tickets = sqlx::query_as(
        "SELECT t.number, t.title, t.status, t.created_at FROM tickets t
          WHERE EXISTS (SELECT 1 FROM ticket_objects o WHERE o.ticket_id = t.id AND o.kind = $1 AND o.object_id = $2)
             OR ($1 = 'person' AND t.requester_id = $2)
             OR ($1 = 'computer' AND lower(t.computer_name) = (SELECT lower(name) FROM ad_computers WHERE id = $2))
          ORDER BY t.created_at DESC LIMIT 200",
    )
    .bind(&kind)
    .bind(id)
    .fetch_all(&core.db)
    .await?;
    let asset = if kind == "asset" { fetch_asset(&core, id).await? } else { None };
    Ok(Json(ObjectCard { kind, id, label, detail, tickets, asset }))
}

// ------------------------------------------------------------------ Каталог оборудования

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct AssetType {
    pub id: Uuid,
    pub name: String,
    #[schema(value_type = Vec<tickets::FormField>)]
    pub fields: Value,
    pub is_active: bool,
    pub count: i64,
}

#[derive(Deserialize, ToSchema)]
pub struct AssetTypeInput {
    pub name: String,
    /// Дополнительные поля карточки (как поля формы категории).
    #[serde(default)]
    pub fields: Vec<tickets::FormField>,
    #[serde(default = "yes")]
    pub is_active: bool,
}

fn yes() -> bool {
    true
}

#[utoipa::path(get, path = "/api/helpdesk/asset-types", tag = "helpdesk", responses((status = 200, body = Vec<AssetType>)))]
async fn list_asset_types(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<Vec<AssetType>>> {
    user.require(&perm::WORK)?;
    let rows = sqlx::query_as(
        "SELECT t.id, t.name, t.fields, t.is_active, (SELECT count(*) FROM assets a WHERE a.type_id = t.id) AS count
           FROM asset_types t ORDER BY t.name",
    )
    .fetch_all(&core.db)
    .await?;
    Ok(Json(rows))
}

fn check_type(b: &AssetTypeInput) -> ApiResult<()> {
    if b.name.trim().is_empty() {
        return Err(ApiError::bad_request("invalid_asset", "Укажите название типа."));
    }
    let mut keys = std::collections::HashSet::new();
    if b.fields.iter().any(|f| {
        f.key.trim().is_empty() || !keys.insert(f.key.clone()) || !matches!(f.kind.as_str(), "text" | "select" | "bool")
    }) {
        return Err(ApiError::bad_request("invalid_asset", "У каждого поля — свой ключ и тип text, select или bool."));
    }
    Ok(())
}

#[utoipa::path(post, path = "/api/admin/helpdesk/asset-types", tag = "helpdesk-admin", request_body = AssetTypeInput, responses((status = 201, body = Object)))]
async fn create_asset_type(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<AssetTypeInput>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    user.require(&perm::ADMIN)?;
    check_type(&b)?;
    let id: Uuid =
        sqlx::query_scalar("INSERT INTO asset_types (name, fields, is_active) VALUES ($1, $2, $3) RETURNING id")
            .bind(b.name.trim())
            .bind(json!(b.fields))
            .bind(b.is_active)
            .fetch_one(&core.db)
            .await
            .map_err(dup)?;
    audit::record(&core.db, &user.actor, Entry::new("asset_type.create", Source::Portal).object("asset_type", id))
        .await?;
    Ok((StatusCode::CREATED, Json(json!({"id": id}))))
}

fn dup(e: sqlx::Error) -> ApiError {
    match &e {
        sqlx::Error::Database(d) if d.is_unique_violation() => {
            ApiError::bad_request("duplicate", "Такое название или инвентарный номер уже есть.")
        }
        _ => e.into(),
    }
}

#[utoipa::path(put, path = "/api/admin/helpdesk/asset-types/{id}", tag = "helpdesk-admin", params(("id" = Uuid, Path)), request_body = AssetTypeInput, responses((status = 204)))]
async fn update_asset_type(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<AssetTypeInput>,
) -> ApiResult<StatusCode> {
    user.require(&perm::ADMIN)?;
    check_type(&b)?;
    let n = sqlx::query("UPDATE asset_types SET name = $2, fields = $3, is_active = $4 WHERE id = $1")
        .bind(id)
        .bind(b.name.trim())
        .bind(json!(b.fields))
        .bind(b.is_active)
        .execute(&core.db)
        .await
        .map_err(dup)?
        .rows_affected();
    if n == 0 {
        return Err(ApiError::NotFound);
    }
    audit::record(&core.db, &user.actor, Entry::new("asset_type.update", Source::Portal).object("asset_type", id))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct Asset {
    pub id: Uuid,
    pub type_id: Uuid,
    pub type_name: String,
    pub name: String,
    pub inventory_number: Option<String>,
    pub serial_number: Option<String>,
    pub site_id: Option<Uuid>,
    pub site_name: Option<String>,
    pub room: Option<String>,
    pub responsible_person_id: Option<Uuid>,
    pub responsible_name: Option<String>,
    /// `in_use`, `in_stock`, `repair`, `retired`.
    pub status: String,
    pub notes: Option<String>,
    #[schema(value_type = Object)]
    pub attributes: Value,
    pub open_tickets: i64,
}

const ASSET_SELECT: &str = "SELECT a.id, a.type_id, t.name AS type_name, a.name, a.inventory_number, a.serial_number, a.site_id,
        s.name AS site_name, a.room, a.responsible_person_id,
        trim(concat_ws(' ', p.last_name, p.first_name)) AS responsible_name, a.status, a.notes, a.attributes,
        (SELECT count(*) FROM ticket_objects o JOIN tickets x ON x.id = o.ticket_id
          WHERE o.kind = 'asset' AND o.object_id = a.id AND x.status = ANY('{new,assigned,in_progress,waiting,waiting_approval,on_hold}')) AS open_tickets
   FROM assets a JOIN asset_types t ON t.id = a.type_id
   LEFT JOIN sites s ON s.id = a.site_id
   LEFT JOIN persons p ON p.id = a.responsible_person_id";

async fn fetch_asset(core: &Core, id: Uuid) -> ApiResult<Option<Asset>> {
    Ok(sqlx::query_as(&format!("{ASSET_SELECT} WHERE a.id = $1")) // SAFE: константа
        .bind(id)
        .fetch_optional(&core.db)
        .await?)
}

#[derive(Deserialize, IntoParams)]
pub struct AssetQuery {
    pub q: Option<String>,
    pub type_id: Option<Uuid>,
    pub site_id: Option<Uuid>,
    /// Со списанными.
    #[serde(default)]
    pub all: bool,
}

#[utoipa::path(get, path = "/api/helpdesk/assets", tag = "helpdesk", params(AssetQuery), responses((status = 200, body = Vec<Asset>)))]
async fn list_assets(
    State(core): State<Core>,
    user: CurrentUser,
    Query(q): Query<AssetQuery>,
) -> ApiResult<Json<Vec<Asset>>> {
    user.require(&perm::WORK)?;
    let needle =
        q.q.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(|s| s.chars().take(100).collect::<String>());
    let rows = sqlx::query_as(&format!(
        "{ASSET_SELECT}
          WHERE ($1::uuid IS NULL OR a.type_id = $1) AND ($2::uuid IS NULL OR a.site_id = $2) AND ($3 OR a.status <> 'retired')
            AND ($4::text IS NULL OR strpos(lower(concat_ws(' ', a.name, a.inventory_number, a.serial_number, a.room, t.name)), lower($4)) > 0)
          ORDER BY t.name, a.name LIMIT 1000" // SAFE: константа
    ))
    .bind(q.type_id)
    .bind(q.site_id)
    .bind(q.all)
    .bind(needle)
    .fetch_all(&core.db)
    .await?;
    Ok(Json(rows))
}

#[derive(Deserialize, ToSchema)]
pub struct AssetInput {
    pub type_id: Uuid,
    pub name: String,
    pub inventory_number: Option<String>,
    pub serial_number: Option<String>,
    pub site_id: Option<Uuid>,
    pub room: Option<String>,
    pub responsible_person_id: Option<Uuid>,
    pub status: String,
    pub notes: Option<String>,
    #[serde(default)]
    #[schema(value_type = Object)]
    pub attributes: Value,
}

async fn check_asset(core: &Core, b: &AssetInput) -> ApiResult<Value> {
    let bad = |m: String| ApiError::bad_request("invalid_asset", m);
    if b.name.trim().is_empty() {
        return Err(bad("Укажите название.".into()));
    }
    if !matches!(b.status.as_str(), "in_use" | "in_stock" | "repair" | "retired") {
        return Err(bad("Неверное состояние.".into()));
    }
    let fields: Option<Value> = sqlx::query_scalar("SELECT fields FROM asset_types WHERE id = $1")
        .bind(b.type_id)
        .fetch_optional(&core.db)
        .await?;
    let fields: Vec<tickets::FormField> =
        serde_json::from_value(fields.ok_or_else(|| bad("Тип не найден.".into()))?).unwrap_or_default();
    tickets::validate_answers(&fields, &b.attributes).map_err(bad)
}

fn clean(s: &Option<String>) -> Option<String> {
    s.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(|s| s.chars().take(200).collect())
}

#[utoipa::path(post, path = "/api/helpdesk/assets", tag = "helpdesk", request_body = AssetInput, responses((status = 201, body = Object)))]
async fn create_asset(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<AssetInput>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    user.require(&MANAGE_ASSETS)?;
    let attrs = check_asset(&core, &b).await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO assets (type_id, name, inventory_number, serial_number, site_id, room, responsible_person_id, status, notes, attributes)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING id",
    )
    .bind(b.type_id)
    .bind(b.name.trim())
    .bind(clean(&b.inventory_number))
    .bind(clean(&b.serial_number))
    .bind(b.site_id)
    .bind(clean(&b.room))
    .bind(b.responsible_person_id)
    .bind(&b.status)
    .bind(clean(&b.notes))
    .bind(attrs)
    .fetch_one(&core.db)
    .await
    .map_err(dup)?;
    audit::record(&core.db, &user.actor, Entry::new("asset.create", Source::Portal).object("asset", id)).await?;
    Ok((StatusCode::CREATED, Json(json!({"id": id}))))
}

#[utoipa::path(get, path = "/api/helpdesk/assets/{id}", tag = "helpdesk", params(("id" = Uuid, Path)), responses((status = 200, body = Asset)))]
async fn get_asset(State(core): State<Core>, user: CurrentUser, Path(id): Path<Uuid>) -> ApiResult<Json<Asset>> {
    user.require(&perm::WORK)?;
    fetch_asset(&core, id).await?.map(Json).ok_or(ApiError::NotFound)
}

#[utoipa::path(put, path = "/api/helpdesk/assets/{id}", tag = "helpdesk", params(("id" = Uuid, Path)), request_body = AssetInput, responses((status = 204)))]
async fn update_asset(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<AssetInput>,
) -> ApiResult<StatusCode> {
    user.require(&MANAGE_ASSETS)?;
    let attrs = check_asset(&core, &b).await?;
    let n = sqlx::query(
        "UPDATE assets SET type_id = $2, name = $3, inventory_number = $4, serial_number = $5, site_id = $6, room = $7,
                responsible_person_id = $8, status = $9, notes = $10, attributes = $11, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(b.type_id)
    .bind(b.name.trim())
    .bind(clean(&b.inventory_number))
    .bind(clean(&b.serial_number))
    .bind(b.site_id)
    .bind(clean(&b.room))
    .bind(b.responsible_person_id)
    .bind(&b.status)
    .bind(clean(&b.notes))
    .bind(attrs)
    .execute(&core.db)
    .await
    .map_err(dup)?
    .rows_affected();
    if n == 0 {
        return Err(ApiError::NotFound);
    }
    audit::record(&core.db, &user.actor, Entry::new("asset.update", Source::Portal).object("asset", id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ------------------------------------------------------------------ Связанные заявки и дубли

#[derive(Debug, Clone, Serialize, sqlx::FromRow, ToSchema)]
pub struct TicketLink {
    pub number: i64,
    pub title: String,
    pub status: String,
    /// `related`, `follow_up`, `duplicate`.
    pub kind: String,
}

/// Связи в обе стороны.
pub async fn links_of(db: &sqlx::PgPool, ticket: Uuid) -> Result<Vec<TicketLink>, sqlx::Error> {
    sqlx::query_as(
        "SELECT x.number, x.title, x.status, l.kind FROM ticket_links l JOIN tickets x ON x.id = l.linked_ticket_id WHERE l.ticket_id = $1
         UNION
         SELECT x.number, x.title, x.status, l.kind FROM ticket_links l JOIN tickets x ON x.id = l.ticket_id WHERE l.linked_ticket_id = $1
         ORDER BY 1",
    )
    .bind(ticket)
    .fetch_all(db)
    .await
}

#[derive(Deserialize, ToSchema)]
pub struct TicketLinkInput {
    pub number: i64,
    /// `related` или `follow_up`.
    pub kind: String,
}

async fn other(tx: &mut Transaction<'_, Postgres>, number: i64, me: Uuid) -> ApiResult<(Uuid, i64)> {
    let id: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM tickets WHERE number = $1").bind(number).fetch_optional(&mut **tx).await?;
    match id {
        Some(id) if id != me => Ok((id, number)),
        _ => Err(ApiError::bad_request("invalid_ticket", "Связать можно с другой существующей заявкой.")),
    }
}

/// Связать с другой заявкой.
#[utoipa::path(post, path = "/api/helpdesk/tickets/{number}/links", tag = "helpdesk", params(("number" = i64, Path)), request_body = TicketLinkInput, responses((status = 204)))]
async fn add_link(
    State(core): State<Core>,
    user: CurrentUser,
    Path(number): Path<i64>,
    Json(b): Json<TicketLinkInput>,
) -> ApiResult<StatusCode> {
    let who = Who::from_user(&user);
    need_engineer(&who)?;
    if !matches!(b.kind.as_str(), "related" | "follow_up") {
        return Err(ApiError::bad_request("invalid_ticket", "Связь — related или follow_up."));
    }
    let mut tx = core.db.begin().await?;
    let t = lock(&mut tx, number, &who).await?;
    let (oid, onum) = other(&mut tx, b.number, t.id).await?;
    sqlx::query("INSERT INTO ticket_links (ticket_id, linked_ticket_id, kind, created_by) VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING")
        .bind(t.id)
        .bind(oid)
        .bind(&b.kind)
        .bind(who.user)
        .execute(&mut *tx)
        .await?;
    event(&mut tx, t.id, who.user, "ticket_linked", Value::Null, json!({"number": onum, "kind": b.kind})).await?;
    event(&mut tx, oid, who.user, "ticket_linked", Value::Null, json!({"number": t.number, "kind": b.kind})).await?;
    audit_rec(&mut tx, &who, "ticket.links.add", &t, json!({"other": onum, "kind": b.kind})).await?;
    ping(&mut tx, &t).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(delete, path = "/api/helpdesk/tickets/{number}/links/{other}", tag = "helpdesk",
    params(("number" = i64, Path), ("other" = i64, Path)), responses((status = 204)))]
async fn remove_link(
    State(core): State<Core>,
    user: CurrentUser,
    Path((number, other_number)): Path<(i64, i64)>,
) -> ApiResult<StatusCode> {
    let who = Who::from_user(&user);
    need_engineer(&who)?;
    let mut tx = core.db.begin().await?;
    let t = lock(&mut tx, number, &who).await?;
    let (oid, _) = other(&mut tx, other_number, t.id).await?;
    let n = sqlx::query(
        "DELETE FROM ticket_links WHERE (ticket_id = $1 AND linked_ticket_id = $2) OR (ticket_id = $2 AND linked_ticket_id = $1)",
    )
    .bind(t.id)
    .bind(oid)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if n == 0 {
        return Err(ApiError::NotFound);
    }
    event(&mut tx, t.id, who.user, "ticket_unlinked", json!({"number": other_number}), Value::Null).await?;
    audit_rec(&mut tx, &who, "ticket.links.remove", &t, json!({"other": other_number})).await?;
    ping(&mut tx, &t).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, ToSchema)]
pub struct MergeInput {
    /// Основная заявка, в которую объединяем.
    pub into: i64,
}

/// Объединить дубль с основной: описание и публичные ответы — заметкой в основную, заявитель дубля —
/// наблюдателем основной, дубль закрывается с кодом «Дубль».
#[utoipa::path(post, path = "/api/helpdesk/tickets/{number}/merge", tag = "helpdesk", params(("number" = i64, Path)), request_body = MergeInput, responses((status = 204)))]
async fn merge_ticket(
    State(core): State<Core>,
    user: CurrentUser,
    Path(number): Path<i64>,
    Json(b): Json<MergeInput>,
) -> ApiResult<StatusCode> {
    let who = Who::from_user(&user);
    merge(&core, &who, number, b.into).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn merge(core: &Core, who: &Who, number: i64, into: i64) -> ApiResult<()> {
    need_engineer(who)?;
    if number == into {
        return Err(ApiError::bad_request("invalid_ticket", "Заявку нельзя объединить саму с собой."));
    }
    let c = crate::notify::ctx(core).await?;
    let mut tx = core.db.begin().await?;
    // Блокируем в порядке номеров — два встречных объединения не застрянут.
    let (first, second) = if number < into { (number, into) } else { (into, number) };
    let a = lock(&mut tx, first, who).await?;
    let b = lock(&mut tx, second, who).await?;
    let (mut dup_t, main) = if a.number == number { (a, b) } else { (b, a) };
    if !tickets::is_open(&dup_t.status) {
        return Err(bad_state("Дубль уже решён или закрыт."));
    }
    if matches!(main.status.as_str(), "closed" | "canceled") {
        return Err(bad_state("Основная заявка закрыта."));
    }
    let (description, comments): (String, Vec<String>) = (
        sqlx::query_scalar("SELECT description FROM tickets WHERE id = $1").bind(dup_t.id).fetch_one(&mut *tx).await?,
        sqlx::query_scalar(
            "SELECT body FROM ticket_comments WHERE ticket_id = $1 AND visibility = 'public' ORDER BY created_at",
        )
        .bind(dup_t.id)
        .fetch_all(&mut *tx)
        .await?,
    );
    let mut note = tr(&format!("Объединена заявка #{} «{}».", dup_t.number, dup_t.title)).into_owned();
    if !description.trim().is_empty() {
        note.push_str(&format!("\n\n{description}"));
    }
    for cm in comments {
        note.push_str(&format!("\n\n— {cm}"));
    }
    insert_comment(&mut tx, main.id, who, &NewComment { body: note, internal: true, ..Default::default() }).await?;
    if let Some(p) = dup_t.requester_id.filter(|p| Some(*p) != main.requester_id) {
        sqlx::query(
            "INSERT INTO ticket_watchers (ticket_id, person_id, added_by) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
        )
        .bind(main.id)
        .bind(p)
        .bind(who.user)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query("UPDATE ticket_objects o SET ticket_id = $2 WHERE ticket_id = $1
                  AND NOT EXISTS (SELECT 1 FROM ticket_objects x WHERE x.ticket_id = $2 AND x.kind = o.kind AND x.object_id = o.object_id)")
        .bind(dup_t.id)
        .bind(main.id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO ticket_links (ticket_id, linked_ticket_id, kind, created_by) VALUES ($1, $2, 'duplicate', $3) ON CONFLICT DO NOTHING")
        .bind(dup_t.id)
        .bind(main.id)
        .bind(who.user)
        .execute(&mut *tx)
        .await?;
    let public = tr(&format!(
        "Заявка объединена с #{} — дальше работа идёт там, вы будете получать уведомления по ней.",
        main.number
    ))
    .into_owned();
    insert_comment(&mut tx, dup_t.id, who, &NewComment { body: public, ..Default::default() }).await?;
    sqlx::query("UPDATE tickets SET closure_code = 'duplicate', resolved_at = coalesce(resolved_at, now()), closed_at = now(), closed_by = $2 WHERE id = $1")
        .bind(dup_t.id)
        .bind(who.user)
        .execute(&mut *tx)
        .await?;
    set_status(&mut tx, &mut dup_t, who, "closed").await?;
    event(&mut tx, dup_t.id, who.user, "merged", Value::Null, json!(main.number)).await?;
    event(&mut tx, main.id, who.user, "merged_from", Value::Null, json!(dup_t.number)).await?;
    audit_rec(&mut tx, who, "ticket.merge", &dup_t, json!({"into": main.number})).await?;
    crate::notify::to_requester(&mut tx, &dup_t, &crate::notify::Msg::Closed, &c).await?;
    ping(&mut tx, &dup_t).await?;
    ping(&mut tx, &main).await?;
    tx.commit().await?;
    Ok(())
}
